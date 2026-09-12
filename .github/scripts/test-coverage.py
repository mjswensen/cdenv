"""Dependency-free checks for coverage failure propagation and report inventory."""

import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPTS = Path(__file__).resolve().parent
SPEC = importlib.util.spec_from_file_location("coverage_index", SCRIPTS / "coverage-index.py")
INDEX = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(INDEX)


class CoverageTests(unittest.TestCase):
    def test_inventory_requires_every_matrix_report(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.assertTrue(INDEX.index(root / "absent", root / "index"))
            result = json.loads((root / "index/index.json").read_text())
            self.assertEqual(len(result["errors"]), 6)

    def test_inventory_accepts_low_coverage_but_not_missing_branches_or_failed_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for identifier in INDEX.expected_ids():
                directory = root / f"coverage-{identifier}"
                (directory / "html").mkdir(parents=True)
                for filename in ("coverage.lcov", "html/index.html", "versions.txt", "tests.log"):
                    (directory / filename).write_text("fixture\n")
                (directory / "test-exit-code.txt").write_text("0\n")
                (directory / "report-exit-code.txt").write_text("0\n")
                data = {"type": "llvm.coverage.json.export", "data": [{"totals": {
                    "functions": {"count": 100, "covered": 1},
                    "branches": {"count": 100, "covered": 0},
                }}]}
                (directory / "coverage.json").write_text(json.dumps(data))
            self.assertFalse(INDEX.index(root, root / "index"))
            (directory / "report-exit-code.txt").write_text("1\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "report-exit-code.txt").write_text("0\n")
            (directory / "test-exit-code.txt").write_text("1\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "test-exit-code.txt").write_text("0\n")
            data["data"][0]["totals"]["branches"]["count"] = 0
            (directory / "coverage.json").write_text(json.dumps(data))
            self.assertTrue(INDEX.index(root, root / "index"))

    def run_collector(self, scope, test_status=0, report_status=0, setup_status=0):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            cargo = root / "cargo"
            cargo.write_text('''#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$CALLS"
case "$*" in
  'llvm-cov show-env'*) exit "$SETUP_STATUS" ;;
  test*|xtask*)
    [[ "$CARGO_TARGET_DIR" == "$CARGO_LLVM_COV_TARGET_DIR" ]] || exit 91
    [[ "$CARGO_TARGET_DIR" == "$PWD/target/coverage-build/"* ]] || exit 92
    echo 'test output'; exit "$TEST_STATUS" ;;
  'llvm-cov report'*) exit "$REPORT_STATUS" ;;
esac
''')
            cargo.chmod(0o755)
            rustc = root / "rustc"
            rustc.write_text("#!/bin/sh\necho mock-rustc\n")
            rustc.chmod(0o755)
            subprocess.run(["git", "init", "-q", temporary], check=True)
            subprocess.run(["git", "-c", "user.name=Test", "-c", "user.email=test@example.com",
                            "commit", "--allow-empty", "-qm", "fixture"], cwd=root, check=True)
            environment = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                               CALLS=str(root / "calls"), COVERAGE_ID="fixture",
                               COVERAGE_TOOLCHAIN="mock-nightly", TEST_STATUS=str(test_status),
                               REPORT_STATUS=str(report_status), SETUP_STATUS=str(setup_status))
            result = subprocess.run(["bash", str(SCRIPTS / "coverage.sh"), scope],
                                    cwd=root, env=environment, capture_output=True, text=True)
            outcome = root / "target/coverage/fixture/test-exit-code.txt"
            return result.returncode, (root / "calls").read_text(), (
                outcome.read_text().strip() if outcome.exists() else None)

    def test_release_uses_real_gate_and_preserves_failure(self):
        for suite in ("devcontainer-v1", "openssh"):
            code, calls, outcome = self.run_collector(suite, test_status=7)
            self.assertEqual((code, outcome), (7, "7"))
            self.assertIn(f"xtask test-integration --suite {suite}\n", calls)
            self.assertEqual(calls.count("llvm-cov report --release --branch --locked"), 3)

    def test_workspace_is_locked_and_report_failure_is_not_hidden(self):
        code, calls, outcome = self.run_collector("workspace", report_status=9)
        self.assertEqual((code, outcome), (9, "0"))
        self.assertIn("test --workspace --locked --all-targets\n", calls)

    def test_failed_setup_does_not_run_uninstrumented_tests(self):
        code, calls, outcome = self.run_collector("workspace", setup_status=5)
        self.assertEqual(code, 5)
        self.assertNotIn("test --workspace", calls)
        self.assertIsNone(outcome)


if __name__ == "__main__":
    unittest.main()
