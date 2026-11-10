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
            self.assertEqual(len(result["errors"]), len(INDEX.expected_ids()))

    def test_inventory_accepts_low_coverage_but_not_missing_branches_or_failed_tests(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for identifier in INDEX.expected_ids():
                directory = root / f"coverage-{identifier}"
                (directory / "html").mkdir(parents=True)
                for filename in ("coverage.lcov", "html/index.html", "versions.txt", "tests.log"):
                    (directory / filename).write_text("fixture\n")
                (directory / "setup-exit-code.txt").write_text("0\n")
                (directory / "test-exit-code.txt").write_text("0\n")
                (directory / "report-exit-code.txt").write_text("0\n")
                if not identifier.startswith("workspace-"):
                    (directory / "gate-exit-code.txt").write_text("0\n")
                    (directory / "gate.log").write_text("ordinary gate fixture\n")
                data = {"type": "llvm.coverage.json.export", "data": [{"totals": {
                    "functions": {"count": 100, "covered": 1},
                    "branches": {"count": 100, "covered": 0},
                }}]}
                (directory / "coverage.json").write_text(json.dumps(data))
            self.assertFalse(INDEX.index(root, root / "index"))
            (directory / "gate-exit-code.txt").write_text("7\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "gate-exit-code.txt").write_text("0\n")
            (directory / "setup-exit-code.txt").write_text("not-run\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "setup-exit-code.txt").write_text("0\n")
            (directory / "report-exit-code.txt").write_text("1\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "report-exit-code.txt").write_text("0\n")
            (directory / "test-exit-code.txt").write_text("1\n")
            self.assertTrue(INDEX.index(root, root / "index"))
            (directory / "test-exit-code.txt").write_text("0\n")
            data["data"][0]["totals"]["branches"]["count"] = 0
            (directory / "coverage.json").write_text(json.dumps(data))
            self.assertTrue(INDEX.index(root, root / "index"))

    def run_collector(self, scope, test_status=0, report_status=0, setup_status=0,
                      build_status=0, host_status=0, verified_agents=True):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary).resolve()
            cargo = root / "cargo"
            cargo.write_text('''#!/usr/bin/env bash
printf '%s\\n' "$*" >> "$CALLS"
case "$*" in
  'llvm-cov show-env'*)
    echo 'export RUSTFLAGS=coverage-fixture'
    exit "$SETUP_STATUS" ;;
  build*)
    [[ "$RUSTFLAGS" == coverage-fixture ]] || exit 93
    [[ "$CARGO_TARGET_DIR" == "$CARGO_LLVM_COV_TARGET_DIR" ]] || exit 94
    [[ "$CDENV_BUILD_ID" == "$(git rev-parse HEAD)" ]] || exit 95
    [[ -n "$CDENV_AGENT_ARTIFACT_DIR" ]] || exit 96
    if (( BUILD_STATUS != 0 )); then exit "$BUILD_STATUS"; fi
    mkdir -p "$CARGO_TARGET_DIR/release"
    cat > "$CARGO_TARGET_DIR/release/cdenv" <<'HOST'
#!/usr/bin/env bash
[[ "$1" == __validate-artifacts ]] || exit 97
echo 'host validation' >> "$CALLS"
exit "$HOST_STATUS"
HOST
    chmod +x "$CARGO_TARGET_DIR/release/cdenv" ;;
  test*|xtask*)
    [[ "$CARGO_TARGET_DIR" == "$CARGO_LLVM_COV_TARGET_DIR" ]] || exit 91
    [[ "$CARGO_TARGET_DIR" == "$PWD/target/coverage-build/"* ]] || exit 92
    if [[ "$*" == 'xtask test-integration --suite credentials' ]]; then
      [[ "$CDENV_CREDENTIAL_TEST_BINARY" == "$CARGO_TARGET_DIR/release/cdenv" ]] || exit 98
      [[ -x "$CDENV_CREDENTIAL_TEST_BINARY" ]] || exit 99
      echo "credential binary=$CDENV_CREDENTIAL_TEST_BINARY" >> "$CALLS"
    else
      [[ "$CDENV_CREDENTIAL_TEST_BINARY" == "$PWD/target/release/cdenv" ]] || exit 100
    fi
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
                               REPORT_STATUS=str(report_status), SETUP_STATUS=str(setup_status),
                               BUILD_STATUS=str(build_status), HOST_STATUS=str(host_status),
                               CDENV_CREDENTIAL_TEST_BINARY=str(root / "target/release/cdenv"))
            environment.pop("CDENV_AGENT_ARTIFACT_DIR", None)
            if verified_agents:
                environment["CDENV_AGENT_ARTIFACT_DIR"] = str(root / "verified-agents")
            result = subprocess.run(["bash", str(SCRIPTS / "coverage.sh"), scope],
                                    cwd=root, env=environment, capture_output=True, text=True)
            outcome = root / "target/coverage/fixture/test-exit-code.txt"
            setup_outcome = root / "target/coverage/fixture/setup-exit-code.txt"
            return result.returncode, (root / "calls").read_text(), (
                outcome.read_text().strip() if outcome.exists() else None), (
                setup_outcome.read_text().strip() if setup_outcome.exists() else None)

    def test_release_uses_real_gate_and_preserves_failure(self):
        for suite in ("credentials", "devcontainer-v1", "openssh"):
            code, calls, outcome, setup = self.run_collector(suite, test_status=7)
            self.assertEqual((code, outcome, setup), (7, "7", "0"))
            self.assertIn(f"xtask test-integration --suite {suite}\n", calls)
            self.assertEqual(calls.count("llvm-cov report --release --branch --locked"), 3)

    def test_release_reuses_dependencies_but_cleans_workspace_profiles(self):
        script = (SCRIPTS / "coverage.sh").read_text()
        self.assertIn('build_scope=release', script)
        self.assertIn('coverage-build/$build_scope', script)
        code, calls, _, _ = self.run_collector("credentials")
        self.assertEqual(code, 0)
        self.assertLess(calls.index("llvm-cov clean --workspace"),
                        calls.index("xtask test-integration"))

    def test_credentials_build_and_validate_instrumented_host_before_gate(self):
        code, calls, outcome, setup = self.run_collector("credentials")
        self.assertEqual((code, outcome, setup), (0, "0", "0"))
        build = "build --release --package cdenv-cli --bin cdenv --locked"
        self.assertLess(calls.index("llvm-cov clean --workspace"), calls.index(build))
        self.assertLess(calls.index(build), calls.index("host validation"))
        self.assertLess(calls.index("host validation"), calls.index("xtask test-integration"))
        self.assertIn("/target/coverage-build/release/release/cdenv\n", calls)
        self.assertEqual(calls.count("llvm-cov report --release --branch --locked"), 3)

    def test_credential_host_setup_failures_prevent_gate_and_reports(self):
        for options, expected in [({"build_status": 7}, 7),
                                  ({"host_status": 8}, 8),
                                  ({"verified_agents": False}, 1)]:
            with self.subTest(options=options):
                code, calls, outcome, setup = self.run_collector("credentials", **options)
                self.assertEqual((code, outcome, setup), (expected, "not-run", str(expected)))
                self.assertNotIn("xtask test-integration", calls)
                self.assertNotIn("llvm-cov report", calls)

    def test_other_scopes_keep_original_host_and_do_not_build_coverage_host(self):
        for scope in ("workspace", "devcontainer-v1", "openssh"):
            code, calls, outcome, setup = self.run_collector(scope)
            self.assertEqual((code, outcome, setup), (0, "0", "0"))
            self.assertNotIn("build --release", calls)
            self.assertNotIn("host validation", calls)

    def test_workspace_is_locked_and_report_failure_is_not_hidden(self):
        code, calls, outcome, setup = self.run_collector("workspace", report_status=9)
        self.assertEqual((code, outcome, setup), (9, "0", "0"))
        self.assertIn("test --workspace --locked --all-targets\n", calls)

    def test_failed_setup_does_not_run_uninstrumented_tests(self):
        code, calls, outcome, setup = self.run_collector("workspace", setup_status=5)
        self.assertEqual((code, outcome, setup), (5, "not-run", "5"))
        self.assertNotIn("test --workspace", calls)

    def test_release_evidence_retains_early_failure_and_not_run_outcomes(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            script = SCRIPTS / "release-evidence.sh"
            subprocess.run(["bash", str(script), "init", "first", "second"],
                           cwd=root, check=True)
            self.assertEqual((root / "target/coverage/second/gate-exit-code.txt").read_text(),
                             "not-run\n")

            cargo = root / "cargo"
            cargo.write_text("#!/bin/sh\necho 'ordinary diagnostic'\nexit 7\n")
            cargo.chmod(0o755)
            environment = dict(os.environ, PATH=f"{root}:{os.environ['PATH']}",
                               COVERAGE_ID="first")
            result = subprocess.run(["bash", str(script), "run", "credentials"],
                                    cwd=root, env=environment)
            self.assertEqual(result.returncode, 7)
            self.assertEqual((root / "target/coverage/first/gate-exit-code.txt").read_text(),
                             "7\n")
            self.assertIn("ordinary diagnostic",
                          (root / "target/coverage/first/gate.log").read_text())
            self.assertEqual((root / "target/coverage/second/test-exit-code.txt").read_text(),
                             "not-run\n")

    def test_workflow_runs_independent_suites_and_retains_pre_tool_outcomes(self):
        workflow = (SCRIPTS.parent / "workflows/ci.yml").read_text()
        condition = "if: always() && steps.release_prerequisites.outcome == 'success'"
        self.assertGreaterEqual(workflow.count(condition), 7)
        self.assertIn("release-evidence.sh init", workflow)
        # Instrumentation records setup failure instead of being skipped when tool install fails.
        self.assertNotIn("&& steps.coverage_tools.outcome", workflow)


if __name__ == "__main__":
    unittest.main()
