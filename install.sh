#!/bin/sh
# SPDX-License-Identifier: MPL-2.0
# Python's standard library keeps the filesystem transaction and path quoting explicit.
command -v python3 >/dev/null 2>&1 || { echo 'Cosmos installer requires Python 3.' >&2; exit 1; }
exec python3 - "$0" "$@" <<'PY'
import fcntl
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import stat
import struct
import subprocess
import sys
import tempfile
import time
import uuid
from types import MappingProxyType

if sys.version_info < (3, 9):
    sys.exit('Cosmos installer requires Python 3.9 or newer.')

APP = 'cosmos-audio-mixer'
APP_ID = 'io.github.petterssonjonas.CosmosAudioMixer'
UNIT = APP + '.service'
MARKER = '.cosmos-install.json'
HELP = '''Usage: ./install.sh install [--enable] | uninstall | --help

Install or update this verified bundle for the current user. --enable opts in
 to engine startup at user login and starts it now. Updates otherwise preserve
 the current service enable/running state. Uninstall stops the engine and removes
 only installer-owned files; mixer profiles and all other settings remain.

Requires Linux x86-64/aarch64, Python 3.9+, getconf, ldd, and a systemd user manager.
Run as your normal desktop user, never with sudo. No packages are installed.
Install location: ~/.local/opt/cosmos-audio-mixer/current
Custom absolute XDG_CONFIG_HOME and XDG_DATA_HOME values are retained. The user
manager must search XDG_CONFIG_HOME/systemd/user; otherwise installation fails.
SHA256SUMS detects corruption, not an untrusted publisher. Trust the bundle first.
'''


def fail(message):
    raise RuntimeError(message)


def digest(path):
    value = hashlib.sha256()
    with path.open('rb') as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b''):
            value.update(chunk)
    return value.hexdigest()


def clean_path(value):
    if not value or not Path(value).is_absolute() or any(ord(c) < 32 or ord(c) == 127 for c in value):
        fail('Paths must be absolute and contain no control characters: ' + repr(value))
    path = Path(os.path.normpath(value))
    if path == Path('/'):
        fail('A user directory must not be the filesystem root.')
    return path


def safe_path(path):
    # Do not traverse a symlink on a path we will change, including custom XDG paths.
    for part in (path, *path.parents):
        if part.is_symlink():
            fail('Refusing symlink in install path: ' + str(part))
        if part.exists() and part.stat().st_uid not in (0, os.getuid()):
            fail('Install path belongs to another user: ' + str(part))
        if part.exists() and part.stat().st_mode & (stat.S_IWGRP | stat.S_IWOTH) and not part.stat().st_mode & stat.S_ISVTX:
            fail('Install path is writable by a group or other users: ' + str(part))


def atomic(path, content, mode=0o644):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, name = tempfile.mkstemp(prefix='.cosmos-', dir=path.parent)
    try:
        with os.fdopen(fd, 'wb') as handle:
            handle.write(content)
            handle.flush()
            os.fsync(handle.fileno())
            os.fchmod(handle.fileno(), mode)
        os.replace(name, path)
    finally:
        if os.path.lexists(name):
            os.unlink(name)


def point(root, release):
    temporary = root / ('.current-' + uuid.uuid4().hex)
    try:
        temporary.symlink_to('releases/' + release)
        os.replace(temporary, root / 'current')
    finally:
        temporary.unlink(missing_ok=True)


def systemctl(*args, check=True):
    result = subprocess.run(['systemctl', '--user', *args], text=True, capture_output=True,
                            stdin=subprocess.DEVNULL, timeout=25)
    if check and result.returncode:
        fail('systemctl --user ' + ' '.join(args) + ' failed: ' + result.stderr.strip())
    return result


def service_running():
    result = systemctl('is-active', UNIT, check=False)
    return result.returncode == 0 or result.stdout.strip() in ('activating', 'deactivating', 'reloading')


def restore_enablement(state):
    # Remove both kinds first: --enable may have added permanent login startup
    # while the original install was enabled only until the next logout.
    systemctl('disable', UNIT)
    systemctl('disable', '--runtime', UNIT)
    if state['persistent']:
        systemctl('enable', UNIT)
    if state['runtime']:
        systemctl('enable', '--runtime', UNIT)


def quote_unit(value, executable=False):
    value = value.replace('\\', '\\\\').replace('"', '\\"').replace('%', '%%')
    if executable:
        value = value.replace('$', '$$')
    return '"' + value + '"'


