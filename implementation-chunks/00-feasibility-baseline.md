# 00 — Establish the feasibility baseline

**Parent phase:** [Implementation plan §5 and Chunk 0](../implementation-plan.md#5-feasibility-first-phase)  
**Depends on:** nothing

## Goal

Create an isolated, disposable spike harness and pin the inputs needed to investigate V1 without committing to production abstractions.

## Work

- Create a spike-only harness outside the future production crates. Make its commands reproducible and document required Docker, Compose, Git, and OpenSSH dependencies.
- Select a candidate upstream Dev Container specification commit, vendor its schema/fixtures with provenance and checksums, and expose the candidate profile as `cdenv-devcontainer-v1` inside the spike.
- Fix this repository’s `.devcontainer/devcontainer.json` so it is valid under the candidate schema, including removing the trailing comma.
- Add the four fixture families required by [§5.2](../implementation-plan.md#52-spike-fixtures): image, Dockerfile plus Feature/lifecycle, two-service Compose, and metadata/Feature-source fixtures.
- Record candidate Docker Engine/CLI, Compose V2, Bollard, Russh, JSONC, HTTP/TLS, and cross-build versions for later verification. Do not declare minimum versions yet.

Keep spike code quarantined and easy to delete. Do not create production crate APIs or promote spike abstractions.

## Rust guidance

If the harness uses Rust, load and follow the `rust-best-practices` skill. Prefer direct code over speculative traits, use typed errors rather than panics, and keep tests behavior-focused.

## Acceptance criteria

- A documented one-command baseline check validates the vendored schema identity, this repository’s Dev Container configuration, and fixture discovery.
- The vendored files identify their exact upstream commit and pass checksum verification.
- Every required fixture family exists and has a short statement of the behavior it will prove.
- The spike is not a member or dependency of the production Cargo workspace.
- No production dependency/version decision is presented as final before Chunk 04.