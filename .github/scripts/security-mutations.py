"""Fail-closed, isolated runner for the reviewed Feature security mutants."""

import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tarfile
import tempfile
import tomllib

MANIFEST = ".cargo/security-mutants.json"
BASELINE = ".cargo/security-mutants-baseline.json"
TRIAGE = ".cargo/security-mutants-triage.json"
CONFIG = ".cargo/security-mutants.toml"


def digest(data):
    return hashlib.sha256(data).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def git(root, *arguments):
    return subprocess.check_output(["git", "-C", str(root), *arguments])


def snapshot(root):
    """Hash source bytes, link targets, modes, HEAD and index/worktree status.

    Ignored build output and the separate issues worktree are not source inputs.
    Including nonignored untracked paths catches accidental new source files.
    """
    files = {}
    names = git(root, "ls-files", "-z", "--cached", "--others", "--exclude-standard")
    for name in sorted(set(names.split(b"\0")) - {b""}):
        path = root / os.fsdecode(name)
        mode = path.lstat().st_mode
        if stat.S_ISLNK(mode):
            content = os.fsencode(os.readlink(path))
        elif stat.S_ISREG(mode):
            content = path.read_bytes()
        else:
            raise ValueError(f"unsupported source entry: {path}")
        files[os.fsdecode(name)] = [mode, digest(content)]
    return {
        "head": git(root, "rev-parse", "HEAD").decode().strip(),
        "status": git(root, "status", "--porcelain=v1", "--untracked-files=all").decode(),
        "files": files,
    }


def validate_policy(manifest, baseline, triage, today):
    if manifest["schema"] != 1 or baseline["schema"] != 1:
        raise ValueError("unsupported mutation policy schema")
    if baseline["revision"] != manifest["baseline_revision"]:
        raise ValueError("baseline revision mismatch")
    targets = manifest["targets"]
    ids = [target["id"] for target in targets]
    names = [target["name"] for target in targets]
    if not targets or len(set(ids)) != len(ids) or len(set(names)) != len(names):
        raise ValueError("empty or duplicate target selection")
    if set(baseline["diff_sha256"]) != set(ids):
        raise ValueError("baseline must describe exactly the reviewed targets")
    if set(target["shard"] for target in targets) != set(manifest["shards"]):
        raise ValueError("empty or unknown shard")
    for target in targets:
        if not target["reason"].strip() or not target["test"].strip():
            raise ValueError("each target requires a reason and focused test")
        if not re.fullmatch(r"[0-9a-f]{64}", baseline["diff_sha256"][target["id"]]):
            raise ValueError("invalid baseline diff hash")
    for identifier, entry in triage.items():
        if identifier not in ids or set(entry) != {"owner", "expires", "reason"}:
            raise ValueError(f"unknown target or invalid triage: {identifier}")
        if not entry["owner"].strip() or not entry["reason"].strip():
            raise ValueError(f"triage requires owner and reason: {identifier}")
        if datetime.date.fromisoformat(entry["expires"]) <= today:
            raise ValueError(f"expired triage: {identifier}")


def validate_selection(selected, targets, baseline):
    expected = {target["name"]: target for target in targets}
    names = [mutant["name"] for mutant in selected]
    if not names or len(set(names)) != len(names) or set(names) != set(expected):
        raise ValueError("runner selection differs from reviewed nonempty target list")
    for mutant in selected:
        target = expected[mutant["name"]]
        if digest(mutant["diff"].encode()) != baseline["diff_sha256"][target["id"]]:
            raise ValueError(f"mutation diff drifted; review baseline: {target['id']}")


def test_names(log, outcome):
    return set(re.findall(r"^test (\S+) \.\.\. " + outcome + r"$", log, re.MULTILINE))


