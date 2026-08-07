# 05 — Scaffold the Rust workspace and quality baseline

**Parent phase:** [Implementation plan §6 and Chunk 1](../implementation-plan.md#chunk-1-cargo-workspace-standards-and-core-domain)  
**Depends on:** the passed feasibility gate in [04](04-feasibility-adr-and-gate.md)

## Goal

Create the production workspace with enforceable standards before feature implementation starts.

## Work

- Add Rust 2024 workspace members for `cdenv-core`, `cdenv-devcontainer`, `cdenv-cli`, `cdenv-agent`, `xtask`, and the non-published integration-test package described in [§6.1](../implementation-plan.md#61-workspace-layout).
- Pin one stable toolchain and matching workspace `rust-version`; commit `Cargo.lock`.
- Centralize dependency versions/features. Exact-pin volatile integration crates to the versions approved by the ADR, avoid Tokio `full`, and configure `cargo-deny` advisories/licenses/sources/bans.
- Configure workspace Rust/Clippy/rustdoc lints, missing public API docs, broken-link denial, unsafe defaults, and `unsafe_op_in_unsafe_fn` as specified in §6.
- Give host and agent crates thin `main.rs` entry points and testable `lib.rs` files. Keep Linux-only agent behavior behind `cfg`; unsupported host behavior should compile to typed stubs.
- Add minimal crate/module docs and CI/local commands for formatting, linting, tests, docs, and deny checks.

## Rust guidance

Load and follow the `rust-best-practices` skill, especially chapters on lint configuration, layered errors, tests, documentation, and avoiding premature abstraction.

## Acceptance criteria

These commands pass from a clean checkout:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps
cargo deny check
```

Additionally:

- `cargo metadata --locked` lists exactly the intended workspace members and the integration package is non-publishable.
- `cdenv-core` has no Bollard, Russh, Tokio CLI, or host-orchestration dependency.
- `cdenv-devcontainer` has no Docker, Compose, HTTP, credential-helper, or subprocess dependency.
- No build script recursively invokes Cargo, and ordinary workspace tests do not require Docker.