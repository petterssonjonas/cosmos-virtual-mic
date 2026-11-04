#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Run the real downloader/local installer with isolated ELF and transport fixtures.

Only curl, uname, and the existing private systemctl fixture are substituted.
No release is fetched and no command can reach the desktop service manager.
"""
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile

from installer import APP, Fixture, ROOT

BASE = 'https://github.com/petterssonjonas/cosmos-virtual-mic/releases'
SCRIPT = ROOT / 'install-release.sh'
CURL = r'''#!/usr/bin/env python3
import json
import os
from pathlib import Path
import signal
import sys
args = sys.argv[1:]
with open(os.environ['CURL_LOG'], 'a') as log:
    log.write(json.dumps(args) + '\n')
for option, value in (('--proto', '=https'), ('--proto-redir', '=https')):
    assert args[args.index(option) + 1] == value, args
for option in ('--fail', '--silent', '--show-error', '--location'):
    assert option in args, args
for option in ('--connect-timeout', '--max-time'):
    assert float(args[args.index(option) + 1]) > 0, args
base = 'https://github.com/petterssonjonas/cosmos-virtual-mic/releases'
url = args[-1]
output = Path(args[args.index('--output') + 1])
if '--head' in args:
    assert url == base + '/latest', args
    assert str(output) == '/dev/null', args
    assert args[args.index('--write-out') + 1] == '%{url_effective}', args
    if os.environ.get('CURL_FAIL') == 'latest':
        sys.exit(22)
    print(os.environ.get('LATEST_RELEASE_URL', base + '/tag/v0.2'), end='')
    sys.exit(0)
prefix = base + '/download/' + os.environ.get('EXPECTED_TAG', 'v0.2') + '/'
assert url.startswith(prefix), url
name = url[len(prefix):]
assert name and '/' not in name, url
assert output.name == name, output
assert output.parent.parent == Path(os.environ['TMPDIR']), output
assert output.parent.stat().st_mode & 0o777 == 0o700, output
source = Path(os.environ['PUBLISHED_DIR']) / name
payload = source.read_bytes()
kind = 'checksum' if name == 'SHA256SUMS' else 'archive'
if os.environ.get('CURL_FAIL') == kind:
    output.write_bytes(payload[:max(1, len(payload) // 2)])
    sys.exit(18)
if os.environ.get('CURL_SIGNAL') == kind:
    output.write_bytes(payload[:max(1, len(payload) // 2)])
    os.kill(os.getppid(), signal.SIGTERM)
    sys.exit(18)
output.write_bytes(payload)
'''
UNAME = r'''#!/usr/bin/env python3
import os
import sys
assert sys.argv[1:] in (['-s'], ['-m']), sys.argv
print(os.environ.get('UNAME_SYSTEM', 'Linux') if sys.argv[1] == '-s'
      else os.environ.get('UNAME_MACHINE', 'x86_64'))
'''


class ReleaseFixture(Fixture):
    def __init__(self, root):
        super().__init__(root)
        shutil.copy2(ROOT / 'LICENSE', self.bundle / 'LICENSE')
        license_text = b'Deterministic third-party fixture license.\n'
        self.license_name = 'LICENSES/' + hashlib.sha256(license_text).hexdigest() + '.txt'
        (self.bundle / 'LICENSES').mkdir()
        (self.bundle / self.license_name).write_bytes(license_text)
        (self.bundle / 'THIRD-PARTY-NOTICES.txt').write_text('Fixture third-party notices.\n')
        (self.bundle / 'dependency-inventory.json').write_text('{}\n')
        (self.bundle / 'BUILD-PROVENANCE.json').write_text('{}\n')
        (self.bundle / 'RUNTIME-REQUIREMENTS.txt').write_text('Fixture runtime requirements.\n')
        (self.bundle / 'data/compatibility.conf').write_text('ARCH=x86_64\nGLIBC_MIN=2.44\n')
        self.checksums()
        self.published = root / 'release-assets'
        self.staging = root / 'staging'
        self.published.mkdir()
        self.staging.mkdir()
        self.requests = root / 'curl-requests.jsonl'
        self.env |= {'CURL_LOG': str(self.requests), 'PUBLISHED_DIR': str(self.published),
                     'TMPDIR': str(self.staging)}
        for name, source in (('curl', CURL), ('uname', UNAME)):
            (self.bin / name).write_text(source)
            (self.bin / name).chmod(0o755)
        self.archive()

    def archive(self, *, version='0.2.0', extra=(), omit=()):
        name = f'cosmos-virtual-mic-{version}-linux-x86_64.tar.gz'
        self.asset = self.published / name
        with tarfile.open(self.asset, 'w:gz', format=tarfile.USTAR_FORMAT) as archive:
            for path in sorted(self.bundle.rglob('*')):
                member = path.relative_to(self.bundle).as_posix()
                if not path.is_file() or member in omit:
                    continue
                info = tarfile.TarInfo(member)
                payload = path.read_bytes()
                info.size = len(payload)
                info.mode = 0o755 if member in (APP, 'cosmos-virtual-mic-engine', 'install.sh') else 0o644
                archive.addfile(info, io.BytesIO(payload))
            for info, payload in extra:
                archive.addfile(info, io.BytesIO(payload) if payload is not None else None)
        digest = hashlib.sha256(self.asset.read_bytes()).hexdigest()
        self.manifest = self.published / 'SHA256SUMS'
        self.manifest.write_text(
            digest + '  ' + name + '\n' +
            '0' * 64 + f'  cosmos-virtual-mic-{version}-1.x86_64.rpm\n' +
            '0' * 64 + f'  cosmos-virtual-mic_{version}_amd64.deb\n' +
            '0' * 64 + '  install-release.sh\n')

    def download(self, *args, ok=True, extra=None, pipe=False, content=None):
        command = ['bash', '-s', '--'] if pipe or content is not None else ['bash', str(SCRIPT)]
        source = SCRIPT.read_text() if pipe and content is None else content
        result = subprocess.run(command + list(args), input=source,
                                env=self.env | (extra or {}), cwd=self.root,
                                capture_output=True, text=True, timeout=50)
        assert (result.returncode == 0) == ok, (args, result.returncode, result.stdout, result.stderr)
        assert not list(self.staging.iterdir()), 'Release staging was not cleaned'
        return result

    def urls(self):
        if not self.requests.exists():
            return []
        return [json.loads(line)[-1] for line in self.requests.read_text().splitlines()]

    def snapshot(self):
        contents = {}
        for root in (self.home, self.config, self.data, self.root / 'run'):
            for path in (root, *root.rglob('*')):
                contents[str(path.relative_to(self.root))] = (
                    path.stat().st_mode, path.read_bytes() if path.is_file() else None)
        for path in (self.state, self.log):
            contents[path.name] = path.read_bytes() if path.exists() else None
        return contents

    def rejected(self, *args, extra=None, network=True, manager=False):
        before = self.snapshot()
        requests = self.urls()
        self.download(*args, ok=False, extra=extra)
        assert self.snapshot() == before, 'A rejected release changed installed files or manager state'
        if not network:
            assert self.urls() == requests, 'Preflight failure reached the transport'
        if not manager:
            assert (self.log.read_bytes() if self.log.exists() else None) == before[self.log.name]


def regular(name, payload=b'unsafe\n'):
    info = tarfile.TarInfo(name)
    info.size = len(payload)
    return info, payload


def main():
    assert os.getuid() != 0, 'Run as a normal user; root/sudo rejection must remain real.'
    with tempfile.TemporaryDirectory(prefix='cosmos-release-installer-', dir='/var/tmp') as temporary:
        f = ReleaseFixture(Path(temporary))
        for extra in ({'SUDO_USER': 'root'}, {'UNAME_MACHINE': 'aarch64'}, {'UNAME_SYSTEM': 'Darwin'}):
            f.rejected(extra=extra, network=False)
        for args in (('--version',), ('--version', ''), ('--version', 'v0.2/../../other'),
                     ('--version', 'v0.2;false'), ('--version', 'v00.2'), ('--version', '0.2'),
                     ('--enable', '--enable'), ('--version', 'v0.2', '--version', 'v0.2'),
                     ('uninstall',), ('--unknown',)):
            f.rejected(*args, network=False)
        f.download('--help')
        assert f.urls() == []

        # stdin execution must not depend on BASH_SOURCE or the current directory.
        f.download(pipe=True)
        f.same_pair('old')
        assert f.urls() == [BASE + '/latest', BASE + '/download/v0.2/SHA256SUMS',
                            BASE + '/download/v0.2/' + f.asset.name]
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        assert (f.data / APP / 'install.sh').read_bytes() == (ROOT / 'install.sh').read_bytes()
        assert f.profile.read_text() == 'preserve user settings'

        f.requests.unlink()
        f.download('--version', 'v0.2')
        assert f.urls() == [BASE + '/download/v0.2/SHA256SUMS', BASE + '/download/v0.2/' + f.asset.name]
        f.archive(version='0.2.3')
        f.requests.unlink()
        f.download('--version', 'v0.2.3', extra={'EXPECTED_TAG': 'v0.2.3'})
        assert f.urls() == [BASE + '/download/v0.2.3/SHA256SUMS', BASE + '/download/v0.2.3/' + f.asset.name]
        f.archive()

        compatibility = f.bundle / 'data/compatibility.conf'
        compatibility.write_text('ARCH=x86_64\nGLIBC_MIN=2.44\n$(touch unsafe)\n')
        f.checksums()
        f.archive()
        f.download()
        assert not (f.root / 'unsafe').exists(), 'Bundle metadata was executed'
        compatibility.write_text('ARCH=x86_64\nGLIBC_MIN=2.44\n')
        f.checksums()
        f.archive()

        for url in ('https://example.invalid/releases/tag/v0.2', BASE + '/tag/not-a-version',
                    BASE + '/tag/v0.2?other=tag', 'http://github.com/petterssonjonas/cosmos-virtual-mic/releases/tag/v0.2'):
            f.rejected(extra={'LATEST_RELEASE_URL': url})
        for kind in ('latest', 'checksum', 'archive'):
            f.rejected(extra={'CURL_FAIL': kind})
        f.rejected(extra={'CURL_SIGNAL': 'archive'})

        original = f.manifest.read_text()
        archive_line = original.splitlines(True)[0]
        for manifest in ('0' * 64 + '  ' + f.asset.name + '\n',
                         ''.join(original.splitlines(True)[1:]), original + archive_line,
                         original + '0' * 64 + '  ../../escape\n',
                         original + '$(touch unsafe)\n'):
            f.manifest.write_text(manifest)
            f.rejected()
        f.manifest.write_text(original)
        f.asset.write_bytes(f.asset.read_bytes()[:100])  # Interrupted bytes despite a successful curl status.
        f.rejected()
        f.archive()
        f.asset.write_bytes(b'Not a gzip or tar archive.\n')
        f.manifest.write_text(hashlib.sha256(f.asset.read_bytes()).hexdigest() + '  ' + f.asset.name + '\n')
        f.rejected()
        f.archive()

        # A matching release checksum does not authorize unsafe tar member types/names.
        cases = [([regular('../outside')], ()),
                 ([regular(str(f.root / 'absolute-escape'))], ()),
                 ([regular('data/../../outside')], ()),
                 ([regular('unexpected-file')], ()),
                 ([regular('cosmos-virtual-mic cosmos-virtual-mic-engine')], ()),
                 ([regular('data/new\nline')], ()),
                 ([regular('$(touch unsafe)')], ()),
                 ([regular(APP)], ()),
                 ([], ('install.sh',)),
                 ([], (f.license_name,)),
                 ([], ('BUILD-PROVENANCE.json',))]
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE, tarfile.BLKTYPE, tarfile.DIRTYPE):
            info = tarfile.TarInfo(APP)
            info.type = kind
            info.linkname = str(f.root / 'outside') if kind == tarfile.SYMTYPE else 'LICENSE'
            cases.append(([(info, None)], (APP,)))
        for extra, omit in cases:
            f.archive(extra=extra, omit=omit)
            f.rejected()
            assert not (f.root / 'outside').exists()
            assert not (f.root / 'absolute-escape').exists()
            assert not (f.root / 'unsafe').exists()
        f.archive()

        # Even a truncated entrypoint with --enable must not reach curl or systemctl.
        source = SCRIPT.read_text()
        invocation = source.rindex('{ cosmos_install_release "$@"; }')
        boundaries = {0, 1, len(source) // 2, invocation}
        boundaries.update(range(invocation, source.rindex('}') + 1))
        for boundary in sorted(boundaries):
            before, requests = f.snapshot(), f.urls()
            result = subprocess.run(['bash', '-s', '--', '--enable'], input=source[:boundary],
                                    env=f.env, cwd=f.root, capture_output=True, text=True, timeout=10)
            assert f.snapshot() == before, (boundary, result.stderr)
            assert f.urls() == requests, (boundary, result.stderr)
            assert not list(f.staging.iterdir())

        # Explicit enable is the only first-install/start/login opt-in. Retained uninstall
        # remains local, retains user settings, and never fetches another release.
        f.download('--enable', pipe=True)
        assert f.status() == {'persistent': True, 'runtime': False, 'running': True}
        requests = f.urls()
        f.run('uninstall', installed=True)
        assert f.urls() == requests
        assert not (f.install / APP).exists() and not f.unit.exists()
        assert f.profile.read_text() == 'preserve user settings'
        assert f.status() == {'persistent': False, 'runtime': False, 'running': False}
        for path, payload in f.mixer_files.items():
            assert path.read_bytes() == payload
    print('PASS: pinned release/stdin installation, opt-in service, retained uninstall, checksum/download/platform/archive/truncation failures without installed changes')


if __name__ == '__main__':
    main()
