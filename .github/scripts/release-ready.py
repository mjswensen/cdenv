#!/usr/bin/env python3
"""Validate release identity and assemble an evidence-backed, unpublished candidate."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parents[2]
PLATFORMS = ("linux-x86_64", "linux-aarch64", "macos-aarch64")
GATES = {
    "quality", "security-mutations", "workspace-coverage", "linux-release-gates",
    "coverage-index", "macos-agent-stub", "linux-agent-targets", "linux-agents",
}
SEMVER = re.compile(
    r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)"
    r"(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
    r"(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?"
)


def write_json(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def identity(root, ref):
    version = tomllib.loads((root / "Cargo.toml").read_text())["workspace"]["package"]["version"]
    match = SEMVER.fullmatch(version)
    if not match or any(part.isdigit() and len(part) > 1 and part.startswith("0")
                        for part in (match[4] or "").split(".")):
        raise ValueError("workspace version must be valid SemVer")
    manifest = tomllib.loads((root / "Cargo.toml").read_text())
    packages = []
    for member in manifest["workspace"]["members"]:
        package = tomllib.loads((root / member / "Cargo.toml").read_text())["package"]
        if package["version"] != {"workspace": True}:
            raise ValueError(f"{member} must inherit the workspace version")
        packages.append(package["name"])
    lock = tomllib.loads((root / "Cargo.lock").read_text())
    for name in packages:
        entries = [p for p in lock["package"] if p["name"] == name and "source" not in p]
        if len(entries) != 1 or entries[0]["version"] != version:
            raise ValueError(f"Cargo.lock workspace version mismatch: {name}")
    for name, dependency in manifest["workspace"]["dependencies"].items():
        if "path" in dependency and dependency.get("version") != version:
            raise ValueError(f"workspace dependency version mismatch: {name}")
    if ref.startswith("refs/tags/") and ref != f"refs/tags/v{version}":
        raise ValueError(f"release tag must be v{version}, got {ref}")
    return {"version": version, "prerelease": bool(match[4])}


def gate_record(results, sha):
    if not re.fullmatch(r"[0-9a-f]{40}", sha):
        raise ValueError("expected full release commit SHA")
    success = set(results) == GATES and all(v.get("result") == "success" for v in results.values())
    return {"commit": sha, "success": success, "jobs": results}


def package_record(platform, sha):
    if platform not in PLATFORMS:
        raise ValueError("unsupported package platform")
    return {"commit": sha, "platform": platform, "packageSmoke": "passed",
            "reproducibility": "passed",
            "installedSmoke": "passed" if platform.startswith("linux-") else "manual-required"}


def bundle(source, evidence, destination, root, environment):
    release = identity(root, environment["GITHUB_REF"])
    sha = environment["GITHUB_SHA"]
    tag = f"v{release['version']}"
    if environment["GITHUB_REF"] != f"refs/tags/{tag}":
        raise ValueError("only a version tag can prepare a release")
    gates = json.loads((evidence / "ci-gate-results/ci-gate-results.json").read_text())
    if gates != gate_record(gates["jobs"], sha) or not gates["success"]:
        raise ValueError("missing successful CI gates for this exact commit")
    index = json.loads((evidence / "coverage-index/index.json").read_text())
    expected_reports = {f"workspace-{p}" for p in ("linux-x86_64", "linux-arm64", "macos-arm64")}
    expected_reports |= {f"{suite}-linux-{arch}-{baseline}"
                         for suite in ("credentials", "devcontainer-v1", "openssh")
                         for arch in ("x86_64", "arm64") for baseline in ("minimum", "pinned")}
    if index["errors"] or set(index["reports"]) != expected_reports:
        raise ValueError("incomplete or failed coverage/release-suite inventory")
    for shard in ("reference-lock", "transport", "archive-integrity"):
        gate = json.loads((evidence / f"security-mutations-linux-arm64-{shard}/gate.json").read_text())
        if (gate.get("success") is not True or gate.get("source_unchanged") is not True
                or gate.get("source_revision") != sha):
            raise ValueError(f"invalid mutation evidence for {shard}")
    # Validate the complete input inventory before writing or overwriting anything.
    packages = []
    files = []
    agents = None
    installer = (root / "install.sh").read_bytes()
    if {p.name for p in source.iterdir()} != {f"cdenv-{p}" for p in PLATFORMS}:
        raise ValueError("expected exactly three native package artifacts")
    for platform in PLATFORMS:
        directory = source / f"cdenv-{platform}"
        name = f"cdenv-{platform}-{sha}.tar"
        expected = {name, name + ".sha256", name + ".json", "install.sh"}
        if {p.name for p in directory.iterdir()} != expected:
            raise ValueError(f"unexpected package inventory for {platform}")
        if (directory / "install.sh").read_bytes() != installer:
            raise ValueError(f"installer differs from tagged source: {platform}")
        archive = directory / name
        checksum = digest(archive)
        if (directory / (name + ".sha256")).read_text() != f"{checksum}  {name}\n":
            raise ValueError(f"checksum mismatch for {name}")
        metadata = json.loads((directory / (name + ".json")).read_text())
        artifacts = metadata["artifacts"]
        if (metadata["platform"] != platform or artifacts["version"] != release["version"]
                or artifacts["buildId"] != sha or artifacts["protocolVersion"] != 1
                or artifacts["schemaVersion"] != 1):
            raise ValueError(f"package identity mismatch for {name}")
        hashes = artifacts["agents"]
        if set(hashes) != {"aarch64", "x86_64"} or any(
                not re.fullmatch(r"[0-9a-f]{64}", h) for h in hashes.values()):
            raise ValueError("invalid embedded agent hashes")
        if agents is not None and agents != hashes:
            raise ValueError("hosts embed different agents")
        agents = hashes
        report = json.loads((evidence / f"package-evidence-{platform}/package.json").read_text())
        if report != package_record(platform, sha):
            raise ValueError(f"missing completed package gates for {platform}")
        logs = ["build.log", "rebuild.log", "package-smoke.log"]
        if platform.startswith("linux-"):
            logs.append("installed-smoke.log")
        for filename in logs:
            if (evidence / f"package-evidence-{platform}" / filename).stat().st_size == 0:
                raise ValueError(f"empty package evidence log: {platform}/{filename}")
        packages.append({"platform": platform, "archive": name, "sha256": checksum,
                         "gates": report})
        files += [directory / filename for filename in sorted(expected - {"install.sh"})]
    if destination.exists() and any(destination.iterdir()):
        raise ValueError("release destination must be empty")
    destination.mkdir(parents=True, exist_ok=True)
    for path in files:
        shutil.copyfile(path, destination / path.name)
    shutil.copyfile(root / "install.sh", destination / "install.sh")
    shutil.copyfile(root / "tests/release/manual-macos-smoke.py", destination / "manual-macos-smoke.py")
    shutil.copyfile(root / "docs/smoke/macos-docker-desktop-v1.md", destination / "macos-docker-desktop-v1.md")
    with tarfile.open(destination / "release-evidence.tar.gz", "w:gz") as archive:
        archive.add(evidence, arcname="evidence")
    run_url = (f"{environment['GITHUB_SERVER_URL']}/{environment['GITHUB_REPOSITORY']}/actions/runs/"
               f"{environment['GITHUB_RUN_ID']}/attempts/{environment['GITHUB_RUN_ATTEMPT']}")
    notes = f"""# Release review: {tag}

