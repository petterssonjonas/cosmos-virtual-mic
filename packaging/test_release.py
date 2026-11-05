#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Offline policy/upload fixtures; never build application distribution packages."""
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import subprocess
import tempfile
from unittest.mock import patch

import licenses
import release
import upload


def rejects(function, *args):
    try:
        function(*args)
    except (ValueError, SystemExit):
        return
    raise AssertionError(f"Expected rejection: {function.__name__}{args!r}")


def main():
    for tag, version in (("v0.2", "0.2.0"), ("v0.2.0", "0.2.0"), ("v10.20.30", "10.20.30")):
        assert release.version_from_tag(tag) == version
    for tag in ("0.2", "v01.2", "v0.02", "v0.2.00", "v0.2-rc1", "v0.2\n", "v0.2;touch /tmp/no"):
        rejects(release.version_from_tag, tag)
    event = {"action": "published", "release": {"id": 42, "tag_name": "v0.2", "draft": False,
             "prerelease": False, "target_commitish": "main"}}
    assert release.release_event(event)[1] == "0.2.0"
    for field, value in (("draft", True), ("prerelease", True), ("target_commitish", "development"), ("id", -1)):
        rejected = copy.deepcopy(event)
        rejected["release"][field] = value
        rejects(release.release_event, rejected)
    rejected = copy.deepcopy(event)
    rejected["action"] = "created"
    rejects(release.release_event, rejected)
    for expression in ("MIT", "MIT OR Apache-2.0", "(MIT AND BSD-3-Clause) OR Unknown-1.0",
                       "MIT/Apache-2.0", "Apache-2.0 WITH LLVM-exception"):
        assert licenses.accepted_expression(expression)
    for expression in ("Unknown-1.0", "MIT AND GPL-3.0-only"):
        assert not licenses.accepted_expression(expression)
    for expression in ("MIT OR", "(MIT", "MIT garbage", "MIT | ISC", ""):
        rejects(licenses.accepted_expression, expression)
    assert not licenses.accepted_expression("MIT WITH LLVM-exception")
    metadata = {"workspace_members": ["app"], "resolve": {"nodes": [
        {"id": "app", "deps": [{"pkg": "dep", "dep_kinds": [{"kind": None}]},
                                {"pkg": "dev", "dep_kinds": [{"kind": "dev"}]}]},
        {"id": "dep", "deps": []}, {"id": "dev", "deps": []}]},
        "packages": [{"id": name, "name": name, "version": "1.0.0"} for name in ("app", "dep", "dev")]}
    assert [p["id"] for p in licenses.resolved_packages(metadata, "app v1.0.0\n\ndep v1.0.0\n")] == ["app", "dep"]
    rejects(licenses.resolved_packages, metadata, "dep v1.0.0\n")
    duplicates = {"workspace_members": ["pinned"], "packages": [
        {"id": "pinned", "name": "derive", "version": "1.0.0",
         "source": "git+https://example.invalid/lib?rev=" + "a" * 40 + "#" + "a" * 40},
        {"id": "floating", "name": "derive", "version": "1.0.0",
         "source": "git+https://example.invalid/lib#" + "a" * 40}]}
    tree = "derive v1.0.0 (proc-macro) (https://example.invalid/lib?rev=" + "a" * 40 + "#aaaaaaaa)\n"
    assert [p["id"] for p in licenses.resolved_packages(duplicates, tree)] == ["pinned"]
    with tempfile.TemporaryDirectory(prefix="cosmos-packaging-fixture-") as temporary:
        root = Path(temporary)
        source = root / "source"
        # Publication commit A must reject a tag and checkout both moved to B.
        repository = root / "repository"
        repository.mkdir()
        def git(*arguments):
            return subprocess.check_output(
                ["git", "-c", "user.name=Release fixture", "-c", "user.email=fixture@example.invalid",
                 "-c", "core.hooksPath=/dev/null", "-c", "commit.gpgSign=false", "-c", "tag.gpgSign=false",
                 *arguments], cwd=repository, text=True).strip()
        git("init", "--quiet")
        git("commit", "--quiet", "--allow-empty", "-m", "Publication")
        published = git("rev-parse", "HEAD")
        git("tag", "v0.2")
        git("commit", "--quiet", "--allow-empty", "-m", "Later commit")
        moved = git("rev-parse", "HEAD")
        git("tag", "--force", "v0.2")
        git("update-ref", "refs/remotes/origin/main", moved)
        (repository / "crates/fixture").mkdir(parents=True)
        (repository / "crates/fixture/Cargo.toml").write_text("[package]\nversion='0.2.0'\n")
        (repository / "Cargo.lock").write_text("[[package]]\nname='cosmos-control'\nversion='0.2.0'\n")
        publication = root / "publication.json"
        publication.write_text(json.dumps(event))
        previous = Path.cwd()
        try:
            os.chdir(repository)
            with patch.object(release, "ROOT", repository), patch.dict(os.environ, {
                    "GITHUB_EVENT_NAME": "release", "GITHUB_EVENT_PATH": str(publication), "GITHUB_SHA": published}):
                rejects(release.gate)
                with patch.dict(os.environ, {"GITHUB_SHA": moved}):
                    assert release.gate()[2] == moved
                git("checkout", "--quiet", published)
                git("tag", "--force", "v0.2")
                assert release.gate()[2] == published
        finally:
            os.chdir(previous)
        source.mkdir()
        (source / ".git").mkdir()
        (source / "Cargo.toml").write_text("[package]\nname='fixture'\n")
        text = "Fixture source license text; this is NOT a real app distribution.\n"
        (source / "LICENSE").write_text(text)
        package = {"id": "fixture", "name": "fixture", "version": "1.0.0", "source": None,
                   "license": "MIT", "license_file": None, "manifest_path": str(source / "Cargo.toml")}
        graph = {"workspace_members": ["fixture"], "resolve": {"nodes": [{"id": "fixture", "deps": []}]},
                 "packages": [package]}
        def cargo_output(command, **kwargs):
            return json.dumps(graph).encode() if command[1] == "metadata" else "fixture v1.0.0\n"

        with patch.object(licenses.subprocess, "check_output", side_effect=cargo_output):
            document = licenses.audit(root / "notices")
        hashed = hashlib.sha256(text.encode()).hexdigest()
        assert document["packages"][0]["license_texts"][0]["sha256"] == hashed
        assert (root / f"notices/LICENSES/{hashed}.txt").read_text() == text
        assert text in (root / "notices/THIRD-PARTY-NOTICES.txt").read_text()
        package["license"] = None
        with patch.object(licenses.subprocess, "check_output", side_effect=cargo_output):
            rejects(licenses.audit, root / "rejected")
        record = {"source": "workspace", "manifest_sha256": hashlib.sha256((source / "Cargo.toml").read_bytes()).hexdigest(),
                  "texts": []}
        assert licenses.clarification(package, {"fixture@1.0.0": record})[0] == record
        record["source"] = "registry+wrong"
        rejects(licenses.clarification, package, {"fixture@1.0.0": record})
        record["source"] = "workspace"
        record["manifest_sha256"] = "0" * 64
        rejects(licenses.clarification, package, {"fixture@1.0.0": record})
        (source / "LICENSE").unlink()
        rejects(licenses.source_licenses, package)
        directory = root / "dist"
        directory.mkdir()
        names = {"cosmos-virtual-mic-0.2.0-linux-x86_64.tar.gz", "cosmos-virtual-mic-0.2.0-1.x86_64.rpm",
                 "cosmos-virtual-mic_0.2.0_amd64.deb", "install-release.sh"}
        for name in names:
            (directory / name).write_bytes(f"Non-package fixture: {name}".encode())
        release.checksum_manifest(directory)
        event_path = root / "event.json"
        event_path.write_text(json.dumps(event))
        current = dict(event["release"], published_at="2026-11-04T00:00:00Z",
                       upload_url="https://uploads.github.com/repos/petterssonjonas/cosmos-virtual-mic/releases/42/assets{?name,label}")
        remote = {}
        posted = []

        def urlopen(request, timeout=None):
            if isinstance(request, str):
                name = request.rsplit("/", 1)[-1]
                return io.BytesIO(remote[name])
            url = request.full_url
            if request.data is not None:
                name = upload.urllib.parse.parse_qs(upload.urllib.parse.urlparse(url).query)["name"][0]
                remote[name] = request.data.read()
                posted.append(name)
                result = {"name": name, "size": len(remote[name]), "state": "uploaded",
                          "digest": "sha256:" + hashlib.sha256(remote[name]).hexdigest()}
            elif "/assets?" in url:
                result = [{"name": name, "size": len(content), "state": "uploaded",
                           "browser_download_url": "https://github.com/fixture/" + name}
                          for name, content in remote.items()]
            elif "/git/ref/tags/" in url:
                result = {"object": {"type": "commit", "sha": "a" * 40}}
            else:
                result = current
            return io.BytesIO(json.dumps(result).encode())

        previous = Path.cwd()
        try:
            os.chdir(root)
            with patch.dict(os.environ, {"GITHUB_EVENT_NAME": "release", "GITHUB_EVENT_PATH": str(event_path),
                                         "GITHUB_REPOSITORY": "petterssonjonas/cosmos-virtual-mic", "GH_TOKEN": "fixture-not-a-token"}), \
                    patch.object(upload.urllib.request, "urlopen", side_effect=urlopen), \
                    patch.object(upload, "capture", return_value="a" * 40):
                upload.upload()
                assert posted[-1] == "SHA256SUMS" and len(posted) == 5
                posted.clear()
                upload.upload()
                assert not posted, "Rerun must preserve matching immutable assets"
                remote.pop("install-release.sh")
                upload.upload()
                assert posted == ["install-release.sh"]
                posted.clear()
                remote["install-release.sh"] = b"conflicting bytes"
                remote.pop("cosmos-virtual-mic_0.2.0_amd64.deb")
                rejects(upload.upload)
                assert not posted, "All collisions must be checked before any upload"
                with patch.object(upload, "capture", return_value="b" * 40):
                    rejects(upload.upload)
                assert not posted, "A moved tag must fail before uploads"
        finally:
            os.chdir(previous)
    print("Packaging policy/license/immutable-upload fixtures passed; no distribution packages built.")


if __name__ == "__main__":
    main()
