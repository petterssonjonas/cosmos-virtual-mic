#!/usr/bin/env python3
# SPDX-License-Identifier: MPL-2.0
"""Fail-closed source-license inventory for the Linux normal/build dependency graph.

This is a license-policy audit, NOT a vulnerability or legal-compliance audit.
OR expressions select an accepted alternative; AND requires every obligation.
Only explicitly listed SPDX exceptions and hash-pinned clarifications are accepted.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import tomllib
import xml.etree.ElementTree as ET

TARGET = "x86_64-unknown-linux-gnu"
ACCEPTED = frozenset({
    "MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib",
    "MPL-2.0", "Unicode-3.0", "Unicode-DFS-2016", "CC0-1.0", "Unlicense",
    "BSL-1.0", "0BSD", "CC-BY-4.0", "OpenSSL",
})


ACCEPTED_EXCEPTIONS = frozenset({("Apache-2.0", "LLVM-exception")})


def accepted_expression(expression):
    """Parse SPDX plus Cargo's historical slash spelling for dual licensing."""
    expression = expression.replace("/", " OR ")
    tokens = re.findall(r"\(|\)|[A-Za-z0-9.+-]+", expression)
    if "".join(tokens) != re.sub(r"\s+", "", expression):
        raise ValueError(f"Malformed license expression: {expression!r}")
    position = 0

    def atom():
        nonlocal position
        if position >= len(tokens):
            raise ValueError("Incomplete license expression")
        token = tokens[position]
        position += 1
        if token == "(":
            result = either()
            if position >= len(tokens) or tokens[position] != ")":
                raise ValueError("Unbalanced license expression")
            position += 1
            return result
        if token in {"AND", "OR", "WITH", ")"}:
            raise ValueError(f"Unexpected license token: {token}")
        if position < len(tokens) and tokens[position] == "WITH":
            position += 1
            if position >= len(tokens):
                raise ValueError("Incomplete SPDX exception")
            exception = tokens[position]
            position += 1
            return (token, exception) in ACCEPTED_EXCEPTIONS
        return token in ACCEPTED

    def both():
        nonlocal position
        result = atom()
        while position < len(tokens) and tokens[position] == "AND":
            position += 1
            other = atom()
            result = result and other
        return result

    def either():
        nonlocal position
        result = both()
        while position < len(tokens) and tokens[position] == "OR":
            position += 1
            other = both()
            result = result or other
        return result

    result = either()
    if position != len(tokens):
        raise ValueError(f"Unsupported license expression: {expression!r}")
    return result


def resolved_packages(metadata, tree):
    # cargo metadata's global feature graph may include dev-induced edges.
    # cargo tree applies the exact build's target/workspace/normal+build selector.
    selected = set()
    for line in tree.splitlines():
        if not line or line == "[build-dependencies]":
            continue
        match = re.match(r"^(\S+) v(\S+)(?: \(proc-macro\))?(?: \(([^)]+)\))?", line)
        if not match:
            raise ValueError(f"Unrecognized cargo tree package: {line!r}")
        candidates = [package for package in metadata["packages"]
                      if (package["name"], package["version"]) == match.groups()[:2]]
        if len(candidates) > 1 and match[3] and match[3] != "*":
            hint = match[3]
            def matches(package):
                source = package.get("source")
                if not source:
                    return str(Path(package["manifest_path"]).parent) == hint
                url, _, revision = source.removeprefix("git+").partition("#")
                shown_url, _, shown_revision = hint.partition("#")
                return url == shown_url and bool(shown_revision) and revision.startswith(shown_revision)
            candidates = [package for package in candidates if matches(package)]
        if len(candidates) != 1:
            raise ValueError(f"Ambiguous or absent metadata identity: {line}")
        selected.add(candidates[0]["id"])
    if not selected or not set(metadata["workspace_members"]) <= selected:
        raise ValueError("Incomplete resolved workspace graph")
    return sorted((p for p in metadata["packages"] if p["id"] in selected), key=lambda p: p["id"])


