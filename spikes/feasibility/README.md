# Disposable feasibility harness

This directory is the quarantined Chunk 00–04 feasibility spike for
`cdenv-devcontainer-v1`. It is an independent, unpublished Cargo workspace and
is not a production API. Production code must not depend on it; delete the
implementation after its fixtures and black-box contracts have been migrated.

## Declared environment

The gate requires Linux/aarch64 or Linux/x86_64 with:

- Rust/Cargo with the committed `Cargo.lock` (verified with Rust 1.97.1);
- Git;
- a reachable local Docker Engine through one Unix socket, Docker CLI, Compose
  V2, and Buildx/BuildKit with `linux/amd64` and `linux/arm64` execution;
- system OpenSSH client and `ssh-keygen`;
- `tmux`, OpenSSL, curl, GNU tar/gzip/coreutils, and `readelf`;
- outbound verified HTTPS access to GHCR for the public Feature fixture.

The verified environment was Docker Engine 29.6.2 (API 1.55), Docker CLI
29.7.1, Compose 5.3.1, Buildx 0.36.0/BuildKit 0.31.2, and OpenSSH 10.0p2.
Missing declared dependencies are failures, not skips. No normal command uses
Node.js or `@devcontainers/cli`.

When Docker is reached from a development container, the scripts discover the
Docker-host source of the repository bind mount before creating sibling
containers. Private Unix control sockets remain on the local Linux filesystem,
not a Docker Desktop shared mount.

## Commands

One-command baseline check:

```bash
cargo run --manifest-path spikes/feasibility/Cargo.toml --locked \
  --bin feasibility-spike -- baseline
```

The baseline verifies all vendored checksums, the pinned schema, this
repository's configuration, fixture discovery, executable Feature installers,
and Cargo-workspace quarantine.

Complete Chunk 00–04 gate:

```bash
./spikes/feasibility/scripts/gate.sh
```

The component gates are intentionally available for diagnosis:

```text
scripts/profile-integration.sh       profile, OCI/HTTPS/local Features, hostile inputs
scripts/agent-cross-build.sh         static musl agent artifacts for amd64 and arm64
scripts/docker-build-matrix.sh       Debian/Alpine × amd64/arm64 Feature/UID builds
scripts/docker-compose-integration.sh labels, Compose, lifecycle, forwarding
scripts/ssh-integration.sh           OpenSSH → Bollard Exec → Russh packet flow
scripts/quarantine-check.sh          isolation, credential, and Node-runtime checks
```

The Docker scripts use uniquely labeled temporary resources and remove them on
success or failure. They retain deterministic evidence only below ignored
`target/spike-runtime/`.

## Pinned inputs and reviewed evidence

- Upstream provenance: [`vendor/PROVENANCE.md`](vendor/PROVENANCE.md)
- Integrity manifest: [`SHA256SUMS`](SHA256SUMS)
- Behavioral fixtures: [`fixtures/`](fixtures/)
- Reviewed normalized plans and Feature lock/evidence: [`expected/`](expected/)
- Decision record: [`../../docs/adr/0001-cdenv-devcontainer-v1-feasibility.md`](../../docs/adr/0001-cdenv-devcontainer-v1-feasibility.md)
- Property contract: [`../../docs/cdenv-devcontainer-v1-support.md`](../../docs/cdenv-devcontainer-v1-support.md)

`profile --accept` rewrites reviewed snapshots and is deliberately not used by
the normal gate. Inspect every diff before invoking it.
