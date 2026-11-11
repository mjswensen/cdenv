#!/usr/bin/env python3
"""Prepare an exact draft release for manual Apple Silicon smoke testing.

Requires Python 3.11+, authenticated gh, Git, Docker Desktop, and OpenSSH.
Optional --run-basics creates disposable fixture environments and runs the
noninteractive baseline. The complete checklist and publication remain manual.
"""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shlex
import shutil
import subprocess
import tarfile
import tempfile


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def run(arguments, *, log=None, cwd=None):
    result = subprocess.run(arguments, cwd=cwd, text=True, stdout=subprocess.PIPE,
                            stderr=subprocess.PIPE, check=False, timeout=600)
    if log:
        with log.open("a") as stream:
            stream.write(f"$ {shlex.join(map(str, arguments))}\n")
            stream.write(result.stdout + result.stderr)
            stream.write(f"\nexit: {result.returncode}\n")
    result.check_returncode()
    return result.stdout.strip()


def extract_binary(archive, destination):
    # Never extract arbitrary members/links from a downloaded archive.
    with tarfile.open(archive, "r:") as contents:
        members = contents.getmembers()
        if len(members) != 1 or members[0].name != "cdenv" or not members[0].isfile():
            raise ValueError("expected exactly one regular cdenv executable")
        with contents.extractfile(members[0]) as source, destination.open("xb") as target:
            shutil.copyfileobj(source, target)
    destination.chmod(0o700)


def verify_downloads(directory, manifest, tag):
    sha = manifest["commit"]
    if (manifest["schemaVersion"] != 1 or manifest["tag"] != tag
            or tag != "v" + manifest["version"] or not re.fullmatch(r"[0-9a-f]{40}", sha)
            or manifest["ci"]["commit"] != sha or manifest["ci"]["success"] is not True):
        raise ValueError("release manifest identity/CI mismatch")
    name = f"cdenv-macos-aarch64-{sha}.tar"
    for filename in (name, name + ".sha256", name + ".json", "macos-docker-desktop-v1.md"):
        if digest(directory / filename) != manifest["assets"][filename]:
            raise ValueError(f"manifest checksum mismatch: {filename}")
    checksum = digest(directory / name)
    if (directory / (name + ".sha256")).read_text() != f"{checksum}  {name}\n":
        raise ValueError("adjacent checksum mismatch")
    metadata = json.loads((directory / (name + ".json")).read_text())
    if (metadata["platform"] != "macos-aarch64"
            or metadata["artifacts"]["buildId"] != sha
            or metadata["artifacts"]["version"] != manifest["version"]
            or metadata["artifacts"]["agents"] != manifest["agents"]):
        raise ValueError("macOS package metadata mismatch")
    return directory / name


def fixtures(directory):
    configs = {
        "image": {"image": "alpine:3.22"},
        "feature": {"build": {"dockerfile": "Dockerfile"}, "features": {"./feature": {}}},
        "compose": {"dockerComposeFile": "compose.yaml", "service": "app",
                    "runServices": ["app", "db"], "workspaceFolder": "/workspaces/compose"},
    }
    for name, config in configs.items():
        repository = directory / name
        repository.mkdir(parents=True)
        (repository / ".devcontainer.json").write_text(json.dumps(config, indent=2) + "\n")
        (repository / "tracked.txt").write_text("tracked baseline\n")
        if name == "feature":
            (repository / "Dockerfile").write_text("FROM debian:13-slim\n")
            feature = repository / "feature"
            feature.mkdir()
            (feature / "devcontainer-feature.json").write_text(json.dumps({
                "id": "marker", "version": "1.0.0", "name": "Release smoke marker",
            }) + "\n")
            install = feature / "install.sh"
            install.write_text("#!/bin/sh\nset -eu\nprintf marker > /usr/local/share/cdenv-smoke-marker\n")
            install.chmod(0o755)
        if name == "compose":
            (repository / "compose.yaml").write_text("""services:
  app:
    image: debian:13-slim
    command: [sleep, infinity]
    volumes:
      - smoke-data:/data
  db:
    image: debian:13-slim
    command: [sleep, infinity]
volumes:
  smoke-data:
""")
        for arguments in (["init", "--quiet"], ["config", "user.name", "Release smoke"],
                          ["config", "user.email", "smoke@example.invalid"], ["add", "."],
                          ["commit", "--quiet", "-m", "Release smoke fixture"]):
            run(["git", "-C", str(repository), *arguments])


