#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Upload checked artifacts to the triggering Release; never delete or replace assets.

Existing assets must all match before any missing asset is uploaded. SHA256SUMS
is uploaded last as the completion marker. A failed partial upload can be resumed
with the identical checked artifact; a conflicting rebuild fails closed.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import urllib.parse
import urllib.request

from release import APP, capture, release_event


def digest_stream(stream):
    digest = hashlib.sha256()
    while block := stream.read(1024 * 1024):
        digest.update(block)
    return digest.hexdigest()


def upload():
    if os.environ.get("GITHUB_EVENT_NAME") != "release":
        raise ValueError("Uploads require the triggering published Release")
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text())
    expected_release, version = release_event(event)
    repository = os.environ["GITHUB_REPOSITORY"]
    if repository != "petterssonjonas/cosmos-virtual-mic":
        raise ValueError("Unexpected repository")
    token = os.environ["GH_TOKEN"]
    api = f"https://api.github.com/repos/{repository}"
    release_id = expected_release["id"]

    def request(url, data=None, headers=None):
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or parsed.hostname not in {"api.github.com", "uploads.github.com"}:
            raise ValueError("Untrusted GitHub API URL")
        request_headers = {"Authorization": f"Bearer {token}", "Accept": "application/vnd.github+json",
                           "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "cosmos-release"}
        request_headers.update(headers or {})
        with urllib.request.urlopen(urllib.request.Request(url, data=data, headers=request_headers), timeout=120) as response:
            return json.load(response)

    current = request(f"{api}/releases/{release_id}")
    if (current["id"] != release_id or current["tag_name"] != expected_release["tag_name"]
            or current["target_commitish"] != "main" or current["draft"] or current["prerelease"]
            or not current.get("published_at")):
        raise ValueError("Triggering Release no longer matches the checked publication")
    reference = request(f"{api}/git/ref/tags/{urllib.parse.quote(expected_release['tag_name'], safe='')}")["object"]
    for _ in range(10):
        if reference["type"] == "commit":
            break
        if reference["type"] != "tag" or not re.fullmatch(r"[0-9a-f]{40}", reference["sha"]):
            raise ValueError("Invalid Release tag object")
        reference = request(f"{api}/git/tags/{reference['sha']}")["object"]
    else:
        raise ValueError("Release annotated tag nesting exceeds safe limit")
    if reference["sha"] != capture("git", "rev-parse", "HEAD"):
        raise ValueError("Release tag moved after the checked build")
    directory = Path("dist")
    expected = {f"{APP}-{version}-linux-x86_64.tar.gz", f"{APP}-{version}-1.x86_64.rpm",
                f"{APP}_{version}_amd64.deb", "install-release.sh"}
    if {p.name for p in directory.iterdir()} != expected | {"SHA256SUMS"}:
        raise ValueError("Unexpected or missing release assets")
    checksums = {}
    for line in (directory / "SHA256SUMS").read_text().splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in checksums:
            raise ValueError("Invalid asset checksum manifest")
        checksums[match[2]] = match[1]
    if set(checksums) != expected:
        raise ValueError("Incomplete asset checksum manifest")
    checksums["SHA256SUMS"] = hashlib.sha256((directory / "SHA256SUMS").read_bytes()).hexdigest()
    for name, digest in checksums.items():
        path = directory / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"Nonregular release asset: {name}")
        with path.open("rb") as stream:
            if digest_stream(stream) != digest:
                raise ValueError(f"Unchecked or corrupted local release asset: {name}")
    assets = []
    page = 1
    while True:
        batch = request(f"{api}/releases/{release_id}/assets?per_page=100&page={page}")
        assets.extend(batch)
        if len(batch) < 100:
            break
        page += 1
    by_name = {}
    for asset in assets:
        if asset["name"] in by_name:
            raise ValueError("Duplicate existing Release asset")
        by_name[asset["name"]] = asset
    # Preflight ALL existing collisions before mutating the Release.
    for name, digest in checksums.items():
        if name not in by_name:
            continue
        asset = by_name[name]
        if asset["state"] != "uploaded" or asset["size"] != (directory / name).stat().st_size:
            raise ValueError(f"Existing asset is incomplete or different: {name}; nothing replaced")
        # Release is public. Download WITHOUT the write token, including redirects.
        url = asset["browser_download_url"]
        parsed = urllib.parse.urlparse(url)
        if parsed.scheme != "https" or parsed.hostname != "github.com":
            raise ValueError("Untrusted public asset URL")
        with urllib.request.urlopen(url, timeout=120) as stream:
            if digest_stream(stream) != digest:
                raise ValueError(f"Existing asset differs: {name}; nothing replaced")
    upload_url = current["upload_url"].split("{", 1)[0]
    for name in [*sorted(expected), "SHA256SUMS"]:
        if name in by_name:
            continue
        path = directory / name
        with path.open("rb") as stream:
            asset = request(f"{upload_url}?name={urllib.parse.quote(name)}", data=stream,
                            headers={"Content-Type": "application/octet-stream", "Content-Length": str(path.stat().st_size)})
        if asset["name"] != name or asset["size"] != path.stat().st_size or asset["state"] != "uploaded":
            raise ValueError(f"GitHub did not confirm the exact asset: {name}")
        remote_digest = asset.get("digest")
        if remote_digest and remote_digest != f"sha256:{checksums[name]}":
            raise ValueError(f"GitHub asset digest mismatch: {name}")
        if not remote_digest:
            url = asset["browser_download_url"]
            parsed = urllib.parse.urlparse(url)
            if parsed.scheme != "https" or parsed.hostname != "github.com":
                raise ValueError("Untrusted public asset URL")
            with urllib.request.urlopen(url, timeout=120) as stream:
                if digest_stream(stream) != checksums[name]:
                    raise ValueError(f"Uploaded asset failed public download verification: {name}")
    print(f"All checked assets present on triggering Release ID {release_id}; no assets replaced")


if __name__ == "__main__":
    upload()