def quote_desktop(value):
    # Desktop Exec has both string-value and command-line escaping layers.
    value = value.replace('\\', '\\\\\\\\').replace('"', '\\\\"')
    value = value.replace('`', '\\\\`').replace('$', '\\\\$').replace('%', '%%')
    return '"' + value + '"'


def bundle_files(bundle):
    manifest = bundle / 'SHA256SUMS'
    if not manifest.is_file() or manifest.is_symlink():
        fail('Bundle SHA256SUMS is missing or is a symlink.')
    expected = {}
    manifest_bytes = manifest.read_bytes()
    for line in manifest_bytes.decode('utf-8').splitlines():
        match = re.fullmatch(r'([0-9a-fA-F]{64})  (.+)', line)
        if not match:
            fail('Invalid SHA256SUMS line; use sha256sum with relative file paths.')
        checksum, name = match.groups()
        path = Path(name)
        if path.is_absolute() or '..' in path.parts or name in expected or name == 'SHA256SUMS':
            fail('Unsafe or duplicate SHA256SUMS entry: ' + name)
        expected[name] = checksum.lower()
    actual = set()
    for directory, dirs, files in os.walk(bundle, followlinks=False):
        for name in dirs + files:
            path = Path(directory) / name
            if path.is_symlink():
                fail('Symlinks are not allowed in the bundle: ' + str(path))
        for name in files:
            path = Path(directory) / name
            if not path.is_file():
                fail('Non-regular bundle file: ' + str(path))
            relative = path.relative_to(bundle).as_posix()
            if relative != 'SHA256SUMS':
                actual.add(relative)
    if actual != set(expected):
        fail('SHA256SUMS must cover every bundle file exactly once.')
    for name, checksum in expected.items():
        if digest(bundle / name) != checksum:
            fail('Bundle checksum failed: ' + name)
    required = {'install.sh', 'cosmos-audio-mixer', 'cosmos-engine',
                'data/' + UNIT, 'data/' + APP_ID + '.desktop', 'data/' + APP_ID + '.svg',
                'data/compatibility.conf'}
    if not required <= actual:
        fail('Bundle is incomplete: ' + ', '.join(sorted(required - actual)))
    expected['SHA256SUMS'] = hashlib.sha256(manifest_bytes).hexdigest()
    return MappingProxyType(dict(sorted(expected.items())))


def runtime_check(bundle):
    machines = {'x86_64': 62, 'aarch64': 183}
    machine = platform.machine()
    if not shutil.which('ldd'):
        fail('ldd is required to check the bundle shared-library dependencies.')
    manifest = {}
    for line in (bundle / 'data/compatibility.conf').read_text().splitlines():
        match = re.fullmatch(r'(ARCH|GLIBC_MIN)=([A-Za-z0-9_.]+)', line)
        if not match or match[1] in manifest:
            fail('Invalid or duplicate compatibility.conf field.')
        manifest[match[1]] = match[2]
    if set(manifest) != {'ARCH', 'GLIBC_MIN'} or not re.fullmatch(r'2\.[0-9]+', manifest['GLIBC_MIN']):
        fail('compatibility.conf requires ARCH and GLIBC_MIN=2.xx.')
    if sys.platform != 'linux' or machine not in machines or manifest['ARCH'] != machine:
        fail('Bundle architecture ' + manifest.get('ARCH', '?') + ' does not match supported Linux host ' + machine)
    libc = subprocess.run(['getconf', 'GNU_LIBC_VERSION'], capture_output=True, text=True, timeout=10)
    match = re.fullmatch(r'glibc (\d+)\.(\d+)\s*', libc.stdout)
    if libc.returncode or not match:
        fail('A glibc Linux system is required; getconf GNU_LIBC_VERSION failed.')
    if tuple(map(int, match.groups())) < tuple(map(int, manifest['GLIBC_MIN'].split('.'))):
        fail('Bundle requires glibc >= ' + manifest['GLIBC_MIN'] + '; found ' + libc.stdout.strip())
    for name in ('cosmos-audio-mixer', 'cosmos-engine'):
        path = bundle / name
        with path.open('rb') as handle:
            header = handle.read(20)
        if len(header) < 20 or header[:6] != b'\x7fELF\x02\x01' or struct.unpack('<H', header[18:20])[0] != machines[machine]:
            fail(name + ' is not a Linux 64-bit ' + machine + ' ELF executable.')
        if not os.access(path, os.X_OK):
            fail(name + ' is not executable; preserve bundle permissions when copying.')
        result = subprocess.run(['ldd', str(path)], capture_output=True, text=True, timeout=20,
                                env=os.environ | {'LC_ALL': 'C'})
        if result.returncode or 'not found' in result.stdout or 'not found' in result.stderr:
            fail('Runtime libraries are missing or incompatible for ' + name + ':\n' + result.stdout + result.stderr)


