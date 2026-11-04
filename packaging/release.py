#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Published-Release gating, native package creation, and staged artifact smoke checks."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

from licenses import TARGET, audit

ROOT = Path(__file__).resolve().parent.parent
APP = "cosmos-virtual-mic"
ID = "io.github.petterssonjonas.CosmosVirtualMic"
BINARIES = (APP, APP + "-engine")
IMAGE = "registry.fedoraproject.org/fedora-toolbox@sha256:bb687ae7bbfc8b957b1ac04bb0fd92f8e443c657220e863cfdf76a692342d663"
# Explicit native dependencies include dlopen-loaded Wayland/EGL and the LV2 host.
DEB_LIBRARIES = {
    "libc.so.6": "libc6 (>= 2.44)", "libm.so.6": "libc6 (>= 2.44)",
    "libdl.so.2": "libc6 (>= 2.44)", "librt.so.1": "libc6 (>= 2.44)",
    "libpthread.so.0": "libc6 (>= 2.44)", "ld-linux-x86-64.so.2": "libc6 (>= 2.44)",
    "libgcc_s.so.1": "libgcc-s1", "libstdc++.so.6": "libstdc++6",
    "libfontconfig.so.1": "libfontconfig1", "libfreetype.so.6": "libfreetype6",
    "libexpat.so.1": "libexpat1", "libxkbcommon.so.0": "libxkbcommon0",
    "libwayland-client.so.0": "libwayland-client0", "libwayland-cursor.so.0": "libwayland-cursor0",
    "libwayland-egl.so.1": "libwayland-egl1", "libEGL.so.1": "libegl1",
    "libGL.so.1": "libgl1", "libpipewire-0.3.so.0": "libpipewire-0.3-0 (>= 1.0)",
    "libdbus-1.so.3": "libdbus-1-3", "libz.so.1": "zlib1g",
}
DEB_RUNTIME = {"libc6 (>= 2.44)", "pipewire (>= 1.0)", "wireplumber", "libpipewire-0.3-modules",
               "libspa-0.2-modules", "liblilv-0-0", "lsp-plugins-lv2", "libegl1", "libgl1",
               "libxkbcommon0", "libwayland-client0", "libwayland-cursor0", "libwayland-egl1"}
RUNTIME = """Cosmos Virtual Mic runtime requirements
Architecture: x86_64 / amd64; Linux; glibc >= 2.44 (Fedora 45 build baseline).
DEB: Ubuntu 26.10+ or a Debian system providing glibc >= 2.44 and the declared
package dependencies. Stock Ubuntu 24.04 and 26.04 are NOT supported.
A working Wayland desktop, system graphics/font libraries, PipeWire >= 1.0,
WirePlumber, PipeWire's native filter-chain/LV2 host, Lilv, and LSP LV2 plugins
must be installed by the operating system. No desktop/audio stack is bundled.
RPM requires pipewire-module-filter-chain-lv2 and lsp-plugins-lv2.
DEB requires libpipewire-0.3-modules, libspa-0.2-modules and lsp-plugins-lv2;
the distribution's PipeWire build must include native LV2 support.
System packages install an opt-in user service; installation does not enable,
start or restart services, change microphone defaults, or run audio hooks.
After installation, enable only when desired: systemctl --user enable --now cosmos-virtual-mic.service
Before upgrading/removing a running engine, save/stop with: cosmos-virtual-mic-engine stop
Then: systemctl --user stop cosmos-virtual-mic.service
The tarball's install.sh manages a separate per-user installation and rejects
an existing system-owned unit. Do not mix system and per-user installations.
Dependency inventory is a source-license policy audit, NOT a vulnerability scan.
"""


def run(*args, **kwargs):
    return subprocess.run([str(arg) for arg in args], check=True, **kwargs)


def capture(*args):
    return subprocess.check_output([str(arg) for arg in args], text=True).strip()


