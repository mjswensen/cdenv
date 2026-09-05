# Release packages

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
  embedded agent SHA-256 identities.

`cargo xtask test-package <archive.tar>` checks the adjacent checksum and metadata,
validates canonical archive contents and native executable format/architecture,
extracts to a temporary directory, and runs `--version` and the private
`__validate-artifacts` entry point. The latter structurally validates both embedded
agents and reports their hashes. Comparing those hashes to the build-time manifest
connects the embedded bytes to the agents whose runtime identities were verified
on Linux; it does not attempt to execute Linux binaries on macOS. Run the smoke
test only on trusted release packages, on their declared native host.

The smoke path clears the host process environment and requires no Docker,
OpenSSH, installation, consent, or home directory. Development binaries without
staged agents fail artifact validation. Installed operational workflows remain
outside this gate (issue 66).

`.github/workflows/release.yml` builds the two agents once on Linux using
`cargo xtask stage-agents`. Each clean native host job downloads that same verified
manifest and pair of agents under `target/`, sets `CDENV_AGENT_ARTIFACT_DIR`, then
runs `cargo xtask dist`. Supplied agents must match the current commit and manifest
hashes and pass ELF validation again. The three supported hosts are Linux x86_64,
Linux aarch64, and macOS aarch64 (Apple Silicon). Each job rebuilds the host to
check identical archive checksums and uploads the archive and its adjacent
metadata. No Docker daemon is needed on the macOS runner.
