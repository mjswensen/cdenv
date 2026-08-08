# cdenv

`cdenv` is a host CLI for creating and operating isolated, Docker-backed development environments with system OpenSSH access. The supported Dev Container behavior is defined by the [`cdenv-devcontainer-v1` support matrix](docs/cdenv-devcontainer-v1-support.md).

## Workspace

The Rust workspace contains platform-neutral core types, pure Dev Container profile logic, the host CLI, the Linux-targeted agent, `xtask`, and a non-published integration-test package. The disposable feasibility spike under `spikes/feasibility/` is intentionally excluded.

## Development

The repository pins Rust 1.97.1 in `rust-toolchain.toml`. Install [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) 0.20.2 before running the complete local gate:

```bash
cargo install cargo-deny --version 0.20.2 --locked
cargo xtask check
```

`cargo xtask check` runs the same baseline commands used by CI:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps
cargo deny check
```

Ordinary workspace tests do not require Docker or OpenSSH. Environment-backed tests use the explicit entry point:

```bash
cargo xtask test-integration
```

When integration support is declared in CI, missing Docker Engine/CLI, Compose V2, or OpenSSH dependencies are errors rather than skipped tests.
