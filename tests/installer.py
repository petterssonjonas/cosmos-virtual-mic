#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Exercise the Bash installer with native ELF fixtures and a private manager.

Python is development test tooling, not an installation/runtime dependency.
The real installer, checksum checks, file operations, and old client run here.
Only systemctl is replaced; no command reaches the desktop service manager.
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
    if (argc > 1 && !strcmp(argv[1], "--help")) return 0;
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
assert sys.argv[1:4] == ['--user', '--no-pager', '--no-ask-password'], sys.argv
args = sys.argv[4:]
action = args[0]
assert action in {'show-environment', 'show', 'is-active', 'stop', 'disable',
                  'enable', 'daemon-reload', 'start'}, args
p = Path(os.environ['MANAGER_STATE'])
state = json.loads(p.read_text())
with open(os.environ['FIXTURE_LOG'], 'a') as log:
    log.write('manager:' + action + '\n')
unit = Path(os.environ['XDG_CONFIG_HOME']) / 'systemd/user/cosmos-audio-mixer.service'
code = 0
if action == 'show':
    if '--property=MainPID' in args:
        print(os.environ.get('MAINPID_VALUE', '4242' if state['running'] else '0'))
    else:
        print(os.environ.get('FOREIGN_UNIT', str(unit) if unit.exists() else ''))
elif action == 'is-active':
    print('active' if state['running'] else 'inactive')
    code = 0 if state['running'] else 3
elif action == 'stop':
    state['running'] = False
elif action in {'enable', 'disable'}:
    state['runtime' if '--runtime' in args else 'persistent'] = action == 'enable'
elif action == 'start':
    if os.environ.get('FAIL_START'):
        code = 4
    else:
        state['running'] = True
        socket = Path(os.environ['FIXTURE_SOCKET'])
        socket.parent.mkdir(parents=True, exist_ok=True)
        socket.touch()
p.write_text(json.dumps(state))
sys.exit(code)
'''


class Fixture:
    def __init__(self, root):
        self.root = root
        self.home = root / "home=a'b%$\"\\`"
        self.config = root / "config a'b%$\"\\`"
        self.data = root / "data a'b%$\"\\`"
        self.bundle = root / 'bundle'
        self.bin = root / 'tools'
        for path in (self.home, self.config, self.data, self.bundle, self.bin, root / 'run'):
            path.mkdir()
        self.log = root / 'events'
        self.state = root / 'manager.json'
        self.state.write_text(json.dumps({'persistent': False, 'runtime': False, 'running': False}))
        self.socket = root / 'run' / APP / 'control.sock'
        self.env = {'PATH': str(self.bin) + ':/usr/bin:/bin', 'LC_ALL': 'C',
                    'HOME': str(self.home), 'XDG_CONFIG_HOME': str(self.config),
                    'XDG_DATA_HOME': str(self.data), 'XDG_RUNTIME_DIR': str(root / 'run'),
                    'MANAGER_STATE': str(self.state), 'FIXTURE_LOG': str(self.log),
                    'FIXTURE_SOCKET': str(self.socket), 'GUI_OUTPUT': str(root / 'gui-output'),
                    'DBUS_SESSION_BUS_ADDRESS': 'unix:path=' + str(root / 'no-session-bus')}
        (self.bin / 'systemctl').write_text(MANAGER)
        (self.bin / 'systemctl').chmod(0o755)
        (self.bin / 'install').write_text('''#!/usr/bin/env bash
