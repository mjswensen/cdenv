---
id: 31
created: 2026-08-20
---

# Build, identify, and embed both agent artifacts

_Converted from [`implementation-chunks/31-agent-build-identity-and-embedding.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §13.1–13.2 and Chunk 8](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#131-build-pipeline)
**Depends on:** 05 and cross-build decisions from 04

## Goal

Make one host build carry verified static Linux agents for x86_64 and arm64 with a shared build identity.

## Work

- Add the agent command/library skeleton and Linux-targeted binary while keeping the crate compilable on macOS; Linux-only behavior must be `cfg`-guarded and return typed unsupported-platform errors elsewhere.
- Implement `cdenv-agent version` machine output containing name, package version, protocol version, and build ID.
- Have `xtask` generate one build ID shared by the host and both agent targets, then build/stage/embed:
  - `x86_64-unknown-linux-musl`
  - `aarch64-unknown-linux-musl`
- Use the cross-build method verified by the ADR. Do not recursively invoke Cargo from `build.rs`.
- Support an injectable/staged agent provider for ordinary development builds/tests; container operations must fail clearly if artifacts are absent.
- Verify staged artifacts are nonempty static Linux executables of the claimed architecture/build/protocol.

## Rust guidance

Load the `rust-best-practices` skill. Keep build identity in small validated types, use typed errors instead of build-time panics, document public provider contracts, and avoid runtime trait objects unless artifact providers genuinely require them.

## Acceptance criteria

- `cargo xtask build` stages a host artifact and two verified agent artifacts sharing one nonempty build ID.
- Tests reject missing, empty, wrong-format, wrong-architecture, and mismatched build/protocol artifacts.
- Ordinary `cargo test --workspace --locked` needs no cross compiler or staged agents.
- The agent crate checks on a macOS target and both Linux musl targets available to CI.
- No build script invokes Cargo; standard quality and deny checks pass.