def basics(binary, root, repositories, log):
    def cdenv(*arguments):
        return run([str(binary), "--root", str(root), "--no-modify-ssh-config", *arguments], log=log)

    # Do not accidentally create fixtures against a remote daemon or another VM.
    endpoint = os.environ.get("DOCKER_HOST") if not os.environ.get("DOCKER_CONTEXT") else None
    if not endpoint:
        endpoint = run(["docker", "context", "inspect", "--format", "{{.Endpoints.docker.Host}}"], log=log)
    if not endpoint.startswith("unix://"):
        raise ValueError("basic smoke requires a local Docker Desktop Unix socket")
    if run(["docker", "info", "--format", "{{.OperatingSystem}}"], log=log) != "Docker Desktop":
        raise ValueError("basic smoke must exercise Docker Desktop")
    cdenv("doctor")
    # Keep all three environments for the subsequent interactive checks.
    for name in ("image", "feature", "compose"):
        cdenv("create", str(repositories / name), "--name", name)
        cdenv("status", name)
        cdenv("status", name, "--json")
    cdenv("list")
    cdenv("list", "--json")
    cdenv("doctor")
    cdenv("doctor", "--json")
    if cdenv("ssh", "image", "--", "printf", "smoke-ssh") != "smoke-ssh":
        raise ValueError("non-PTY SSH output mismatch")
    if run(["ssh", "-F", str(root / "ssh/config"), "image.cdenv", "printf", "direct-ssh"], log=log) != "direct-ssh":
        raise ValueError("direct OpenSSH output mismatch")
    checkout = root / "workspaces/image/checkout/image"
    (checkout / "tracked.txt").write_text("tracked change\n")
    (checkout / "untracked.txt").write_text("untracked change\n")
    for arguments in (("down", "image"), ("up", "image"), ("rebuild", "image"),
                      ("rebuild", "image", "--no-cache")):
        cdenv(*arguments)
        if ((checkout / "tracked.txt").read_text() != "tracked change\n"
                or (checkout / "untracked.txt").read_text() != "untracked change\n"):
            raise ValueError("source changes did not survive lifecycle operation")
    if cdenv("ssh", "feature", "--", "cat", "/usr/local/share/cdenv-smoke-marker") != "marker":
        raise ValueError("local Feature did not install")
    cdenv("lock", "feature")
    locks = sorted(root.glob("workspaces/feature/checkout/feature/**/devcontainer-lock.json"))
    if len(locks) != 1:
        raise ValueError("expected one Feature lockfile")
    before = locks[0].read_bytes()
    cdenv("lock", "feature")
    if before != locks[0].read_bytes():
        raise ValueError("second lock invocation was not stable")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("--repo", default="mjswensen/cdenv")
    parser.add_argument("--run-basics", action="store_true")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("run on an Apple Silicon Mac with local Docker Desktop")
    if not re.fullmatch(r"v[0-9][0-9A-Za-z.+-]*", args.tag):
        parser.error("expected a v-prefixed version tag")
    if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", args.repo):
        parser.error("expected owner/repository")
    os.umask(0o077)
    directory = Path(tempfile.mkdtemp(prefix="cdm-", dir="/private/tmp"))
    print(f"Private working directory (retained on failure): {directory}", flush=True)
    assets = directory / "assets"
    assets.mkdir()
    run(["gh", "release", "download", args.tag, "--repo", args.repo, "--dir", str(assets),
         "--pattern", "release-manifest.json"])
    manifest = json.loads((assets / "release-manifest.json").read_text())
    # Validate identity before using the manifest's commit in a download pattern.
    if manifest.get("tag") != args.tag or not re.fullmatch(r"[0-9a-f]{40}", manifest.get("commit", "")):
        raise ValueError("release manifest identity mismatch")
    name = f"cdenv-macos-aarch64-{manifest['commit']}.tar"
    for filename in (name, name + ".sha256", name + ".json", "macos-docker-desktop-v1.md"):
        run(["gh", "release", "download", args.tag, "--repo", args.repo, "--dir", str(assets),
             "--pattern", filename])
    archive = verify_downloads(assets, manifest, args.tag)
    install = directory / "bin"
    install.mkdir()
    binary = install / "cdenv"
    extract_binary(archive, binary)
    log = directory / "baseline.log"
    if run([str(binary), "--version"], log=log) != f"cdenv {manifest['version']}":
        raise ValueError("installed version mismatch")
    artifacts = json.loads(run([str(binary), "__validate-artifacts"], log=log))
    if (artifacts["buildId"] != manifest["commit"] or artifacts["agents"] != manifest["agents"]
            or artifacts["version"] != manifest["version"] or artifacts["protocolVersion"] != 1):
        raise ValueError("installed embedded-agent identity mismatch")
    root = directory / "root"
    repositories = directory / "repositories"
    fixtures(repositories)
    environment = directory / "environment.sh"
    environment.write_text(f"""# Source in bash or zsh. Uses only this candidate and private root.
export CDENV_HOME={shlex.quote(str(root))}
export PATH={shlex.quote(str(install))}:"$PATH"
export CDENV_SMOKE_REPOSITORIES={shlex.quote(str(repositories))}
cdenv() {{ command cdenv --root "$CDENV_HOME" --no-modify-ssh-config "$@"; }}
""")
    record = (assets / "macos-docker-desktop-v1.md").read_text()
    observations = {
        "Release/date (UTC)": f"{args.tag} / {datetime.datetime.now(datetime.timezone.utc).isoformat()}",
        "Tester": os.environ.get("USER", "REQUIRED"),
        "Host architecture (`uname -m`, must be `arm64`)": "arm64",
        "cdenv archive filename": name,
        "cdenv archive SHA-256": digest(archive),
        "Published checksum verified": "pass (candidate adjacent checksum and manifest)",
        "Overall outcome": "PENDING — complete all required observations",
    }
    probes = {
        "Mac model/hardware": ["sysctl", "-n", "hw.model"],
        "macOS version/build (`sw_vers`)": ["sw_vers"],
        "Docker Desktop version": ["defaults", "read", "/Applications/Docker.app/Contents/Info", "CFBundleShortVersionString"],
        "Docker Engine/API version": ["docker", "version", "--format", "{{.Server.Version}} / {{.Server.APIVersion}}"],
        "Docker CLI version": ["docker", "--version"],
        "Compose V2 version": ["docker", "compose", "version"],
    }
    for field, command in probes.items():
        try:
            observations[field] = run(command, log=log).replace("\n", "; ")
        except (OSError, subprocess.CalledProcessError):
            observations[field] = "REQUIRED — probe failed; resolve before testing"
    ssh_version = subprocess.run(["ssh", "-V"], text=True, capture_output=True, check=True)
    observations["OpenSSH version"] = (ssh_version.stdout + ssh_version.stderr).strip()
    for field, value in observations.items():
        # Escape table delimiters in version strings and other observations.
        value = value.replace("|", "\\|")
        record = re.sub(r"^\| " + re.escape(field) + r" \|.*$",
                        lambda _: f"| {field} | {value} |", record, flags=re.MULTILINE)
    record += "\n## Candidate identity\n\n" + f"Commit: `{manifest['commit']}`\n\nCI: {manifest['runUrl']}\n"
    record += "\n## Manual results — fill every row, with redacted evidence\n\n"
    record += "| Required step | Outcome | Evidence / real vs simulated observations |\n|---|---|---|\n"
    record += "".join(f"| {step} | PENDING | |\n" for step in range(1, 13))
    (directory / "smoke-record.md").write_text(record)
    instructions = f"""# Manual candidate smoke

Candidate: {args.tag}; commit: {manifest['commit']}
The overall result is PENDING. Preparation/basic tests never approve a release.

1. In a fresh terminal: source {environment}
2. Read smoke-record.md and verify Docker uses the local Docker Desktop Unix
   socket/context, all dependency baselines pass, and all required fields are set.
3. If --run-basics was omitted, create the fixtures:
   cdenv create "$CDENV_SMOKE_REPOSITORIES/image" --name image
   cdenv create "$CDENV_SMOKE_REPOSITORIES/feature" --name feature
   cdenv create "$CDENV_SMOKE_REPOSITORIES/compose" --name compose
4. Complete ALL twelve steps in smoke-record.md. The optional baseline covers
   only package identity, basic fixture creation/status/doctor, non-PTY SSH,
   image source preservation over lifecycle operations, local Feature install,
   and repeated lock stability. It does NOT cover stale/tampered locks, PTY
   resize/Ctrl+C, forwarding, Compose isolation/volume preservation, interrupted
   recovery, keychain/agent permissions, secret scans, or full cleanup.
5. For interactive SSH, start with: cdenv ssh image
   Check Ctrl+C and resize, then exit. Record stderr and nonzero exit propagation
   separately. Follow checklist steps 9–10 with controlled credentials and a
   controlled host agent; record real vs simulated provider/hardware behavior.
6. Before cleanup, finish inspection and fixture-secret scans. Record findings
   without copying secret-bearing payloads into your logs. Review baseline.log
   and every manually captured log before attaching anything.
7. Stop only these fixtures:
   cdenv down image
   cdenv down feature
   cdenv down compose
   Follow the checklist's retention/uninstall procedure. The work directory,
   containers and named volumes are deliberately retained for inspection; do
   scoped cleanup after testing. Do not prune unrelated Docker resources.
8. Complete all fields, the twelve result rows, and overall outcome. Only a
   complete passing record can support publication. Upload reviewed files:
   gh release upload {args.tag} {directory}/smoke-record.md --repo {args.repo}
   # Add explicitly reviewed, redacted logs as additional asset paths.
9. Review the draft and generated notes, then publish manually in GitHub's UI.
   Keep release candidates marked prerelease. After publication, install the
   exact tag into a temporary directory using CDENV_VERSION and CDENV_INSTALL_DIR
   (see docs/release-packaging.md). Test the README latest URL for stable releases.

This script never uploads evidence or publishes automatically.
"""
    (directory / "MANUAL.md").write_text(instructions)
    print(f"Candidate verified. Source {environment}")
    print(f"Follow {directory / 'MANUAL.md'}; complete {directory / 'smoke-record.md'}.")
    if args.run_basics:
        basics(binary, root, repositories, log)
        with log.open("a") as stream:
            stream.write("\nBasic noninteractive smoke passed. Full checklist remains PENDING.\n")
        print("Basic smoke passed; environments retained. Full checklist remains PENDING.")


if __name__ == "__main__":
    main()
