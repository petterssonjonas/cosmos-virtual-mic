#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise the installer with real ELF fixtures and a private manager boundary.

No systemctl call reaches the session manager. A fake systemctl records service
state and failure injection; the actual installer, checksum/runtime checks,
filesystem transaction, and old executable stop command run unchanged.
Requires cc, ldd, getconf, and gio (for actual Desktop Exec parsing).
Run as a normal user: python3 tests/installer.py
"""
import hashlib
import json
import os
from pathlib import Path
import platform
import shutil
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
APP = 'cosmos-audio-mixer'
APP_ID = 'io.github.petterssonjonas.CosmosAudioMixer'
UNIT = APP + '.service'

ENGINE = r'''
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
int main(int argc, char **argv) {
    FILE *log = fopen(getenv("FIXTURE_LOG"), "a");
    if (!log) return 90;
    fprintf(log, "engine-%s:%s\n", VERSION, argc > 1 ? argv[1] : "gui");
    fclose(log);
    if (argc > 1 && !strcmp(argv[1], "stop")) {
        if (getenv("FAIL_OLD_STOP")) return 7;
        return unlink(getenv("FIXTURE_SOCKET")) ? 8 : 0;
    }
    FILE *out = fopen(getenv("GUI_OUTPUT"), "wb");
    if (!out) return 91;
    fprintf(out, "%s\n%s\n", getenv("XDG_CONFIG_HOME"), getenv("XDG_DATA_HOME"));
    fclose(out);
    return 0;
}
'''

MANAGER = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import sys
assert sys.argv[1] == '--user', sys.argv
args = sys.argv[2:]
action = args[0]
assert action in {'show-environment', 'show', 'is-enabled', 'is-active', 'stop',
                  'disable', 'enable', 'daemon-reload', 'start'}, args
if action not in {'show-environment', 'daemon-reload'}:
    assert 'cosmos-audio-mixer.service' in args, args
p = Path(os.environ['MANAGER_STATE'])
state = json.loads(p.read_text())
with open(os.environ['FIXTURE_LOG'], 'a') as log:
    log.write('manager:' + action + '\n')
unit = Path(os.environ['XDG_CONFIG_HOME']) / 'systemd/user/cosmos-audio-mixer.service'
pending = Path(os.environ['RESTART_PENDING'])
code = 0
if action == 'show':
    if '--property=MainPID' in args:
        print(os.environ.get('MAINPID_VALUE', '0' if pending.exists() or not state['running'] else '4242'))
    else:
        print(str(unit) if unit.exists() and not os.environ.get('HIDE_UNIT') else '')
elif action == 'is-enabled':
    print('enabled' if state['persistent'] else 'enabled-runtime' if state['runtime'] else 'disabled')
    code = 0 if state['persistent'] or state['runtime'] else 1
elif action == 'is-active':
    if pending.exists():
        print('activating')
        code = 3
    else:
        print('active' if state['running'] else 'inactive')
        code = 0 if state['running'] else 3
elif action == 'stop':
    if os.environ.get('FAIL_MANAGER_STOP'):
        code = 4
    else:
        state['running'] = False
        pending.unlink(missing_ok=True)
elif action in {'enable', 'disable'}:
    if action == 'disable' and not unit.exists():
        print('Unit file does not exist.', file=sys.stderr)
        sys.exit(1)
    kind = 'runtime' if '--runtime' in args else 'persistent'
    state[kind] = action == 'enable'
    base = Path(os.environ['XDG_RUNTIME_DIR']) if kind == 'runtime' else Path(os.environ['XDG_CONFIG_HOME'])
    link = base / 'systemd/user/default.target.wants/cosmos-audio-mixer.service'
    if action == 'enable':
        link.parent.mkdir(parents=True, exist_ok=True)
        if not link.is_symlink():
            link.symlink_to(unit)
    else:
        link.unlink(missing_ok=True)
elif action == 'daemon-reload':
    failure = Path(os.environ['FAIL_RELOAD_ONCE'])
    if failure.exists():
        failure.unlink()
        code = 4
elif action == 'start':
    failure = Path(os.environ['FAIL_START_ONCE'])
    if failure.exists():
        failure.unlink()
        if os.environ.get('SIMULATE_RESTART'):
            pending.touch()
        code = 4
    else:
        state['running'] = True
        Path(os.environ['FIXTURE_SOCKET']).parent.mkdir(parents=True, exist_ok=True)
        Path(os.environ['FIXTURE_SOCKET']).touch()
p.write_text(json.dumps(state))
sys.exit(code)
'''

INJECTOR = r'''
# Deterministic syscall/copy boundary failures. The real installer, its verified
# hashes and rollback run unchanged; no production bypass flag is added.
import os
from pathlib import Path
import shutil
real_mkdir = Path.mkdir
def mkdir(path, *args, **kwargs):
    if str(path) == os.environ.get('INJECT_MKDIR') or str(path.parent) == os.environ.get('INJECT_STAGE_PARENT'):
        raise OSError('injected directory creation failure')
    return real_mkdir(path, *args, **kwargs)
Path.mkdir = mkdir
real_copy = shutil.copy2
def copy(source, target, *args, **kwargs):
    if str(source) == os.environ.get('INJECT_COPY_SOURCE'):
        real_copy(os.environ['INJECT_REPLACEMENT'], source)
    return real_copy(source, target, *args, **kwargs)
shutil.copy2 = copy
'''


class Fixture:
    def __init__(self, root):
        self.root = root
        # Cover every quoting layer: apostrophe, space, percent, dollar, quote,
        # backslash and backtick. A literal percent is not a systemd/desktop code.
        self.home = root / "home a'b%$\"\\`"
        self.config = root / "config a'b%$\"\\`"
        self.data = root / "data a'b%$\"\\`"
        self.bundle = root / 'bundle'
        self.bin = root / 'tools'
        for path in (self.home, self.config, self.data, self.bundle, self.bin, root / 'run'):
            path.mkdir()
        self.log = root / 'events'
        self.log.touch()
        self.state = root / 'manager.json'
        self.state.write_text(json.dumps({'persistent': False, 'runtime': False, 'running': False}))
        self.socket = root / 'run' / APP / 'control.sock'
        self.env = {'PATH': str(self.bin) + ':/usr/bin:/bin', 'LC_ALL': 'C',
                    'HOME': str(self.home), 'XDG_CONFIG_HOME': str(self.config),
                    'XDG_DATA_HOME': str(self.data), 'XDG_RUNTIME_DIR': str(root / 'run'),
                    'MANAGER_STATE': str(self.state), 'FIXTURE_LOG': str(self.log),
                    'FIXTURE_SOCKET': str(self.socket), 'GUI_OUTPUT': str(root / 'gui-output'),
                    'FAIL_START_ONCE': str(root / 'fail-start'),
                    'FAIL_RELOAD_ONCE': str(root / 'fail-reload'),
                    'RESTART_PENDING': str(root / 'restart-pending'),
                    'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(root / 'no-session-bus')}
        (self.bin / 'systemctl').write_text(MANAGER)
        (self.bin / 'systemctl').chmod(0o755)
        shutil.copy2(ROOT / 'install.sh', self.bundle / 'install.sh')
        (self.bundle / 'data').mkdir()
        for name in (UNIT, APP_ID + '.desktop'):
            shutil.copy2(ROOT / 'data' / name, self.bundle / 'data' / name)
        (self.bundle / 'data' / (APP_ID + '.svg')).write_text('<svg xmlns="http://www.w3.org/2000/svg"/>')
        (self.bundle / 'data/compatibility.conf').write_text('ARCH=' + platform.machine() + '\nGLIBC_MIN=2.17\n')
        source = root / 'fixture.c'
        source.write_text(ENGINE)
        self.executables = {}
        for version in ('old', 'new'):
            executable = root / ('fixture-' + version)
            subprocess.run(['cc', '-DVERSION="' + version + '"', str(source), '-o', str(executable)], check=True)
            self.executables[version] = executable
        self.version('old')
        self.hooks = root / 'hooks'
        self.hooks.mkdir()
        (self.hooks / 'sitecustomize.py').write_text(INJECTOR)

    @property
    def install(self):
        return self.home / '.local/opt' / APP

    @property
    def unit(self):
        return self.config / 'systemd/user' / UNIT

    @property
    def desktop(self):
        return self.data / 'applications' / (APP_ID + '.desktop')

    def version(self, version):
        for name in ('cosmos-engine', APP):
            shutil.copy2(self.executables[version], self.bundle / name)
        self.checksums()

    def checksums(self):
        paths = sorted(p for p in self.bundle.rglob('*') if p.is_file() and p.name != 'SHA256SUMS')
        (self.bundle / 'SHA256SUMS').write_text(''.join(
            hashlib.sha256(p.read_bytes()).hexdigest() + '  ' + p.relative_to(self.bundle).as_posix() + '\n'
            for p in paths))

    def run(self, *args, ok=True, extra=None):
        result = subprocess.run([str(self.bundle / 'install.sh'), *args],
                                env=self.env | (extra or {}), capture_output=True, text=True, timeout=40)
        assert (result.returncode == 0) == ok, (args, result.returncode, result.stdout, result.stderr)
        return result

    def status(self):
        return json.loads(self.state.read_text())

    def manager(self, *args):
        subprocess.run([str(self.bin / 'systemctl'), '--user', *args, UNIT], env=self.env, check=True,
                       capture_output=True, text=True, timeout=10)

    def same_pair(self, version):
        for name in ('cosmos-engine', APP):
            assert (self.install / 'current' / name).read_bytes() == self.executables[version].read_bytes()

    def events(self):
        lines = self.log.read_text().splitlines()
        self.log.write_text('')
        return lines


def main():
    assert os.getuid() != 0, 'Run installer tests as a normal user; root rejection is a real check.'
    for tool in ('cc', 'ldd', 'getconf', 'gio'):
        assert shutil.which(tool), 'Missing installer test prerequisite: ' + tool
    with tempfile.TemporaryDirectory(prefix='cosmos-installer-', dir='/var/tmp') as temporary:
        f = Fixture(Path(temporary))
        f.run('--help')
        f.run('install', ok=False, extra={'SUDO_USER': 'someone'})
        f.run('install', ok=False, extra={'XDG_CONFIG_HOME': 'relative'})
        assert not f.install.exists()
        f.unit.parent.mkdir(parents=True)
        f.unit.write_text('unrelated user unit')
        f.run('install', ok=False)
        assert f.unit.read_text() == 'unrelated user unit'
        f.unit.unlink()
        outside = f.root / 'unrelated'
        outside.mkdir()
        f.install.parent.mkdir(parents=True, exist_ok=True)
        f.install.symlink_to(outside, target_is_directory=True)
        f.run('install', ok=False)
        assert outside.is_dir()
        f.install.unlink()
        (f.bundle / 'cosmos-engine').write_bytes(b'corrupt')
        f.run('install', ok=False)
        assert not f.install.exists()
        f.version('old')
        f.run('install', ok=False, extra={'PYTHONPATH': str(f.hooks),
                                         'INJECT_MKDIR': str(f.install / 'releases')})
        assert not f.install.exists(), 'Partial first-install directories must roll back.'
        f.run('install', ok=False, extra={'PYTHONPATH': str(f.hooks),
                                         'INJECT_STAGE_PARENT': str(f.install / 'releases')})
        assert not f.install.exists(), 'Stage creation failure must also remove the new releases directory.'
        f.events()
        f.run('install', ok=False, extra={'PYTHONPATH': str(f.hooks),
                                         'INJECT_COPY_SOURCE': str(f.bundle / 'cosmos-engine'),
                                         'INJECT_REPLACEMENT': str(f.executables['new'])})
        assert not f.install.exists()
        assert 'manager:stop' not in f.events(), 'Changed staged bytes must fail before engine stop.'
        f.version('old')
        compatibility = f.bundle / 'data/compatibility.conf'
        compatibility.write_text('ARCH=' + platform.machine() + '\nGLIBC_MIN=2.999\n')
        f.checksums()
        f.run('install', ok=False)
        assert not f.install.exists()
        compatibility.write_text('ARCH=' + platform.machine() + '\nGLIBC_MIN=2.17\n')
        f.checksums()
        # Real systemctl rejects disable of a missing unit. Both a fresh reload
        # failure and a failed start with a queued restart must fully roll back.
        Path(f.env['FAIL_RELOAD_ONCE']).touch()
        result = f.run('install', '--enable', ok=False)
        assert 'Rollback also failed' not in result.stderr
        assert not f.install.exists() and not f.unit.exists() and not f.desktop.exists()
        Path(f.env['FAIL_START_ONCE']).touch()
        result = f.run('install', '--enable', ok=False, extra={'SIMULATE_RESTART': '1'})
        assert 'Cannot safely roll back' not in result.stderr and 'Rollback also failed' not in result.stderr
        assert not f.install.exists() and not f.unit.exists() and not f.desktop.exists()
        assert not Path(f.env['RESTART_PENDING']).exists()
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        f.run('install')
        f.same_pair('old')
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        assert f.desktop.is_file() and f.unit.is_file()
        # GLib's actual desktop parser must launch the installed fixture with the
        # exact custom paths, not an independent approximation of Exec grammar.
        subprocess.run(['gio', 'launch', str(f.desktop)], env=f.env, capture_output=True, check=True, timeout=10)
        deadline = time.monotonic() + 5
        output = Path(f.env['GUI_OUTPUT'])
        while not output.exists() and time.monotonic() < deadline:
            time.sleep(0.02)
        assert output.read_text().splitlines() == [str(f.config), str(f.data)]
        # A disabled install stays disabled even when an old standalone engine
        # must be stopped. Failed opt-in must restore the disabled/stopped state.
        f.socket.parent.mkdir(parents=True, exist_ok=True)
        f.socket.touch()
        f.run('install')
        assert not f.socket.exists()
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        Path(f.env['FAIL_START_ONCE']).touch()
        f.run('install', '--enable', ok=False)
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        f.same_pair('old')
        f.manager('enable', '--runtime')
        f.manager('start')
        Path(f.env['FAIL_START_ONCE']).touch()
        f.run('install', '--enable', ok=False)
        assert f.status() == {'persistent': False, 'runtime': True, 'running': True}
        assert not (f.config / 'systemd/user/default.target.wants' / UNIT).exists()
        assert (f.root / 'run/systemd/user/default.target.wants' / UNIT).is_symlink()
        # A live or unparseable MainPID without a control socket must never be
        # forced down. A zero-PID restart timer was exercised above.
        for pid in ('4242', 'unparseable'):
            old_link = os.readlink(f.install / 'current')
            f.socket.unlink()
            f.events()
            result = f.run('install', ok=False, extra={'MAINPID_VALUE': pid})
            assert 'refusing an unconfirmed stop' in result.stderr
            assert 'manager:stop' not in f.events()
            assert os.readlink(f.install / 'current') == old_link
            f.same_pair('old')
        f.run('install', '--enable')
        assert f.status() == {'persistent': True, 'runtime': True, 'running': True}
        profile = f.config / APP / 'profile.json'
        profile.parent.mkdir()
        profile.write_text('preserve user settings')
        old_link = os.readlink(f.install / 'current')
        f.version('new')
        f.events()
        f.run('install', ok=False, extra={'FAIL_OLD_STOP': '1'})
        assert os.readlink(f.install / 'current') == old_link
        f.same_pair('old')
        assert 'manager:stop' not in f.events(), 'Failed flush must not force systemd stop.'
        f.run('install', ok=False, extra={'FAIL_MANAGER_STOP': '1'})
        assert os.readlink(f.install / 'current') == old_link
        f.same_pair('old')
        Path(f.env['FAIL_START_ONCE']).touch()
        f.run('install', ok=False)
        assert os.readlink(f.install / 'current') == old_link
        f.same_pair('old')
        assert f.status() == {'persistent': True, 'runtime': True, 'running': True}
        f.events()
        f.run('install')
        f.same_pair('new')
        assert f.status() == {'persistent': True, 'runtime': True, 'running': True}
        events = f.events()
        assert events.index('engine-old:stop') < events.index('manager:stop')
        assert 'engine-new:stop' not in events
        before = os.readlink(f.install / 'current')
        f.unit.write_text(f.unit.read_text() + '# user modification\n')
        f.run('install', ok=False)
        assert os.readlink(f.install / 'current') == before
        f.unit.write_text(f.unit.read_text().removesuffix('# user modification\n'))
        untracked = f.install / 'do-not-delete'
        untracked.write_text('unrelated')
        f.run('uninstall', ok=False)
        assert untracked.read_text() == 'unrelated'
        untracked.unlink()
        old_unit = f.unit.read_bytes()
        old_desktop = f.desktop.read_bytes()
        Path(f.env['FAIL_RELOAD_ONCE']).touch()
        f.run('uninstall', ok=False)
        assert os.readlink(f.install / 'current') == before
        assert f.unit.read_bytes() == old_unit and f.desktop.read_bytes() == old_desktop
        assert f.status() == {'persistent': True, 'runtime': True, 'running': True}
        assert profile.read_text() == 'preserve user settings'
        f.run('uninstall')
        assert not f.install.exists() and not f.unit.exists() and not f.desktop.exists()
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        assert profile.read_text() == 'preserve user settings'
        assert outside.is_dir()
        f.run('uninstall')
    print('PASS: installer state, checksums/runtime, path quoting, old-client stop, rollback, ownership, settings retention')


if __name__ == '__main__':
    main()
