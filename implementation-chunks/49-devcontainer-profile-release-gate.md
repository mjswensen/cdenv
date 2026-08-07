# 49 — Build the Dev Container profile release gate

**Parent phase:** [Implementation plan §17.4–17.5 and Chunk 17](../implementation-plan.md#175-automated-dev-container-profile-release-gate)  
**Depends on:** production flows through [48](48-compose-rebuild-and-partial-recovery.md)

## Goal

Turn `cdenv-devcontainer-v1` into an editor-independent, repository-owned black-box compatibility contract.

## Work

- Complete the non-published integration package and `cargo xtask test-integration`; ordinary `cargo test --workspace` must remain Docker-free.
- Add all fixtures listed in §17.4, with a small static editor-server simulator rather than a named editor dependency.
- Automate every gate in §17.5: parsing/merge/substitution; image/Dockerfile/Compose lifecycle; Features/locks; users/mounts/options/requirements; lifecycle recovery; ports/forwarding; drift; Compose isolation/volumes/orphans; fail-closed unsupported inputs; checkout immutability.
- Add hostile/malformed JSONC, OCI, archive, Docker/Compose output, cancellation, duplicate/drift, and secret-redaction cases.
- Run against the declared minimum and repository-pinned Docker/Compose versions on Linux x86_64 and arm64 where CI provides them.
- Missing required Docker Engine/CLI, Compose V2, or OpenSSH in a declared integration environment is a failure, never a skip.
- Keep optional reference-CLI differential tests diagnostic and outside normal release dependencies.

## Rust guidance

Load the `rust-best-practices` skill. Treat tests as living documentation: descriptive names, one behavior per test, shared fixture setup only, small snapshots, and clear duplication over wrong abstractions.

## Acceptance criteria

- `cargo xtask test-integration --suite devcontainer-v1` passes every §17.5 item on both required Linux architectures.
- Coverage is traceable from each support-matrix row to at least one positive or fail-closed test.
- The suite runs without Node.js, `@devcontainers/cli`, an editor, or network access except tests explicitly serving/pulling controlled Feature fixtures.
- A checkout mutation guard proves only explicit `cdenv lock` and repository-defined commands can change checkout files.
- Release-mode bounded-buffer/long-running cases pass, and standard workspace quality checks remain green.