def evaluate(outcomes, directory, targets, triage, version, exit_code):
    if outcomes["cargo_mutants_version"] != version or not outcomes["end_time"]:
        raise ValueError("wrong runner version or incomplete run")
    if exit_code not in (0, 2):
        raise ValueError(f"runner failed: exit {exit_code}")
    scenarios = outcomes["outcomes"]
    baselines = [item for item in scenarios if item["scenario"] == "Baseline"]
    if len(baselines) != 1 or baselines[0]["summary"] != "Success":
        raise ValueError("unmutated baseline did not pass")
    baseline_log = (directory / baselines[0]["log_path"]).read_text()
    passed = test_names(baseline_log, "ok")
    if not passed or not {target["test"] for target in targets} <= passed:
        raise ValueError("baseline omitted or ignored a required focused test")
    by_name = {}
    for item in scenarios:
        if item["scenario"] == "Baseline":
            continue
        name = item["scenario"]["Mutant"]["name"]
        if name in by_name:
            raise ValueError("duplicate mutant outcome")
        by_name[name] = item
    if set(by_name) != {target["name"] for target in targets}:
        raise ValueError("missing or unexpected mutant outcomes")
    if outcomes["total_mutants"] != len(targets):
        raise ValueError("runner mutant count mismatch")
    reports = []
    missed = 0
    for target in targets:
        item = by_name[target["name"]]
        summary = item["summary"]
        log = (directory / item["log_path"]).read_text()
        report = dict(target, outcome=summary, log=item["log_path"], diff=item["diff_path"])
        if summary == "CaughtMutant":
            if target["test"] not in test_names(log, "FAILED"):
                raise ValueError(f"mutant not killed by its reviewed focused test: {target['id']}")
        elif summary == "MissedMutant":
            missed += 1
            if target["test"] not in test_names(log, "ok"):
                raise ValueError(f"survivor did not execute its focused test: {target['id']}")
            if target["id"] not in triage:
                raise ValueError(f"untreated surviving mutant: {target['id']}")
            report["triage"] = triage[target["id"]]
        else:
            # Compile failures, timeouts, and check-only runs never count as kills.
            raise ValueError(f"invalid mutant outcome {summary}: {target['id']}")
        reports.append(report)
    if exit_code != (2 if missed else 0):
        raise ValueError("runner exit code disagrees with mutant outcomes")
    return reports