def validate_layout(root, metadata):
    if metadata.get('owner') != APP_ID or metadata.get('uid') != os.getuid() or metadata.get('version') != 1:
        fail('Install directory is not owned by this installer.')
    if set(p.name for p in root.iterdir()) != {MARKER, 'current', 'releases'}:
        fail('Install directory contains unrelated files; leaving it untouched.')
    releases = root / 'releases'
    if releases.is_symlink() or not releases.is_dir():
        fail('Unsafe releases directory.')
    recorded = metadata['releases']
    if metadata['current'] not in recorded:
        fail('Current release is not recorded.')
    if set(p.name for p in releases.iterdir()) != set(recorded):
        fail('Releases directory contains untracked files; leaving it untouched.')
    for release, files in recorded.items():
        if not re.fullmatch(r'[0-9a-f]{32}', release):
            fail('Invalid recorded release name.')
        base = releases / release
        if base.is_symlink() or not base.is_dir():
            fail('Unsafe installed release directory.')
        actual = set()
        actual_dirs = set()
        expected_dirs = {parent.as_posix() for name in files for parent in Path(name).parents if parent != Path('.')}
        for directory, dirs, names in os.walk(base, followlinks=False):
            for name in dirs + names:
                path = Path(directory) / name
                if path.is_symlink() or path.stat().st_uid != os.getuid():
                    fail('Unsafe file in installed release: ' + str(path))
            actual_dirs.update((Path(directory) / name).relative_to(base).as_posix() for name in dirs)
            for name in names:
                path = Path(directory) / name
                if not path.is_file():
                    fail('Non-regular file in installed release.')
                actual.add(path.relative_to(base).as_posix())
        if actual != set(files) or actual_dirs != expected_dirs:
            fail('Installed release contains unrelated or missing files or directories.')
    current = root / 'current'
    if not current.is_symlink() or os.readlink(current) != 'releases/' + metadata['current']:
        fail('Install current link was changed; leaving it untouched.')


def stop_engine(root, metadata, running):
    # Flush through the OLD client before systemd can force a timed-out stop.
    socket = Path(os.environ['XDG_RUNTIME_DIR']) / APP / 'control.sock'
    if os.path.lexists(socket):
        if metadata is None:
            fail('An existing standalone engine is running. Stop it with its original cosmos-engine stop first.')
        old = root / 'current' / 'cosmos-engine'
        result = subprocess.run([str(old), 'stop'], capture_output=True, text=True, timeout=20)
        if result.returncode:
            fail('Old engine could not stop; install is unchanged: ' + result.stderr.strip())
        deadline = time.monotonic() + 10
        while os.path.lexists(socket) and time.monotonic() < deadline:
            time.sleep(0.05)
        if os.path.lexists(socket):
            fail('Old engine did not release its socket; install is unchanged.')
    elif metadata is not None or running:
        # An auto-restart timer can be active even though its crashed process
        # never created a socket. Cancel that timer, but never kill an unflushed
        # live process whose control endpoint is unavailable.
        pid = systemctl('show', UNIT, '--property=MainPID', '--value').stdout.strip()
        if pid != '0':
            fail('Managed engine has no control socket and MainPID is ' + repr(pid)
                 + '; refusing an unconfirmed stop.')
        if os.path.lexists(socket):
            return stop_engine(root, metadata, running)
    if metadata is not None or running:
        systemctl('stop', UNIT)


