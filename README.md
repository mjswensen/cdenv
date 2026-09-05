# cdenv

`cdenv` is a host CLI for creating and operating isolated, Docker-backed development environments with system OpenSSH access. The supported Dev Container behavior is defined by the [`cdenv-devcontainer-v1` support matrix](docs/cdenv-devcontainer-v1-support.md).

## Workspace

The Rust workspace contains platform-neutral core types, pure Dev Container profile logic, the host CLI, the Linux-targeted agent, `xtask`, and a non-published integration-test package. The disposable feasibility spike under `spikes/feasibility/` is intentionally excluded. Installation, trust, recovery, forwarding, and macOS smoke-test guidance is in the [operations guide](docs/operations.md).

## Development

The development container uses the shared [`ghcr.io/mjswensen/devcontainer`](https://ghcr.io/mjswensen/devcontainer) image and runs as its `mjs` user. On creation it installs the tools pinned in `mise.toml` (Node 24 and Rust 1.97.1) and bootstraps [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) 0.20.2 and [Pi](https://pi.dev/). To prepare a local checkout with the same tooling, install [mise](https://mise.jdx.dev/) and run:

```bash
mise install
mise bootstrap
```

Then run the complete local gate:

```bash
cargo xtask check
```

Build a locally verifiable release archive from a clean checkout (Docker Buildx builds and verifies both static Linux agents):

```bash
cargo xtask dist
```

See [release packaging](docs/release-packaging.md) for the three-host matrix, reproducibility contract, and package-only smoke test.

`cargo xtask check` runs the same baseline commands used by CI:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps
cargo deny check
```

Ordinary workspace tests do not require Docker or OpenSSH. Environment-backed tests use an explicit named suite:

```bash
cargo xtask test-integration --suite devcontainer-v1
cargo xtask test-integration --suite openssh
```

A suite fails when it is unknown, unavailable, discovers zero tests, or executes no
passing tests; it never turns an empty package into a successful gate. Until the
suite is implemented, this is reported as unavailable. Set `CDENV_INTEGRATION=1`
for declared CI runs: missing Docker Engine/CLI, Compose V2, or OpenSSH dependencies
then fail rather than skip.
