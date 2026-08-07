# 06 — Implement core identities and workspace naming

**Parent phase:** [Implementation plan §8 and Chunk 1](../implementation-plan.md#8-domain-types-and-workspace-naming)  
**Depends on:** [05](05-rust-workspace-and-quality-baseline.md)

## Goal

Add the validated, platform-neutral identity types used at trust and serialization boundaries.

## Work

In `cdenv-core` implement focused newtypes for workspace name/host, container and installation identity, agent build/protocol identity, generation/profile identity, and supported container architecture.

- Enforce lowercase ASCII workspace labels, start/end rules, 63-character limit, and safe display/serde parsing.
- Derive default names from HTTPS, SSH, SCP-like Git URLs, `file://` URLs, and local Git paths: final component, strip `.git`, lowercase, collapse invalid runs, trim, then validate.
- Never invent a numeric suffix; return an error that points users to `--name` when derivation is empty, too long, invalid, or already reserved (reservation itself comes later).
- Implement exact architecture aliases and target mapping for x86_64/amd64 and arm64/aarch64; reject all others.
- Keep constructors explicit enough that unvalidated strings cannot be mistaken for identities.

Do not add filesystem, Git subprocess, Docker, or CLI rendering behavior.

## Rust guidance

Load the `rust-best-practices` skill. Borrow `&str`/`&Path` at parsing boundaries, use small `Copy` identifiers/enums where appropriate, return typed `thiserror` errors, and avoid typestate for persisted/runtime identity.

## Acceptance criteria

- Unit tests cover every valid/invalid naming rule and all source forms, including Unicode, empty/overlong results, trailing separators, `.git`, and SCP-like URLs.
- Serde round trips preserve validated identities and reject invalid serialized values.
- Architecture tests map only the two supported aliases to the exact musl targets.
- Public APIs have rustdoc examples where useful; production code contains no `unwrap()`/`expect()`.
- The standard workspace format, Clippy, test, and doc commands from [05](05-rust-workspace-and-quality-baseline.md) pass.