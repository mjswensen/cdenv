"""Dependency-free tests for fail-closed mutation selection, triage and evidence."""

import copy
import datetime
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
ROOT = SCRIPTS.parent.parent
SPEC = importlib.util.spec_from_file_location("security_mutations", SCRIPTS / "security-mutations.py")
GATE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GATE)
TODAY = datetime.date(2026, 9, 6)


def triage():
    return {"owner": "@security-maintainer", "expires": "2026-10-01",
            "reason": "Fixture only: equivalent mutation under review"}


class PolicyTests(unittest.TestCase):
    def setUp(self):
        self.manifest = json.loads((ROOT / GATE.MANIFEST).read_text())
        self.baseline = json.loads((ROOT / GATE.BASELINE).read_text())

    def validate(self, exceptions=None):
        GATE.validate_policy(self.manifest, self.baseline, exceptions or {}, TODAY)

    def test_reviewed_policy_is_valid_with_nonempty_deterministic_shards(self):
        self.validate(json.loads((ROOT / GATE.TRIAGE).read_text()))
        self.assertEqual(self.manifest["runner_version"], "27.1.0")
        self.assertEqual([sum(target["shard"] == shard for target in self.manifest["targets"])
                          for shard in self.manifest["shards"]], [6, 8, 6])

    def test_empty_duplicate_missing_baseline_and_empty_shard_fail(self):
        cases = []
        empty = copy.deepcopy(self.manifest)
        empty["targets"] = []
        cases.append(empty)
        duplicate = copy.deepcopy(self.manifest)
        duplicate["targets"].append(duplicate["targets"][0])
        cases.append(duplicate)
        removed = copy.deepcopy(self.manifest)
        removed["targets"].pop()
        cases.append(removed)
        empty_shard = copy.deepcopy(self.manifest)
        empty_shard["shards"].append("empty")
        cases.append(empty_shard)
        for case in cases:
            with self.subTest(case=case), self.assertRaises(ValueError):
                GATE.validate_policy(case, self.baseline, {}, TODAY)

    def test_expired_unknown_and_unowned_triage_fail(self):
        identifier = self.manifest["targets"][0]["id"]
        for entry in [dict(triage(), expires=str(TODAY)),
                      dict(triage(), expires="2020-01-01"),
                      dict(triage(), expires="not-a-date"),
                      dict(triage(), owner=""), dict(triage(), reason="")]:
            with self.subTest(entry=entry), self.assertRaises(ValueError):
                self.validate({identifier: entry})
        with self.assertRaises(ValueError):
            self.validate({"not-a-target": triage()})
        # Triage is checked globally, not only for survivors in the current shard.
        self.validate({identifier: triage()})

    def test_selection_requires_exact_names_and_reviewed_diff_hash(self):
        target = self.manifest["targets"][0]
        selected = [{"name": target["name"], "diff": "reviewed diff"}]
        baseline = {"diff_sha256": {target["id"]: GATE.digest(b"reviewed diff")}}
        GATE.validate_selection(selected, [target], baseline)
        for invalid in ([], selected * 2, [{"name": "drifted", "diff": "reviewed diff"}],
                        [{"name": target["name"], "diff": "changed diff"}]):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                GATE.validate_selection(invalid, [target], baseline)


class OutcomeTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.target = {"id": "boundary", "name": "src/security.rs:1:1: delete guard",
                       "test": "security::tests::rejects_unsafe", "reason": "security",
                       "shard": "fixture"}
        (self.directory / "baseline.log").write_text("test security::tests::rejects_unsafe ... ok\n")
        (self.directory / "mutant.log").write_text("test security::tests::rejects_unsafe ... FAILED\n")
        self.outcomes = {
            "cargo_mutants_version": "27.1.0", "end_time": "2026-09-06T00:00:00Z",
            "total_mutants": 1,
            "outcomes": [
                {"scenario": "Baseline", "summary": "Success", "log_path": "baseline.log"},
                {"scenario": {"Mutant": {"name": self.target["name"]}},
                 "summary": "CaughtMutant", "log_path": "mutant.log", "diff_path": "mutant.diff"},
            ],
        }

    def evaluate(self, exceptions=None, exit_code=0):
        return GATE.evaluate(self.outcomes, self.directory, [self.target],
                             exceptions or {}, "27.1.0", exit_code)

    def test_caught_requires_the_named_focused_test_not_an_unrelated_failure(self):
        self.assertEqual(self.evaluate()[0]["outcome"], "CaughtMutant")
        (self.directory / "mutant.log").write_text("test unrelated ... FAILED\n")
        with self.assertRaises(ValueError):
            self.evaluate()

    def test_survivor_requires_owned_triage_and_an_executed_test(self):
        self.outcomes["outcomes"][1]["summary"] = "MissedMutant"
        (self.directory / "mutant.log").write_text("test security::tests::rejects_unsafe ... ok\n")
        with self.assertRaisesRegex(ValueError, "untreated surviving"):
            self.evaluate(exit_code=2)
        result = self.evaluate({"boundary": triage()}, exit_code=2)
        self.assertEqual(result[0]["triage"], triage())
        (self.directory / "mutant.log").write_text("test result: ok. 0 passed\n")
        with self.assertRaises(ValueError):
            self.evaluate({"boundary": triage()}, exit_code=2)

    def test_runner_errors_timeouts_unviable_and_incomplete_runs_fail_even_with_triage(self):
        for code in (1, 3, 4, 101, -9):
            with self.subTest(code=code), self.assertRaises(ValueError):
                self.evaluate(exit_code=code)
        for summary in ("Timeout", "Unviable", "Success", "Failure"):
            self.outcomes["outcomes"][1]["summary"] = summary
            with self.subTest(summary=summary), self.assertRaises(ValueError):
                self.evaluate({"boundary": triage()})
        self.outcomes["end_time"] = None
        with self.assertRaises(ValueError):
            self.evaluate()

    def test_missing_duplicate_empty_or_ignored_baseline_tests_fail(self):
        valid = copy.deepcopy(self.outcomes)
        for scenarios in ([], valid["outcomes"][:1], valid["outcomes"] * 2):
            self.outcomes["outcomes"] = scenarios
            with self.subTest(scenarios=scenarios), self.assertRaises(ValueError):
                self.evaluate()
        self.outcomes = valid
        for log in ("test result: ok. 0 passed\n", "test security::tests::rejects_unsafe ... ignored\n"):
            (self.directory / "baseline.log").write_text(log)
            with self.assertRaises(ValueError):
                self.evaluate()

    def test_failed_baseline_wrong_version_and_disagreeing_exit_fail(self):
        with self.assertRaises(ValueError):
            self.evaluate(exit_code=2)
        self.outcomes["outcomes"][0]["summary"] = "Failure"
        with self.assertRaises(ValueError):
            self.evaluate()
        self.outcomes["cargo_mutants_version"] = "other"
        with self.assertRaises(ValueError):
            self.evaluate()


class SourceTests(unittest.TestCase):
    def test_dirty_checkout_fails_without_modification_and_independent_check_detects_later_edit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / ".gitignore").write_text("target/\n")
            (root / "source.rs").write_text("committed\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                            "commit", "-qm", "fixture"], cwd=root, check=True)
            (root / "source.rs").write_text("uncommitted\n")
            before = GATE.snapshot(root)
            command = ["python3", str(SCRIPTS / "security-mutations.py"), "transport"]
            result = subprocess.run(command, cwd=root, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            report = json.loads((root / "target/security-mutations/transport/gate.json").read_text())
            self.assertIn("clean committed checkout", report["error"])
            self.assertTrue(report["source_unchanged"])
            self.assertEqual(before, GATE.snapshot(root))
            verified = subprocess.run(command + ["--verify-source"], cwd=root, capture_output=True)
            self.assertEqual(verified.returncode, 0)
            (root / "source.rs").write_text("unexpected later mutation\n")
            verified = subprocess.run(command + ["--verify-source"], cwd=root, capture_output=True)
            self.assertNotEqual(verified.returncode, 0)

    def test_byte_modes_links_and_new_source_are_verified_but_build_output_is_not(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subprocess.run(["git", "init", "-q", str(root)], check=True)
            (root / "source.rs").write_text("original\n")
            (root / "link").symlink_to("source.rs")
            (root / ".gitignore").write_text("target/\n")
            subprocess.run(["git", "add", "."], cwd=root, check=True)
            subprocess.run(["git", "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid",
                            "commit", "-qm", "fixture"], cwd=root, check=True)
            before = GATE.snapshot(root)
            self.assertEqual(before["status"], "")
            (root / "target").mkdir()
            (root / "target/report.json").write_text("build evidence")
            self.assertEqual(before, GATE.snapshot(root))
            (root / "source.rs").write_text("mutation\n")
            self.assertNotEqual(before, GATE.snapshot(root))
            (root / "source.rs").write_text("original\n")
            self.assertEqual(before, GATE.snapshot(root))
            original_mode = (root / "source.rs").stat().st_mode
            (root / "source.rs").chmod(0o755)
            self.assertNotEqual(before, GATE.snapshot(root))
            (root / "source.rs").chmod(original_mode)
            (root / "new.rs").write_text("unexpected")
            self.assertNotEqual(before, GATE.snapshot(root))
            (root / "new.rs").unlink()
            (root / "link").unlink()
            (root / "link").symlink_to("elsewhere")
            self.assertNotEqual(before, GATE.snapshot(root))


if __name__ == "__main__":
    unittest.main()