def main():
    args = sys.argv[2:]
    if args in (['--help'], ['-h']):
        print(HELP)
        return
    if not args or args[0] not in ('install', 'uninstall') or args[1:] not in ([], ['--enable']) or (args[0] == 'uninstall' and len(args) != 1):
        fail(HELP)
    if os.getuid() == 0 or os.geteuid() == 0 or os.environ.get('SUDO_USER') or os.environ.get('SUDO_UID'):
        fail('Run this installer as the desktop user, without sudo or root.')
    if not os.environ.get('XDG_RUNTIME_DIR'):
        fail('XDG_RUNTIME_DIR is missing. Run in the desktop user session.')
    clean_path(os.environ['XDG_RUNTIME_DIR'])
    home = clean_path(os.environ.get('HOME', ''))
    config = clean_path(os.environ.get('XDG_CONFIG_HOME') or str(home / '.config'))
    data = clean_path(os.environ.get('XDG_DATA_HOME') or str(home / '.local/share'))
    root = home / '.local/opt' / APP
    unit = config / 'systemd/user' / UNIT
    desktop = data / 'applications' / (APP_ID + '.desktop')
    for path in (root, unit, desktop):
        safe_path(path)
    root.parent.mkdir(parents=True, exist_ok=True)
    lock_path = root.parent / ('.' + APP + '-install.lock')
    fd = os.open(lock_path, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as lock:
        if os.fstat(lock.fileno()).st_uid != os.getuid():
            fail('Installer lock belongs to another user.')
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            fail('Another Cosmos installer is running.')
        metadata = None
        if root.exists():
            marker = root / MARKER
            if not marker.is_file() or marker.is_symlink():
                fail('Refusing to change an unowned install directory: ' + str(root))
            metadata = json.loads(marker.read_text())
            validate_layout(root, metadata)
            if metadata.get('config') != str(config) or metadata.get('data') != str(data):
                fail('XDG paths differ from the existing install. Use its original XDG_CONFIG_HOME and XDG_DATA_HOME.')
        for path in (unit, desktop):
            if path.exists():
                if metadata is None or digest(path) != metadata['integration'].get(str(path)):
                    fail('Refusing to overwrite a file not owned by this install: ' + str(path))
        if args[0] == 'uninstall' and metadata is None:
            print('Cosmos Audio Mixer is not installed; no files changed.')
            return
        systemctl('show-environment')
        fragment = systemctl('show', UNIT, '--property=FragmentPath', '--value', check=False).stdout.strip()
        if fragment and Path(fragment) != unit:
            fail('A different service already uses ' + UNIT + ': ' + fragment)
        enabled = systemctl('is-enabled', UNIT, check=False).stdout.strip()
        if enabled not in ('enabled', 'enabled-runtime', 'disabled', 'not-found', ''):
            fail('Unsupported existing service enable state: ' + enabled)
        enablement = {
            'persistent': enabled == 'enabled',
            'runtime': enabled == 'enabled-runtime' or
                       (Path(os.environ['XDG_RUNTIME_DIR']) / 'systemd/user/default.target.wants' / UNIT).is_symlink(),
        }
        running = service_running()
        backups = {path: path.read_bytes() if path.exists() else None for path in (unit, desktop)}
        if args[0] == 'uninstall':
            changed = False
            try:
                stop_engine(root, metadata, running)
                changed = True
                systemctl('disable', UNIT)
                systemctl('disable', '--runtime', UNIT)
                for path in (unit, desktop):
                    path.unlink(missing_ok=True)
                systemctl('daemon-reload')
            except BaseException as error:
                rollback_errors = []
                if changed:
                    for path, content in backups.items():
                        try:
                            if content is not None:
                                atomic(path, content)
                        except Exception as restore_error:
                            rollback_errors.append(str(restore_error))
                    for action in (lambda: systemctl('daemon-reload'), lambda: restore_enablement(enablement)):
                        try:
                            action()
                        except Exception as restore_error:
                            rollback_errors.append(str(restore_error))
                if running:
                    try:
                        systemctl('start', UNIT)
                    except Exception as restore_error:
                        rollback_errors.append(str(restore_error))
                if rollback_errors:
                    fail(str(error) + '\nUninstall rollback also failed:\n' + '\n'.join(rollback_errors))
                raise
            # Release removal is irreversible: do it only after the manager has
            # accepted removal of the restored-on-failure integration files.
            shutil.rmtree(root)
            print('Uninstalled Cosmos Audio Mixer. Profiles and settings were retained.')
            return
        bundle = Path(sys.argv[1]).resolve().parent
        files = bundle_files(bundle)
        release = uuid.uuid4().hex
        old_current = metadata['current'] if metadata else None
        old_marker = (root / MARKER).read_bytes() if metadata else None
        stage = root / 'releases' / release
        switched = False
        stopped = False
        new_started = False
        try:
            root.mkdir(mode=0o700, exist_ok=True)
            (root / 'releases').mkdir(exist_ok=True)
            stage.mkdir(mode=0o700)
            for name, checksum in files.items():
                target = stage / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(bundle / name, target)
                if digest(target) != checksum:
                    fail('Staged file checksum failed: ' + name)
            runtime_check(stage)
            executable = str(root / 'current/cosmos-engine')
            environment = ['XDG_CONFIG_HOME=' + str(config), 'XDG_DATA_HOME=' + str(data)]
            unit_values = {'EXEC_START': quote_unit(executable, True) + ' run',
                           'ENVIRONMENT': ' '.join(map(quote_unit, environment))}
            unit_text = re.sub(r'@(EXEC_START|ENVIRONMENT)@', lambda m: unit_values[m[1]],
                               (stage / 'data' / UNIT).read_text())
            command = ['/usr/bin/env', *environment, str(root / 'current/cosmos-audio-mixer')]
            desktop_values = {'EXEC': ' '.join(map(quote_desktop, command)),
                              'ICON': str(root / 'current/data' / (APP_ID + '.svg')).replace('\\', '\\\\')}
            desktop_text = re.sub(r'@(EXEC|ICON)@', lambda m: desktop_values[m[1]],
                                  (stage / 'data' / (APP_ID + '.desktop')).read_text())
            stopped = True
            stop_engine(root, metadata, running)
            point(root, release)
            switched = True
            atomic(unit, unit_text.encode())
            atomic(desktop, desktop_text.encode())
            new = {'owner': APP_ID, 'version': 1, 'uid': os.getuid(), 'config': str(config), 'data': str(data),
                   'current': release, 'releases': dict(metadata['releases']) if metadata else {},
                   'integration': {str(unit): hashlib.sha256(unit_text.encode()).hexdigest(),
                                   str(desktop): hashlib.sha256(desktop_text.encode()).hexdigest()}}
            new['releases'][release] = dict(files)
            atomic(root / MARKER, (json.dumps(new, indent=2) + '\n').encode(), 0o600)
            systemctl('daemon-reload')
            loaded = systemctl('show', UNIT, '--property=FragmentPath', '--value').stdout.strip()
            if loaded != str(unit):
                fail('User manager did not load the installed unit. Its unit search path must include ' + str(unit.parent))
            if '--enable' in args:
                systemctl('enable', UNIT)
            if running or '--enable' in args:
                new_started = True
                systemctl('start', UNIT)
                systemctl('is-active', '--quiet', UNIT)
        except BaseException as error:
            rollback_errors = []
            def restore(action):
                try:
                    action()
                except Exception as rollback_error:
                    rollback_errors.append(str(rollback_error))
            if switched:
                if new_started:
                    try:
                        stop_engine(root, new, service_running())
                    except Exception as stop_error:
                        fail(str(error) + '\nCannot safely roll back the new engine: ' + str(stop_error)
                             + '\nNew install and prior releases were retained; fix the save/stop error and retry.')
                # disable requires a unit file, including on a failed first
                # installation. Restore links before restoring/removing files.
                restore(lambda: restore_enablement(enablement))
                if old_current:
                    restore(lambda: point(root, old_current))
                else:
                    restore(lambda: (root / 'current').unlink(missing_ok=True))
                for path, content in backups.items():
                    if content is None:
                        restore(lambda p=path: p.unlink(missing_ok=True))
                    else:
                        restore(lambda p=path, c=content: atomic(p, c))
                restore(lambda: systemctl('daemon-reload'))
            if old_marker:
                restore(lambda: atomic(root / MARKER, old_marker, 0o600))
            if stage.exists():
                restore(lambda: shutil.rmtree(stage))
            if metadata is None:
                restore(lambda: (root / MARKER).unlink(missing_ok=True))
                if (root / 'releases').exists():
                    restore(lambda: (root / 'releases').rmdir())
                if root.exists():
                    restore(lambda: root.rmdir())
            if stopped and running:
                restore(lambda: systemctl('start', UNIT))
            if rollback_errors:
                fail(str(error) + '\nRollback also failed; inspect install before retrying:\n' + '\n'.join(rollback_errors))
            raise
        print('Installed Cosmos Audio Mixer at ' + str(root / 'current'))
        print('Login startup: ' + ('enabled' if '--enable' in args or enabled in ('enabled', 'enabled-runtime') else 'disabled'))


def interrupted(_number, _frame):
    raise KeyboardInterrupt('Installation interrupted.')


signal.signal(signal.SIGTERM, interrupted)
try:
    main()
except (Exception, KeyboardInterrupt) as error:
    print('Cosmos installer: ' + str(error), file=sys.stderr)
    sys.exit(1)
PY