def version_from_tag(tag):
    match = re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:\.(0|[1-9][0-9]*))?", tag)
    if not match:
        raise ValueError("Release tag must be vX.Y or vX.Y.Z without leading zeros")
    return ".".join((*match.groups()[:2], match.group(3) or "0"))


def release_event(event):
    release = event.get("release", {})
    if event.get("action") != "published" or release.get("draft") or release.get("prerelease"):
        raise ValueError("Only a published, non-draft, non-prerelease Release can build packages")
    if release.get("target_commitish") != "main" or not isinstance(release.get("id"), int) or release["id"] <= 0:
        raise ValueError("Release must target main and carry its exact positive Release ID")
    return release, version_from_tag(release.get("tag_name", ""))


def gate():
    if os.environ.get("GITHUB_EVENT_NAME") != "release":
        raise ValueError("Distribution packaging is restricted to release.published")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    release, version = release_event(event)
    tag = release["tag_name"]
    commit = capture("git", "rev-parse", f"refs/tags/{tag}^{{commit}}")
    if commit != capture("git", "rev-parse", "HEAD"):
        raise ValueError("Checkout does not match the triggering Release tag")
    event_commit = os.environ.get("GITHUB_SHA", "")
    if not re.fullmatch(r"[0-9a-f]{40}", event_commit) or commit != event_commit:
        raise ValueError("Checkout does not match the commit captured by Release publication")
    run("git", "merge-base", "--is-ancestor", commit, "refs/remotes/origin/main")
    for path in sorted((ROOT / "crates").glob("*/Cargo.toml")):
        if tomllib.loads(path.read_text())["package"]["version"] != version:
            raise ValueError(f"Release tag/version mismatch: {path}")
    lock = tomllib.loads((ROOT / "Cargo.lock").read_text())
    for package in lock["package"]:
        if package["name"] in {"cosmos-control", "cosmos-engine", APP} and package["version"] != version:
            raise ValueError("Workspace lock version does not match the Release")
    return release, version, commit


