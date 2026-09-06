# Release packages

For informational workspace and release-suite function/branch reports, artifact
retention and baseline review, see [coverage reporting](coverage.md). Coverage does
not replace the required release gates or installed-package smoke checks. The
independent [Feature security mutation gate](security-mutations.md) requires the
reviewed targeted mutants to be killed or explicitly owned and time-limited.

From a clean checkout with the pinned Rust toolchain, run `cargo xtask dist`.
The Cargo alias and all release builds use `--locked`. Docker Buildx and QEMU
support for both Linux architectures are required when building agents locally.

The command builds and runs the `version` command of both Linux musl agents,
checks their exact build/version/protocol identity and static ELF architecture,
and embeds them in the native host. The full Git commit is the shared build ID.
No timestamp or process ID is used. The tar member has fixed ownership, mode,
and timestamp; host source paths are remapped. Reproducibility assumes the same
pinned toolchain, native SDK/linker and agent builder image/toolchain inputs.

Outputs in `target/dist/`:

- `cdenv-<os>-<architecture>-<commit>.tar`: exactly one regular executable, `cdenv`;
- `.tar.sha256`: SHA-256 and exact archive filename;
- `.tar.json`: deterministic platform, version, build ID, protocol and both
  embedded agent SHA-256 identities;
- `install.sh`: the checksum-verifying GitHub release installer. Tagged builds
  upload this script together with the platform archives to a draft release.

`cargo xtask test-package <archive.tar>` checks the adjacent checksum and metadata,
validates canonical archive contents and native executable format/architecture,
extracts to a temporary directory, and runs `--version` and the private
`__validate-artifacts` entry point. The latter structurally validates both embedded
agents and reports their hashes. Comparing those hashes to the build-time manifest
connects the embedded bytes to the agents whose runtime identities were verified
on Linux; it does not attempt to execute Linux binaries on macOS. Run the smoke
test only on trusted release packages, on their declared native host.

The package-only smoke path clears the host process environment and requires no
Docker, OpenSSH, installation, consent, or home directory. Development binaries
without staged agents fail artifact validation. It deliberately makes no runtime
claim.

On Linux, `cargo xtask test-installed <archive.tar>` first repeats package and
adjacent-checksum validation, then extracts that exact archive into an isolated
`PATH` and root and runs `tests/release/installed-smoke.sh`. The workflow shadows
Node/editor commands with failing stubs, creates a local Git fixture, exercises
the applicable implementation-plan §19 list/status/doctor/lock/down/up/rebuild,
OpenSSH, and foreground-forward commands, and verifies that no `sshd` or published
port 22 exists. It cleans up its container/root and never substitutes a build-tree
binary. The release workflow runs this installed smoke on both Linux archive
architectures. The macOS equivalent is the separately retained, versioned
[Docker Desktop checklist](smoke/macos-docker-desktop-v1.md).

## Publishing a tagged release

Pushing a `v*` tag builds all three platforms and uploads their archives,
checksums, metadata, and `install.sh` to a **draft** GitHub Release. Only the
release-upload job receives repository write permission. The workflow does not
make the release public automatically: the macOS runtime gate is manual.

Before publishing:

1. Download the macOS archive and checksum from the draft release as a repository
   maintainer (or from the corresponding workflow artifacts).
2. Complete the [Apple Silicon Docker Desktop checklist](smoke/macos-docker-desktop-v1.md)
   against that exact archive. All required fields and steps must pass.
3. Attach the completed record and redacted command output to the draft release.
4. Publish the draft through GitHub's release UI. If the smoke fails or is missing,
   leave the release in draft; do not publish it.

Draft releases are not available to the public installer. The default installation
command continues to use the latest published, non-prerelease release until a
maintainer publishes the new release. Mark release candidates as prereleases
before publishing them so they do not become the default installation.

`.github/workflows/release.yml` builds the two agents once on Linux using
`cargo xtask stage-agents`. Each clean native host job downloads that same verified
manifest and pair of agents under `target/`, sets `CDENV_AGENT_ARTIFACT_DIR`, then
runs `cargo xtask dist`. Supplied agents must match the current commit and manifest
hashes and pass ELF validation again. The three supported hosts are Linux x86_64,
Linux aarch64, and macOS aarch64 (Apple Silicon). Each job rebuilds the host to
check identical archive checksums and uploads the archive and its adjacent
metadata. No Docker daemon is needed for macOS package construction. Docker Desktop runtime
behavior is recorded manually with the versioned checklist because hosted macOS
packaging is not an automated Docker Desktop guarantee.