def source_licenses(package, allow_missing=False):
    directory = Path(package["manifest_path"]).resolve().parent
    # Registry crates are self-contained. Git/workspace members may inherit the
    # repository's license texts, so walk only to their own checkout boundary.
    boundary = directory
    if not package["source"] or package["source"].startswith("git+"):
        for candidate in (directory, *directory.parents):
            boundary = candidate
            if (candidate / ".git").exists():
                break
        else:
            raise ValueError(f"No source checkout boundary: {directory}")
    found = set()
    for parent in (directory, *directory.parents):
        for path in parent.iterdir():
            if path.is_file() and re.match(r"^(licen[cs]e|copying|notice|copyright)(?:[._-].*)?$", path.name, re.I):
                found.add(path.resolve())
        # Common upstream layout (e.g. ring) puts additional source licenses here.
        for name in ("LICENSES", "licenses", "license"):
            child = parent / name
            if child.is_dir():
                found.update(p.resolve() for p in child.rglob("*") if p.is_file())
        if parent == boundary:
            break
    if (package["source"] or "").startswith("git+https://github.com/pop-os/cosmic-protocols"):
        found.add((boundary / "src/LICENSE").resolve())
    if package.get("license_file"):
        found.add((directory / package["license_file"]).resolve())
    for path in found:
        if not path.is_relative_to(boundary) or not path.is_file() or not path.stat().st_size:
            raise ValueError(f"Invalid or empty source license: {path}")
    if not found and not allow_missing:
        raise ValueError(f"Missing source license text: {package['id']}")
    return sorted(found)


def embedded_licenses(package):
    if not (package["source"] or "").startswith("git+https://github.com/pop-os/cosmic-protocols"):
        return []
    directory = Path(package["manifest_path"]).resolve().parent
    boundary = next((p for p in (directory, *directory.parents) if (p / ".git").exists()), None)
    if boundary is None:
        raise ValueError("No pinned protocol checkout boundary")
    result = []
    for path in sorted((boundary / "unstable").rglob("*.xml")):
        for node in ET.parse(path).getroot().iter("copyright"):
            text = "".join(node.itertext()).strip()
            if not text:
                raise ValueError(f"Empty embedded protocol license: {path}")
            result.append((f"{path.relative_to(boundary)} <copyright>", (text + "\n").encode()))
    if not result:
        raise ValueError("Missing embedded protocol copyright grants")
    return result


def clarification(package, records):
    record = records.get(f"{package['name']}@{package['version']}", {})
    if isinstance(record, list):
        matches = [item for item in record if item.get("source") == (package["source"] or "workspace")]
        if len(matches) != 1:
            raise ValueError("Missing or duplicate source-specific clarification")
        record = matches[0]
    if not record:
        return {}, []
    allowed = {"source", "manifest_sha256", "lock_source", "license", "texts"}
    if set(record) - allowed or not {"source", "manifest_sha256", "texts"} <= set(record):
        raise ValueError("Malformed license clarification fields")
    if record["source"] != (package["source"] or "workspace"):
        raise ValueError("Clarification source does not match resolved dependency")
    if record["manifest_sha256"] != hashlib.sha256(Path(package["manifest_path"]).read_bytes()).hexdigest():
        raise ValueError("Clarification manifest hash does not match pinned dependency")
    base = Path(__file__).resolve().parent
    if (package["source"] or "").startswith("git+"):
        lock = tomllib.loads((base.parent / "Cargo.lock").read_text())
        candidates = [p for p in lock["package"] if p["name"] == package["name"] and p["version"] == package["version"]
                      and p.get("source") == record.get("lock_source")
                      and p["source"].split("#")[0] == package["source"].split("#")[0]]
        if len(candidates) != 1 or candidates[0].get("source") != record.get("lock_source"):
            raise ValueError("Clarification does not match the locked Git revision")
        if not re.search(r"#[0-9a-f]{40}$", record["lock_source"]):
            raise ValueError("Clarification requires a full resolved Git revision")
    if record.get("license") and package.get("license") and record["license"] != package["license"]:
        raise ValueError("Clarification cannot replace a conflicting declared SPDX license")
    supplied = []
    for text in record["texts"]:
        if set(text) != {"path", "sha256", "source_url"}:
            raise ValueError("Malformed clarification text record")
        path = (base / text["path"]).resolve()
        if not path.is_relative_to(base / "license-texts") or not path.is_file() or not path.stat().st_size:
            raise ValueError("Clarification text must be a retained nonempty license-texts file")
        if not text["source_url"].startswith("https://") or text["sha256"] != hashlib.sha256(path.read_bytes()).hexdigest():
            raise ValueError("Clarification text provenance or hash is invalid")
        supplied.append(path)
    if record.get("license") and not supplied:
        raise ValueError("Missing-license clarification requires retained source evidence")
    return record, supplied