Commit: `{sha}`  
CI and package run: {run_url}

All required CI jobs and three native package gates passed. Linux installed smoke
passed on both architectures. Full coverage, ordinary release-suite, mutation,
and package evidence is attached in `release-evidence.tar.gz`.
The release suites rebuild from the same commit; installed package smoke tests
the actual release archives. macOS package smoke does not verify Docker Desktop.

## Maintainer handoff — publication remains manual

1. On Apple Silicon with Docker Desktop, authenticate `gh` as a maintainer.
2. From a checkout of this exact tag, run:

   ```sh
   python3 tests/release/manual-macos-smoke.py {tag} --repo {environment['GITHUB_REPOSITORY']}
   ```

3. Follow the generated instructions, complete every required checklist field and
   step, and retain redacted logs. Native keychain and hardware-confirmation
   observations must distinguish real observations from simulations.
4. Attach the completed record and reviewed logs to this draft. If any required
   observation fails or is missing, leave the release in draft.
5. Publish after review; mark release candidates as prereleases. Check the public
   installer against this tag after publication. Stable releases can become latest.

**Manual macOS runtime evidence: PENDING. This candidate is not approved for publication.**
"""
    (destination / "release-review.md").write_text(notes)
    manifest = {"schemaVersion": 1, "tag": tag, "commit": sha, **release,
                "runUrl": run_url, "runAttempt": environment["GITHUB_RUN_ATTEMPT"],
                "ci": gates, "packages": packages, "agents": agents,
                "manualMacosSmoke": "pending",
                "assets": {p.name: digest(p) for p in sorted(destination.iterdir())}}
    write_json(destination / "release-manifest.json", manifest)
    if summary := environment.get("GITHUB_STEP_SUMMARY"):
        with open(summary, "a") as output:
            output.write(notes)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("identity", "gates", "package", "bundle"))
    parser.add_argument("paths", nargs="*", type=Path)
    args = parser.parse_args()
    if args.command == "identity":
        result = identity(ROOT, os.environ.get("GITHUB_REF", ""))
        if output := os.environ.get("GITHUB_OUTPUT"):
            with open(output, "a") as stream:
                stream.write(f"prerelease={str(result['prerelease']).lower()}\n")
        print(json.dumps(result))
    elif args.command == "gates":
        record = gate_record(json.loads(os.environ["GATE_RESULTS"]), os.environ["GITHUB_SHA"])
        write_json(args.paths[0], record)
        if not record["success"]:
            raise ValueError("all required CI jobs must succeed; missing/skipped jobs fail")
    elif args.command == "package":
        write_json(args.paths[0], package_record(os.environ["PACKAGE_PLATFORM"], os.environ["GITHUB_SHA"]))
    else:
        bundle(*args.paths, ROOT, os.environ)


if __name__ == "__main__":
    main()
