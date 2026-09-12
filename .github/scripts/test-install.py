"""Offline installer regression tests; no release assets or network required."""

import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

INSTALLER = Path(__file__).resolve().parents[2] / "install.sh"
# x86_64 is intentionally disabled while cdenv targets ARM hosts only.
HOSTS = [("Linux", "aarch64", "linux-aarch64", "sha256sum"),
         ("Darwin", "arm64", "macos-aarch64", "shasum")]


class InstallerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="cdenv-install-test-")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.data = self.root / "data"
        self.data.mkdir()
        self.target = self.root / "install" / "cdenv"
        self.payload = b"#!/bin/sh\necho cdenv-fixture\n"
        # An isolated PATH makes missing-dependency tests independent of the host.
        for command in ("awk", "chmod", "cp", "grep", "mkdir", "mktemp", "mv", "rm"):
            executable = shutil.which(command)
            self.assertIsNotNone(executable, command)
            (self.bin / command).symlink_to(executable)
        self.stub("uname", 'case "$1" in -s) echo "$OS";; -m) echo "$ARCH";; esac\n')
        self.stub("curl", '''[ "$1" = -fsSL ] && [ "$3" = -o ] || exit 90
printf '%s\\n' "$2" >> "$FIXTURE/urls"
case "$2" in
  */releases/latest|*/releases/tags/*) name=release.json ;;
  *) name=${2##*/} ;;
esac
cp "$FIXTURE/$name" "$4"
''')
        self.stub("tar", 'echo extracted >> "$FIXTURE/extractions"\n'
                  'exec "$REAL_TAR" "$@"\n')
        self.env = dict(os.environ, PATH=str(self.bin), FIXTURE=str(self.data),
                        REAL_TAR=shutil.which("tar"), TMPDIR=str(self.root),
                        CDENV_INSTALL_DIR=str(self.target.parent),
                        CDENV_VERSION="latest", CDENV_REPOSITORY="fixture/cdenv",
                        CDENV_API_ROOT="https://api.invalid")

    def stub(self, name, body):
        path = self.bin / name
        path.write_text("#!/bin/sh\nset -eu\n" + body)
        path.chmod(0o755)

    def prepare(self, host):
        system, architecture, platform, hasher = host
        self.env.update(OS=system, ARCH=architecture)
        archive = self.data / f"cdenv-{platform}-fixture.tar"
        with tarfile.open(archive, "w", format=tarfile.USTAR_FORMAT) as output:
            member = tarfile.TarInfo("cdenv")
            member.size = len(self.payload)
            member.mode = 0o755
            output.addfile(member, io.BytesIO(self.payload))
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.checksum = self.data / (archive.name + ".sha256")
        self.checksum.write_text(f"{digest}  {archive.name}\n")
        self.env["DIGEST"] = digest
        self.stub(hasher, 'printf \'%s  archive\\n\' "$DIGEST"\n')
        (self.data / "release.json").write_text(json.dumps({"assets": [{
            "name": archive.name,
            "browser_download_url": "https://download.invalid/" + archive.name,
        }]}, indent=2))
        return hasher

    def run_installer(self):
        return subprocess.run(["/bin/sh", str(INSTALLER)], env=self.env,
                              capture_output=True, text=True, timeout=10)

    def assert_rejected_before_extraction(self):
        result = self.run_installer()
        self.assertNotEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertFalse(self.target.exists())
        self.assertFalse((self.data / "extractions").exists())

    def test_supported_hosts_install_verified_executable(self):
        for host in HOSTS:
            with self.subTest(host=host):
                self.prepare(host)
                result = self.run_installer()
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertEqual(self.target.read_bytes(), self.payload)
                self.assertEqual(self.target.stat().st_mode & 0o777, 0o755)

    def test_empty_malformed_and_mismatched_checksums_are_rejected(self):
        self.prepare(HOSTS[0])
        for checksum in ("", "\n", "bad\n", "g" * 64, "0" * 64):
            with self.subTest(checksum=checksum):
                self.checksum.write_text(checksum)
                self.assert_rejected_before_extraction()

    def test_failed_hashers_are_rejected_even_when_they_print_the_correct_digest(self):
        for host in HOSTS:
            with self.subTest(host=host):
                hasher = self.prepare(host)
                self.stub(hasher, 'printf \'%s  archive\\n\' "$DIGEST"\nexit 1\n')
                self.assert_rejected_before_extraction()

    def test_missing_hashers_are_rejected(self):
        for host in HOSTS:
            with self.subTest(host=host):
                hasher = self.prepare(host)
                (self.bin / hasher).unlink()
                self.assert_rejected_before_extraction()

    def test_empty_checksum_with_failed_hasher_is_rejected(self):
        hasher = self.prepare(HOSTS[0])
        self.checksum.write_text("")
        self.stub(hasher, "exit 1\n")
        self.assert_rejected_before_extraction()

    def test_explicit_version_and_repository_select_the_tag_endpoint(self):
        self.prepare(HOSTS[0])
        self.env.update(CDENV_VERSION="1.2.3", CDENV_REPOSITORY="fork/cdenv")
        result = self.run_installer()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual((self.data / "urls").read_text().splitlines()[0],
                         "https://api.invalid/repos/fork/cdenv/releases/tags/v1.2.3")


if __name__ == "__main__":
    unittest.main()