def checksum_manifest(directory):
    files = sorted(p for p in directory.rglob("*") if p.is_file() and p.name != "SHA256SUMS")
    (directory / "SHA256SUMS").write_text("".join(
        f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(directory)}\n" for path in files))


def inspect_binary(path):
    header = capture("readelf", "-h", path)
    if "Advanced Micro Devices X86-64" not in header or "ELF64" not in header:
        raise ValueError(f"Not an x86_64 ELF binary: {path}")
    linked = capture("ldd", path)
    if "not found" in linked:
        raise ValueError(f"Missing runtime dependency for {path}:\n{linked}")
    run("timeout", "15", path, "--help", stdout=subprocess.DEVNULL)
    libraries = re.findall(r"\(NEEDED\).*?\[([^]]+)\]", capture("readelf", "-d", path))
    unknown = set(libraries) - DEB_LIBRARIES.keys()
    if unknown:
        raise ValueError(f"Unmapped Debian runtime dependencies: {sorted(unknown)}")
    versions = capture("readelf", "--version-info", path)
    glibc = [tuple(map(int, version)) for version in re.findall(r"\bGLIBC_(\d+)\.(\d+)\b", versions)]
    if not glibc or max(glibc) > (2, 44):
        raise ValueError(f"ELF exceeds the declared glibc 2.44 baseline: {path}")
    return {"needed": libraries, "ldd": linked, "version_requirements": versions}


def copy_file(source, target, mode=0o644):
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(source, target)
    target.chmod(mode)


def stage_system(bundle, stage):
    for binary in BINARIES:
        copy_file(bundle / binary, stage / "usr/bin" / binary, 0o755)
    unit = (bundle / "data" / f"{APP}.service").read_text()
    unit = unit.replace("# Managed by the Cosmos Virtual Mic per-user installer.\n", "")
    unit = unit.replace("@EXEC_START@", "/usr/bin/cosmos-virtual-mic-engine run")
    unit = unit.replace("Environment=@ENVIRONMENT@\n", "")
    destination = stage / "usr/lib/systemd/user" / f"{APP}.service"
    destination.parent.mkdir(parents=True)
    destination.write_text(unit)
    desktop = (bundle / "data" / f"{ID}.desktop").read_text()
    desktop = desktop.replace("# Managed by the Cosmos Virtual Mic per-user installer.\n", "")
    desktop = desktop.replace("@EXEC@", "/usr/bin/cosmos-virtual-mic").replace("@ICON@", ID)
    destination = stage / "usr/share/applications" / f"{ID}.desktop"
    destination.parent.mkdir(parents=True)
    destination.write_text(desktop)
    copy_file(bundle / "data" / f"{ID}.svg", stage / "usr/share/icons/hicolor/scalable/apps" / f"{ID}.svg")
    license_dir = stage / "usr/share/licenses" / APP
    for name in ("LICENSE", "THIRD-PARTY-NOTICES.txt", "dependency-inventory.json"):
        copy_file(bundle / name, license_dir / name)
    shutil.copytree(bundle / "LICENSES", license_dir / "LICENSES")
    for name in ("BUILD-PROVENANCE.json", "RUNTIME-REQUIREMENTS.txt"):
        copy_file(bundle / name, stage / "usr/share/doc" / APP / name)
    (stage / "usr/share/doc" / APP / "copyright").write_text(
        f"Cosmos Virtual Mic\nSource: https://github.com/petterssonjonas/cosmos-virtual-mic\n\n"
        + (bundle / "LICENSE").read_text() + "\n" + (bundle / "THIRD-PARTY-NOTICES.txt").read_text())
    for text in (unit, desktop):
        if re.search(r"@[A-Z_]+@", text):
            raise ValueError("Unresolved package template")
    run("desktop-file-validate", stage / "usr/share/applications" / f"{ID}.desktop")
    # Verify in a disposable root so executable checks see the packaged engine,
    # not an unrelated host installation. Core target units come from the builder.
    with tempfile.TemporaryDirectory(prefix="cosmos-unit-verify-") as temporary:
        verify_root = Path(temporary)
        shutil.copytree(stage, verify_root, dirs_exist_ok=True)
        shutil.copytree("/usr/lib/systemd/system", verify_root / "usr/lib/systemd/system",
                        dirs_exist_ok=True, symlinks=True)
        # --root is system-mode only; select the staged user-unit directory.
        environment = dict(os.environ, SYSTEMD_UNIT_PATH="/usr/lib/systemd/user:/usr/lib/systemd/system")
        run("systemd-analyze", f"--root={verify_root}", "--man=no", "--generators=no",
            "--recursive-errors=no", "verify", f"{APP}.service", env=environment)


def smoke_tree(stage, expected):
    actual = {str(path.relative_to(stage)): hashlib.sha256(path.read_bytes()).hexdigest()
              for path in stage.rglob("*") if path.is_file()}
    if actual != expected:
        raise ValueError("Extracted package payload differs from checked staging payload")
    for binary in BINARIES:
        inspect_binary(stage / "usr/bin" / binary)
    unit = stage / "usr/lib/systemd/user" / f"{APP}.service"
    if "ExecStart=/usr/bin/cosmos-virtual-mic-engine run" not in unit.read_text():
        raise ValueError("Incorrect packaged engine path")


def build():
    release, version, commit = gate()
    os.chdir(ROOT)
    if os.uname().machine != "x86_64":
        raise ValueError("Unsupported build architecture")
    libc = capture("getconf", "GNU_LIBC_VERSION")
    if tuple(map(int, libc.split()[1].split("."))) < (2, 44):
        raise ValueError("Build glibc baseline must be >= 2.44")
    output = ROOT / "dist"
    if output.exists():
        raise ValueError("dist already exists; use a clean Release checkout")
    with tempfile.TemporaryDirectory(prefix="cosmos-release-") as temporary:
        work = Path(temporary)
        notices = work / "notices"
        audit(notices)
        run("cargo", "build", "--workspace", "--release", "--locked", "--target", TARGET)
        bundle = work / "bundle"
        shutil.copytree(notices, bundle)
        binary_info = {}
        for binary in BINARIES:
            source = ROOT / "target" / TARGET / "release" / binary
            binary_info[binary] = inspect_binary(source)
            copy_file(source, bundle / binary, 0o755)
        for name in ("LICENSE", "install.sh"):
            copy_file(ROOT / name, bundle / name, 0o755 if name.endswith(".sh") else 0o644)
        for name in (f"{APP}.service", f"{ID}.desktop", f"{ID}.svg"):
            copy_file(ROOT / "data" / name, bundle / "data" / name)
        (bundle / "data/compatibility.conf").write_text("ARCH=x86_64\nGLIBC_MIN=2.44\n")
        (bundle / "RUNTIME-REQUIREMENTS.txt").write_text(RUNTIME)
        provenance = {"version": version, "tag": release["tag_name"], "release_id": release["id"],
                      "commit": commit, "target": TARGET, "builder_image": IMAGE,
                      "cargo_lock_sha256": hashlib.sha256((ROOT / "Cargo.lock").read_bytes()).hexdigest(),
                      "rustc": capture("rustc", "-Vv"), "cargo": capture("cargo", "-V"), "glibc": libc,
                      "native_packages": capture("rpm", "-qa", "--qf", "%{NAME} %{VERSION}-%{RELEASE}.%{ARCH}\\n").splitlines(),
                      "binaries": binary_info,
                      "workflow_run": f"{os.environ.get('GITHUB_SERVER_URL', 'https://github.com')}/{os.environ.get('GITHUB_REPOSITORY', '')}/actions/runs/{os.environ.get('GITHUB_RUN_ID', '')}",
                      "audit": "Resolved dependency license-policy audit; no vulnerability scan performed."}
        (bundle / "BUILD-PROVENANCE.json").write_text(json.dumps(provenance, indent=2) + "\n")
        checksum_manifest(bundle)
        run("sha256sum", "--check", "--strict", "SHA256SUMS", cwd=bundle)
        output.mkdir()
        archive = output / f"{APP}-{version}-linux-x86_64.tar.gz"
        with tarfile.open(archive, "w:gz", format=tarfile.PAX_FORMAT) as tar:
            for path in sorted(bundle.rglob("*")):
                if path.is_file():
                    tar.add(path, arcname=str(path.relative_to(bundle)), recursive=False)
        extracted = work / "tar-smoke"
        extracted.mkdir()
        with tarfile.open(archive) as tar:
            tar.extractall(extracted, filter="data")
        run("sha256sum", "--check", "--strict", "SHA256SUMS", cwd=extracted)
        for binary in BINARIES:
            inspect_binary(extracted / binary)
        stage = work / "system"
        stage_system(bundle, stage)
        expected = {str(path.relative_to(stage)): hashlib.sha256(path.read_bytes()).hexdigest()
                    for path in stage.rglob("*") if path.is_file()}
        deb_stage = work / "deb"
        shutil.copytree(stage, deb_stage)
        dependencies = set(DEB_RUNTIME)
        for info in binary_info.values():
            dependencies.update(DEB_LIBRARIES[name] for name in info["needed"])
        control = deb_stage / "DEBIAN"
        control.mkdir()
        (control / "control").write_text(
            f"Package: {APP}\nVersion: {version}\nSection: sound\nPriority: optional\nArchitecture: amd64\n"
            "Maintainer: Jonas Pettersson <petterssonjonas@users.noreply.github.com>\n"
            f"Depends: {', '.join(sorted(dependencies))}\n"
            "Homepage: https://github.com/petterssonjonas/cosmos-virtual-mic\n"
            "Description: Single-microphone processing and virtual microphone for COSMIC\n"
            " Requires a glibc 2.44+ system with native PipeWire LV2 support.\n")
        deb = output / f"{APP}_{version}_amd64.deb"
        run("dpkg-deb", "--root-owner-group", "--build", deb_stage, deb)
        deb_smoke = work / "deb-smoke"
        run("dpkg-deb", "--extract", deb, deb_smoke)
        smoke_tree(deb_smoke, expected)
        if capture("dpkg-deb", "--field", deb, "Architecture") != "amd64":
            raise ValueError("Incorrect DEB architecture")
        rpm_top = work / "rpmbuild"
        rpm_top.mkdir()
        spec = work / "cosmos.spec"
        spec.write_text(f"""Name: {APP}
Version: {version}
Release: 1
Summary: Single-microphone processing and virtual microphone for COSMIC
License: MPL-2.0 AND LicenseRef-ThirdParty-Notices
URL: https://github.com/petterssonjonas/cosmos-virtual-mic
BuildArch: x86_64
Requires: glibc >= 2.44
Requires: pipewire >= 1.0, pipewire-libs, pipewire-module-filter-chain-lv2, wireplumber, lilv, lsp-plugins-lv2
Requires: libglvnd-egl, libglvnd-glx, libxkbcommon, libwayland-client, libwayland-cursor, libwayland-egl
%description
Single-microphone processing using system PipeWire native LV2 support.
See the installed license inventory and runtime requirements.
%install
mkdir -p %{{buildroot}}
cp -a {stage}/. %{{buildroot}}/
%files
/usr/bin/{APP}
/usr/bin/{APP}-engine
/usr/lib/systemd/user/{APP}.service
/usr/share/applications/{ID}.desktop
/usr/share/icons/hicolor/scalable/apps/{ID}.svg
%license /usr/share/licenses/{APP}
%doc /usr/share/doc/{APP}
""")
        run("rpmbuild", "-bb", "--target", "x86_64", "--define", f"_topdir {rpm_top}",
            "--define", "_build_id_links none", "--define", "debug_package %{nil}",
            "--define", "__os_install_post %{nil}", spec)
        rpm = output / f"{APP}-{version}-1.x86_64.rpm"
        shutil.copyfile(rpm_top / "RPMS/x86_64" / rpm.name, rpm)
        if capture("rpm", "-qp", "--qf", "%{ARCH}", rpm) != "x86_64":
            raise ValueError("Incorrect RPM architecture")
        if capture("rpm", "-qp", "--scripts", rpm):
            raise ValueError("RPM must not contain install/service hooks")
        rpm_smoke = work / "rpm-smoke"
        rpm_smoke.mkdir()
        with subprocess.Popen(["rpm2cpio", str(rpm)], stdout=subprocess.PIPE) as source:
            run("cpio", "--extract", "--make-directories", "--no-absolute-filenames", cwd=rpm_smoke, stdin=source.stdout)
            source.stdout.close()
            if source.wait():
                raise ValueError("rpm2cpio failed")
        smoke_tree(rpm_smoke, expected)
        copy_file(ROOT / "install-release.sh", output / "install-release.sh", 0o755)
        checksum_manifest(output)
        run("sha256sum", "--check", "--strict", "SHA256SUMS", cwd=output)
        print(f"Checked release assets for Release ID {release['id']} in {output}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("gate", "build"))
    arguments = parser.parse_args()
    if arguments.command == "gate":
        release, version, commit = gate()
        if os.environ.get("GITHUB_OUTPUT"):
            with Path(os.environ["GITHUB_OUTPUT"]).open("a") as output:
                output.write(f"commit={commit}\n")
        print(json.dumps({"release_id": release["id"], "tag": release["tag_name"], "version": version, "commit": commit}))
    else:
        build()