def audit(destination):
    metadata = json.loads(subprocess.check_output([
        "cargo", "metadata", "--locked", "--format-version", "1", "--filter-platform", TARGET,
    ]))
    tree = subprocess.check_output(["cargo", "tree", "--locked", "--workspace", "--target", TARGET,
                                    "--edges", "normal,build", "--prefix", "none", "--format", "{p}"], text=True)
    clarification_path = Path(__file__).resolve().parent / "license-clarifications.json"
    records = json.loads(clarification_path.read_text())["packages"] if clarification_path.exists() else {}
    destination.mkdir(parents=True, exist_ok=False)
    texts = destination / "LICENSES"
    texts.mkdir()
    inventory = []
    errors = []
    for package in resolved_packages(metadata, tree):
        try:
            evidence, supplied = clarification(package, records)
            expression = package.get("license") or evidence.get("license")
            if not expression or not accepted_expression(expression):
                raise ValueError(f"Missing or unaccepted SPDX license: {expression!r}")
            licenses = []
            sources = [(source.name, source.read_bytes())
                       for source in [*source_licenses(package, allow_missing=bool(supplied)), *supplied]]
            sources.extend(embedded_licenses(package))
            for source_name, content in sources:
                digest = hashlib.sha256(content).hexdigest()
                filename = f"LICENSES/{digest}.txt"
                (destination / filename).write_bytes(content)
                licenses.append({"source_path": source_name, "file": filename, "sha256": digest})
            inventory.append({"id": package["id"], "name": package["name"], "version": package["version"],
                              "source": package["source"] or "workspace", "license": expression,
                              "repository": package.get("repository"), "license_texts": licenses,
                              "clarification": evidence or None})
        except (ValueError, OSError) as error:
            errors.append(f"{package['id']}: {error}")
    if errors:
        raise SystemExit("License audit rejected release:\n" + "\n".join(errors))
    document = {"target": TARGET, "dependency_kinds": ["normal", "build"],
                "policy": {"accepted_spdx": sorted(ACCEPTED),
                           "exceptions": [f"{license} WITH {exception}" for license, exception in sorted(ACCEPTED_EXCEPTIONS)],
                           "note": "License-policy inventory only; no vulnerability audit performed."},
                "packages": inventory}
    (destination / "dependency-inventory.json").write_text(json.dumps(document, indent=2) + "\n")
    notices = ["Cosmos Virtual Mic — resolved dependency source licenses", "",
               "Includes Linux normal/build dependencies; system libraries are not bundled.",
               "All source license texts found at each dependency's source boundary are retained.",
               "SPDX declarations are audited against the explicit policy in dependency-inventory.json.",
               "OR permits an accepted alternative; AND retains all obligations. Only explicitly listed exceptions accepted.",
               "This inventory is not a vulnerability scan or a legal opinion.", ""]
    for package in inventory:
        notices.extend([f"{package['name']} {package['version']} — {package['license']}",
                        f"Source: {package['source']}", f"Repository: {package['repository'] or '(not declared)'}"])
        notices.extend(f"  {text['source_path']}: {text['file']}" for text in package["license_texts"])
        notices.append("")
    notices.extend(["Full source-license texts (deduplicated by SHA-256)", ""])
    for path in sorted(texts.iterdir()):
        content = path.read_bytes()
        try:
            text = content.decode("utf-8")
        except UnicodeDecodeError:
            # Preserve every byte's character value for older Latin-1 notices;
            # the exact original bytes are also shipped under their hash.
            text = content.decode("latin-1")
        notices.extend([f"===== LICENSES/{path.name} =====", text, ""])
    (destination / "THIRD-PARTY-NOTICES.txt").write_text("\n".join(notices))
    return document


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", type=Path, help="new directory to receive inventory and source license texts")
    audit(parser.parse_args().destination)
