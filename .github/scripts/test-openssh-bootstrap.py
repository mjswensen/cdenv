#!/usr/bin/env python3
"""Exercise OpenSSH prerequisite setup without root, downloads, or package changes."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


class OpenSshBootstrapTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.bin = self.directory / "bin"
        self.bin.mkdir()
        self.trace = self.directory / "trace"
        self.environment = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin",
                                RUNNER_TEMP=str(self.directory), TRACE=str(self.trace),
                                MISSING_PACKAGE="", UPDATE_STATUS="0", INSTALL_STATUS="0")
        self.stub("dpkg-query", '''
for package; do :; done
if [ "$package" = "$MISSING_PACKAGE" ]; then exit 1; fi
printf installed
''')
        self.stub("sudo", '''
printf '%s\\n' "$*" >> "$TRACE"
case " $* " in
  *" update "*) exit "$UPDATE_STATUS" ;;
  *" install "*) exit "$INSTALL_STATUS" ;;
  *) exit 90 ;;
esac
''')
        # Stop at the download boundary: later extraction/build stays unchanged.
        self.stub("curl", '''
printf 'curl %s\\n' "$*" >> "$TRACE"
exit 79
''')

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)

    def run_bootstrap(self):
        return subprocess.run(["bash", str(ROOT / ".github/scripts/install-openssh.sh")],
                              env=self.environment, capture_output=True, text=True, timeout=10)

    def test_preinstalled_inputs_do_not_contact_ubuntu_mirrors(self):
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 79, result.stderr)
        commands = self.trace.read_text().splitlines()
        self.assertEqual(len(commands), 1)
        self.assertTrue(commands[0].startswith("curl "))
        self.assertIn("--retry 3 --retry-all-errors --retry-max-time 120", commands[0])
        self.assertIn("openssh-10.0p2.tar.gz", commands[0])

    def test_only_missing_inputs_are_installed(self):
        self.environment["MISSING_PACKAGE"] = "libssl-dev"
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 79, result.stderr)
        commands = self.trace.read_text().splitlines()
        self.assertEqual(commands[0], "apt-get -o Acquire::Retries=3 -o APT::Update::Error-Mode=any update")
        self.assertEqual(commands[1], "apt-get -o Acquire::Retries=3 install --yes --no-install-recommends libssl-dev")
        self.assertEqual(len(commands), 3)

    def test_failed_index_refresh_does_not_use_stale_indexes(self):
        self.environment.update(MISSING_PACKAGE="gcc", UPDATE_STATUS="73")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 73, result.stderr)
        commands = self.trace.read_text().splitlines()
        self.assertEqual(len(commands), 1)
        self.assertTrue(commands[0].endswith(" update"))

    def test_failed_dependency_install_never_starts_download(self):
        self.environment.update(MISSING_PACKAGE="make", INSTALL_STATUS="74")
        result = self.run_bootstrap()
        self.assertEqual(result.returncode, 74, result.stderr)
        self.assertEqual(len(self.trace.read_text().splitlines()), 2)


if __name__ == "__main__":
    unittest.main()