def collect(root, shard, output, report):
    manifest = json.loads((root / MANIFEST).read_text())
    baseline = json.loads((root / BASELINE).read_text())
    triage = json.loads((root / TRIAGE).read_text())
    validate_policy(manifest, baseline, triage, datetime.datetime.now(datetime.timezone.utc).date())
    targets = [target for target in manifest["targets"] if target["shard"] == shard]
    if not targets:
        raise ValueError(f"empty or unknown shard: {shard}")
    report.update(baseline_revision=manifest["baseline_revision"],
                  baseline_sha256=digest((root / BASELINE).read_bytes()),
                  manifest_sha256=digest((root / MANIFEST).read_bytes()),
                  runner_version=manifest["runner_version"], targets=targets)
    environment = dict(os.environ, CARGO_TERM_COLOR="never")
    for key in ("CARGO_TARGET_DIR", "CARGO_BUILD_TARGET_DIR", "RUSTFLAGS", "RUSTDOCFLAGS",
                "CARGO_ENCODED_RUSTFLAGS", "CARGO_ENCODED_RUSTDOCFLAGS", "LLVM_PROFILE_FILE",
                "CDENV_AGENT_ARTIFACT_DIR"):
        environment.pop(key, None)
    environment["RUSTUP_TOOLCHAIN"] = tomllib.loads(
        (root / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    version = subprocess.check_output(["cargo", "mutants", "--version"], env=environment,
                                      text=True).strip()
    if version != f"cargo-mutants {manifest['runner_version']}":
        raise ValueError(f"wrong runner version: {version}")
    report["rustc"] = subprocess.check_output(["rustc", "--version"], env=environment,
                                              text=True).strip()
    with tempfile.TemporaryDirectory(prefix="cdenv-security-mutants-") as temporary:
        temporary = Path(temporary)
        archive = temporary / "source.tar"
        with archive.open("wb") as destination:
            subprocess.run(["git", "-C", str(root), "archive", "--format=tar", "HEAD"],
                           stdout=destination, check=True)
        source = temporary / "source"
        source.mkdir()
        with tarfile.open(archive) as contents:
            contents.extractall(source, filter="data")
        # The runner's discovery uses cargo metadata without forwarding Cargo args.
        # Validate the lock first, before allowing discovery in the archived checkout.
        with (output / "metadata.log").open("w") as log:
            subprocess.run(["cargo", "metadata", "--locked", "--format-version=1"],
                           cwd=source, env=environment, stdout=subprocess.DEVNULL,
                           stderr=log, check=True)
        # cargo-mutants makes a second scratch copy; --in-place/--iterate are never used.
        command = ["cargo", "mutants", "--dir", str(source), "--config", str(source / CONFIG),
                   "--no-shuffle", "--jobs", "1", "--timeout", "60", "--build-timeout", "300",
                   "--baseline", "run", "--test-tool", "cargo", "--colors", "never",
                   "--re", "^(?:" + "|".join(re.escape(target["name"]) for target in targets) + ")$"]
        for path in sorted({target["name"].split(":", 1)[0] for target in targets}):
            command += ["--file", path]
        report["command"] = command
        with (output / "selection.json").open("w") as selection:
            subprocess.run(command + ["--list", "--json"], env=environment,
                           stdout=selection, check=True, cwd=source)
        selected = json.loads((output / "selection.json").read_text())
        validate_selection(selected, targets, baseline)
        with (output / "runner.log").open("w") as log:
            result = subprocess.run(command + ["--output", str(output)], env=environment,
                                    stdout=log, stderr=subprocess.STDOUT, cwd=source)
        report["runner_exit_code"] = result.returncode
        if (source / "Cargo.lock").read_bytes() != (root / "Cargo.lock").read_bytes():
            raise ValueError("runner discovery changed the archived lockfile")
        directory = output / "mutants.out"
        outcomes = json.loads((directory / "outcomes.json").read_text())
        report["results"] = evaluate(outcomes, directory, targets, triage,
                                     manifest["runner_version"], result.returncode)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("shard", choices=("reference-lock", "transport", "archive-integrity"))
    parser.add_argument("--verify-source", action="store_true",
                        help="independently compare the checkout to the pre-run byte snapshot")
    arguments = parser.parse_args()
    root = Path(git(Path.cwd(), "rev-parse", "--show-toplevel").decode().strip())
    output = root / "target/security-mutations" / arguments.shard
    if arguments.verify_source:
        before = json.loads((output / "source-before.json").read_text())
        after = snapshot(root)
        write_json(output / "source-final.json", after)
        if before != after:
            raise ValueError("checked-out source state changed")
        print("Checked-out source bytes, modes, HEAD and status are unchanged")
        return 0
    # Never accidentally consume stale evidence on a local rerun.
    if output.exists():
        raise ValueError(f"remove previous report directory before rerunning: {output}")
    output.mkdir(parents=True)
    report = {"shard": arguments.shard, "success": False}
    before = None
    try:
        before = snapshot(root)
        write_json(output / "source-before.json", before)
        report["source_revision"] = before["head"]
        if before["status"]:
            raise ValueError("mutation gate requires a clean committed checkout")
        collect(root, arguments.shard, output, report)
        report["success"] = True
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        report["error"] = str(error)
    finally:
        try:
            after = snapshot(root)
            write_json(output / "source-after.json", after)
            report["source_unchanged"] = before == after
            if not report["source_unchanged"]:
                report.update(success=False, error="checked-out source state changed")
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            report.update(success=False, source_unchanged=False, error=str(error))
        write_json(output / "gate.json", report)
    print(json.dumps(report, indent=2))
    return 0 if report["success"] else 1


if __name__ == "__main__":
    sys.exit(main())
