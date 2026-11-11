#!/usr/bin/env python3
"""Offline release-boundary tests: reject incomplete gates and mismatched assets."""

import copy
import importlib.util
import io
import json
from pathlib import Path
import re
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    spec = importlib.util.spec_from_file_location(name, ROOT / path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


release = load("release_ready", ".github/scripts/release-ready.py")
manual = load("manual_macos_smoke", "tests/release/manual-macos-smoke.py")
SHA = "a" * 40


class ReleaseTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.root = self.directory / "repo"
        self.root.mkdir()
        (self.root / "Cargo.toml").write_text('''[workspace]
members = ["crate"]
[workspace.package]
version = "0.1.0"
[workspace.dependencies]
crate = { path = "crate", version = "0.1.0" }
''')
        (self.root / "crate").mkdir()
        (self.root / "crate/Cargo.toml").write_text('[package]\nname = "crate"\nversion.workspace = true\n')
        (self.root / "Cargo.lock").write_text('[[package]]\nname = "crate"\nversion = "0.1.0"\n')
        for filename in ("install.sh", "tests/release/manual-macos-smoke.py", "docs/smoke/macos-docker-desktop-v1.md"):
            target = self.root / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes((ROOT / filename).read_bytes())
        self.source = self.directory / "packages"
        self.evidence = self.directory / "evidence"
        self.destination = self.directory / "dist"
        self.results = {gate: {"result": "success", "outputs": {}} for gate in release.GATES}
        self.environment = {"GITHUB_REF": "refs/tags/v0.1.0", "GITHUB_SHA": SHA,
                            "GITHUB_SERVER_URL": "https://github.com", "GITHUB_REPOSITORY": "test/cdenv",
                            "GITHUB_RUN_ID": "123", "GITHUB_RUN_ATTEMPT": "2"}

    def prepare(self):
        release.write_json(self.evidence / "ci-gate-results/ci-gate-results.json",
                           release.gate_record(self.results, SHA))
        ids = [f"workspace-{p}" for p in ("linux-x86_64", "linux-arm64", "macos-arm64")]
        ids += [f"{suite}-linux-{arch}-{baseline}"
                for suite in ("credentials", "devcontainer-v1", "openssh")
                for arch in ("x86_64", "arm64") for baseline in ("minimum", "pinned")]
        release.write_json(self.evidence / "coverage-index/index.json",
                           {"reports": {identifier: {} for identifier in ids}, "errors": []})
        for shard in ("reference-lock", "transport", "archive-integrity"):
            release.write_json(self.evidence / f"security-mutations-linux-arm64-{shard}/gate.json",
                               {"success": True, "source_unchanged": True, "source_revision": SHA})
        for platform in release.PLATFORMS:
            directory = self.source / f"cdenv-{platform}"
            directory.mkdir(parents=True)
            name = f"cdenv-{platform}-{SHA}.tar"
            with tarfile.open(directory / name, "w") as archive:
                member = tarfile.TarInfo("cdenv")
                data = b"fixture executable, not executed by these tests"
                member.size = len(data)
                archive.addfile(member, io.BytesIO(data))
            (directory / (name + ".sha256")).write_text(f"{release.digest(directory / name)}  {name}\n")
            release.write_json(directory / (name + ".json"), {
                "platform": platform, "artifacts": {"schemaVersion": 1, "version": "0.1.0",
                "buildId": SHA, "protocolVersion": 1, "agents": {"aarch64": "b" * 64, "x86_64": "c" * 64}}})
            (directory / "install.sh").write_bytes((self.root / "install.sh").read_bytes())
            report = self.evidence / f"package-evidence-{platform}"
            release.write_json(report / "package.json", release.package_record(platform, SHA))
            for filename in ("build.log", "rebuild.log", "package-smoke.log", "installed-smoke.log"):
                (report / filename).write_text("passed\n")

    def bundle(self):
        release.bundle(self.source, self.evidence, self.destination, self.root, self.environment)

    def test_tag_lockfile_and_dependency_versions_must_agree(self):
        self.assertFalse(release.identity(self.root, "refs/tags/v0.1.0")["prerelease"])
        for ref in ("refs/tags/v0.2.0", "refs/tags/vgarbage", "refs/tags/0.1.0"):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                release.identity(self.root, ref)
        lock = self.root / "Cargo.lock"
        lock.write_text(lock.read_text().replace("0.1.0", "0.0.9"))
        with self.assertRaises(ValueError):
            release.identity(self.root, "refs/tags/v0.1.0")

    def test_local_dependency_version_must_match_workspace(self):
        path = self.root / "Cargo.toml"
        path.write_text(path.read_text().replace('path = "crate", version = "0.1.0"',
                                                'path = "crate", version = "0.0.9"'))
        with self.assertRaises(ValueError):
            release.identity(self.root, "refs/tags/v0.1.0")

    def test_prerelease_classification_and_semver_validation(self):
        for version, valid, prerelease in (("1.2.3-rc.1", True, True), ("1.2.3+build.1", True, False),
                                            ("1.2.3-01", False, False), ("01.2.3", False, False)):
            with self.subTest(version=version):
                for path in ("Cargo.toml", "Cargo.lock"):
                    target = self.root / path
                    target.write_text(target.read_text().replace(getattr(self, "previous", "0.1.0"), version))
                self.previous = version
                if valid:
                    self.assertEqual(release.identity(self.root, "refs/tags/v" + version)["prerelease"], prerelease)
                else:
                    with self.assertRaises(ValueError):
                        release.identity(self.root, "refs/tags/v" + version)

    def test_missing_skipped_failed_cancelled_gates_never_pass(self):
        self.assertTrue(release.gate_record(self.results, SHA)["success"])
        for status in ("skipped", "failure", "cancelled"):
            results = copy.deepcopy(self.results)
            results["quality"]["result"] = status
            self.assertFalse(release.gate_record(results, SHA)["success"])
        del self.results["quality"]
        self.assertFalse(release.gate_record(self.results, SHA)["success"])

    def test_ci_aggregate_covers_every_declared_job(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text().split("\njobs:\n", 1)[1]
        jobs = set(re.findall(r"^  ([a-z0-9-]+):$", workflow, re.MULTILINE))
        self.assertEqual(jobs - {"required-gates"}, release.GATES)
        aggregate = workflow.split("\n  required-gates:\n", 1)[1]
        needs = re.search(r"^    needs: \[([^\]]+)\]$", aggregate, re.MULTILINE)
        self.assertEqual({job.strip() for job in needs[1].split(",")}, release.GATES)

    def test_ci_push_includes_branches_while_excluding_release_tags(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        push = workflow.split("\n  push:\n", 1)[1].split("\n  pull_request:", 1)[0]
        # GitHub disables branch pushes when only tags/tags-ignore is specified.
        self.assertRegex(push, r"(?m)^    branches: \['\*\*'\]$")
        self.assertRegex(push, r"(?m)^    tags-ignore: \['v\*'\]$")
        self.assertIn("\n  workflow_call:", workflow)

    def test_complete_bundle_binds_assets_and_remains_pending(self):
        self.prepare()
        self.bundle()
        manifest = json.loads((self.destination / "release-manifest.json").read_text())
        self.assertEqual(manifest["commit"], SHA)
        self.assertEqual(manifest["manualMacosSmoke"], "pending")
        self.assertEqual(len(manifest["packages"]), 3)
        for filename, checksum in manifest["assets"].items():
            self.assertEqual(release.digest(self.destination / filename), checksum)
        archive = manual.verify_downloads(self.destination, manifest, "v0.1.0")
        manual.extract_binary(archive, self.directory / "cdenv")
        self.assertTrue((self.directory / "cdenv").is_file())
        self.assertIn("PENDING", (self.destination / "release-review.md").read_text())
        with tarfile.open(self.destination / "release-evidence.tar.gz") as evidence:
            self.assertIn("evidence/ci-gate-results/ci-gate-results.json", evidence.getnames())

    def test_tampered_download_rejected_before_extraction(self):
        self.prepare()
        self.bundle()
        manifest = json.loads((self.destination / "release-manifest.json").read_text())
        archive = self.destination / f"cdenv-macos-aarch64-{SHA}.tar"
        archive.write_bytes(b"tampered")
        with self.assertRaises(ValueError):
            manual.verify_downloads(self.destination, manifest, "v0.1.0")

    def test_incomplete_stale_or_inconsistent_evidence_rejected(self):
        self.prepare()
        mutations = [
            ("ci-gate-results/ci-gate-results.json", lambda d: d.update(commit="d" * 40)),
            ("coverage-index/index.json", lambda d: d["reports"].pop("workspace-macos-arm64")),
            ("security-mutations-linux-arm64-transport/gate.json", lambda d: d.update(success=False)),
            ("package-evidence-macos-aarch64/package.json", lambda d: d.update(packageSmoke="skipped")),
        ]
        for filename, mutate in mutations:
            path = self.evidence / filename
            original = path.read_text()
            data = json.loads(original)
            mutate(data)
            path.write_text(json.dumps(data))
            with self.subTest(filename=filename), self.assertRaises(ValueError):
                self.bundle()
            path.write_text(original)
        metadata = next((self.source / "cdenv-macos-aarch64").glob("*.tar.json"))
        data = json.loads(metadata.read_text())
        data["artifacts"]["agents"]["aarch64"] = "e" * 64
        metadata.write_text(json.dumps(data))
        with self.assertRaises(ValueError):
            self.bundle()

    def test_checksum_and_duplicate_asset_inventory_rejected(self):
        self.prepare()
        checksum = next((self.source / "cdenv-linux-x86_64").glob("*.sha256"))
        original = checksum.read_text()
        checksum.write_text("bad\n")
        with self.assertRaises(ValueError):
            self.bundle()
        checksum.write_text(original)
        (checksum.parent / "unexpected.tar").write_bytes(b"extra")
        with self.assertRaises(ValueError):
            self.bundle()

    def test_archive_traversal_links_and_duplicate_members_rejected(self):
        for names in (("../cdenv",), ("cdenv", "cdenv"), ("cdenv", "other")):
            archive = self.directory / "unsafe.tar"
            with tarfile.open(archive, "w") as contents:
                for name in names:
                    contents.addfile(tarfile.TarInfo(name))
            with self.subTest(names=names), self.assertRaises(ValueError):
                manual.extract_binary(archive, self.directory / "binary")
        with tarfile.open(archive, "w") as contents:
            link = tarfile.TarInfo("cdenv")
            link.type = tarfile.SYMTYPE
            link.linkname = "/bin/sh"
            contents.addfile(link)
        with self.assertRaises(ValueError):
            manual.extract_binary(archive, self.directory / "binary")

    def test_manual_fixtures_are_committed_and_keep_feature_inputs_local(self):
        repositories = self.directory / "fixtures"
        manual.fixtures(repositories)
        for name in ("image", "feature", "compose"):
            repository = repositories / name
            self.assertEqual(manual.run(["git", "-C", str(repository), "status", "--porcelain"]), "")
            self.assertTrue((repository / "tracked.txt").is_file())
        config = json.loads((repositories / "feature/.devcontainer.json").read_text())
        self.assertEqual(list(config["features"]), ["./feature"])
        self.assertTrue((repositories / "feature/feature/install.sh").stat().st_mode & 0o100)


if __name__ == "__main__":
    unittest.main()