if [[ ${FAIL_COPY:-} && ${!#} == "$HOME/.local/bin/cosmos-engine."* ]]; then exit 5; fi
exec /usr/bin/install "$@"
''')
        (self.bin / 'install').chmod(0o755)
        shutil.copy2(ROOT / 'install.sh', self.bundle / 'install.sh')
        shutil.copytree(ROOT / 'data', self.bundle / 'data')
        (self.bundle / 'data/compatibility.conf').write_text('ARCH=' + platform.machine() + '\nGLIBC_MIN=2.39\n')
        source = root / 'fixture.c'
        source.write_text(ENGINE)
        self.executables = {}
        for version in ('old', 'new'):
            executable = root / ('fixture-' + version)
            subprocess.run(['cc', '-DVERSION="' + version + '"', str(source), '-o', str(executable)], check=True)
            self.executables[version] = executable
        self.version('old')
        self.profile = self.config / APP / 'profile.json'
        self.profile.parent.mkdir()
        self.profile.write_text('preserve user settings')

    @property
    def install(self):
        return self.home / '.local/bin'

    @property
    def legacy(self):
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

    def run(self, *args, ok=True, extra=None, installed=False):
        script = self.data / APP / 'install.sh' if installed else self.bundle / 'install.sh'
        result = subprocess.run(['sh', str(script), *args], env=self.env | (extra or {}),
                                capture_output=True, text=True, timeout=40)
        assert (result.returncode == 0) == ok, (args, result.returncode, result.stdout, result.stderr)
        return result

    def status(self):
        return json.loads(self.state.read_text())

    def same_pair(self, version):
        for name in ('cosmos-engine', APP):
            assert (self.install / name).read_bytes() == self.executables[version].read_bytes()

    def unchanged(self, version, state):
        self.same_pair(version)
        assert self.status() == state
        assert self.profile.read_text() == 'preserve user settings'


def main():
    assert os.getuid() != 0, 'Run as a normal user; root rejection is a real check.'
    with tempfile.TemporaryDirectory(prefix='cosmos-bash-installer-', dir='/var/tmp') as temporary:
        f = Fixture(Path(temporary))
        f.run('install', '--enable', extra={'SUDO_USER': 'root'}, ok=False)
        assert not f.unit.exists()
        f.run('install')
        f.same_pair('old')
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        verified = subprocess.run(['systemd-analyze', '--user', 'verify', '--man=no', str(f.unit)],
                                  env=f.env | {'SYSTEMD_UNIT_PATH': str(f.unit.parent) + ':/usr/lib/systemd/user',
                                               'SYSTEMD_USER_GENERATOR_PATH': '/dev/null'},
                                  capture_output=True, text=True)
        assert verified.returncode == 0, (verified.stdout, verified.stderr)
        subprocess.run(['gio', 'launch', str(f.desktop)], check=True, capture_output=True,
                       env=f.env | {'XDG_CONFIG_HOME': '/wrong', 'XDG_DATA_HOME': '/wrong'})
        output = f.root / 'gui-output'
        for _ in range(100):
            if output.exists():
                break
            time.sleep(.02)
        assert output.read_text().splitlines() == [str(f.config), str(f.data)]
        f.run('install', extra={'XDG_CONFIG_HOME': str(f.config) + '/'})
        f.run('install', '--enable')
        active = {'persistent': True, 'runtime': False, 'running': True}
        f.version('new')
        overrides = f.unit.parent / (UNIT + '.d')
        overrides.mkdir()
        override = overrides / 'logging.conf'
        original_override = '[Service]\nExecStart=\nExecStart=' + str(f.legacy / 'current/cosmos-engine') + ' run --log-level info\n'
        override.write_text(original_override)
        f.run('install', ok=False)
        f.unchanged('old', active)
        assert override.read_text() == original_override
        override.unlink()
        for failure in ({'FAIL_OLD_STOP': '1'}, {'FOREIGN_UNIT': '/usr/lib/systemd/user/foreign.service'}):
            f.run('install', extra=failure, ok=False)
            f.unchanged('old', active)
        f.socket.unlink()
        for pid in ('4242', '', 'invalid'):
            f.run('install', extra={'MAINPID_VALUE': pid}, ok=False)
            f.unchanged('old', active)
        f.socket.touch()
        (f.bundle / APP).write_bytes(b'corrupted')
        f.run('install', extra={'temporary': str(f.profile)}, ok=False)
        f.unchanged('old', active)
        f.checksums()
        f.run('install', ok=False)  # Correct hash does not make an invalid executable usable.
        f.unchanged('old', active)
        f.version('new')
        compatibility = f.bundle / 'data/compatibility.conf'
        original = compatibility.read_text()
        compatibility.write_text('ARCH=wrong\nGLIBC_MIN=2.39\n')
        f.checksums()
        f.run('install', ok=False)
        f.unchanged('old', active)
        compatibility.write_text(original)
        f.checksums()
        manifest = f.bundle / 'SHA256SUMS'
        original_manifest = manifest.read_text()
        manifest.write_text(''.join(line for line in original_manifest.splitlines(True) if not line.endswith('  install.sh\n')))
        f.run('install', ok=False)
        f.unchanged('old', active)
        manifest.write_text(original_manifest)
        f.run('install', extra={'FAIL_COPY': '1'}, ok=False)
        assert not f.status()['running']
        backups = list((f.home / '.local').glob('.cosmos-install.*/previous'))
        assert any((p / 'cosmos-engine').read_bytes() == f.executables['old'].read_bytes() for p in backups)
        assert f.profile.read_text() == 'preserve user settings'
        f.run('install', '--enable')
        f.same_pair('new')
        assert f.status() == active
        f.state.write_text(json.dumps({'persistent': False, 'runtime': True, 'running': True}))
        f.run('install')
        assert f.status() == {'persistent': False, 'runtime': True, 'running': True}
        f.run('install', extra={'FAIL_START': '1'}, ok=False)
        f.same_pair('new')
        assert f.profile.read_text() == 'preserve user settings'
        f.run('install', '--enable')
        note = f.data / APP / 'keep.txt'
        note.write_text('unrelated data')
        f.run('uninstall', extra={'FAIL_OLD_STOP': '1'}, ok=False, installed=True)
        f.same_pair('new')
        f.run('uninstall', installed=True)
        assert not f.unit.exists() and not f.desktop.exists()
        assert not (f.install / APP).exists() and not (f.install / 'cosmos-engine').exists()
        assert f.profile.read_text() == 'preserve user settings'
        assert note.read_text() == 'unrelated data'
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        f.run('uninstall')
    for versioned in (False, True):
        with tempfile.TemporaryDirectory(prefix='cosmos-legacy-migration-', dir='/var/tmp') as temporary:
            f = Fixture(Path(temporary))
            old = f.legacy / 'releases/old' if versioned else f.legacy
            old.mkdir(parents=True)
            for name in (APP, 'cosmos-engine'):
                shutil.copy2(f.executables['old'], old / name)
            (f.legacy / 'keep.txt').write_text('unrelated legacy data')
            if versioned:
                (f.legacy / 'current').symlink_to('releases/old')
                (f.legacy / '.cosmos-install.json').write_text('{"previous": "installer"}')
                f.unit.parent.mkdir(parents=True)
                f.unit.write_text('[Service]\nExecStart=' + str(old / 'cosmos-engine') + ' run\n')
                f.state.write_text(json.dumps({'persistent': True, 'runtime': False, 'running': True}))
            f.socket.parent.mkdir(parents=True)
            f.socket.touch()
            f.version('new')
            f.run('install', '--enable')
            f.same_pair('new')
            assert not f.legacy.exists()
            backup, = f.legacy.parent.glob(APP + '.backup.*')
            assert (backup / 'files/keep.txt').read_text() == 'unrelated legacy data'
            events = f.log.read_text().splitlines()
            assert events.index('engine-old:stop') < events.index('manager:start')
            assert 'engine-new:stop' not in events
            assert f.profile.read_text() == 'preserve user settings'
            f.run('uninstall', installed=True)
    print('PASS: Bash install/update/remove, native launcher quoting, legacy migration, preflight and save failures, retained backups/settings')


if __name__ == '__main__':
    main()
