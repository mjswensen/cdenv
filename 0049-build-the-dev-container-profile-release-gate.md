---
id: 49
created: 2026-08-20
depends-on:
  - 48
  - 53
  - 54
  - 55
  - 56
  - 57
  - 58
  - 59
---

# Build the Dev Container profile release gate

_Converted from [`implementation-chunks/49-devcontainer-profile-release-gate.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §17.4–17.5 and Chunk 17](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#175-automated-dev-container-profile-release-gate)

## Goal

Turn `cdenv-devcontainer-v1` into an editor-independent, repository-owned black-box compatibility contract.

## Work

- Complete the non-published integration package and `cargo xtask test-integration`; ordinary `cargo test --workspace` must remain Docker-free. Build on 59 so an empty, unknown, or wholly skipped suite cannot pass.
- Add all fixtures listed in §17.4, with a small static editor-server simulator rather than a named editor dependency.
- Automate every gate in §17.5: parsing/merge/substitution; image/Dockerfile/Compose lifecycle; Features/locks; users/mounts/options/requirements; lifecycle recovery; ports/forwarding; drift; Compose isolation/volumes/orphans; fail-closed unsupported inputs; checkout immutability.
- Add hostile/malformed JSONC, OCI, archive, Docker/Compose output, cancellation, duplicate/drift, and secret-redaction cases. Explicitly cover bearer-token-on-blob behavior, cross-origin redirect credential stripping, exactly-one Feature layer, descriptor digest verification, published Feature limits, cache reuse/retention, frozen offline locks, and `.dockerignore` context semantics.
- Run against the declared minimum and repository-pinned Docker/Compose versions on Linux x86_64 and arm64 where CI provides them.
- Missing required Docker Engine/CLI, Compose V2, or OpenSSH in a declared integration environment is a failure, never a skip.
- Keep optional reference-CLI differential tests diagnostic and outside normal release dependencies.

## Rust guidance

Load the `rust-best-practices` skill. Treat tests as living documentation: descriptive names, one behavior per test, shared fixture setup only, small snapshots, and clear duplication over wrong abstractions.

## Acceptance criteria

- `cargo xtask test-integration --suite devcontainer-v1` passes every §17.5 item on both required Linux architectures.
- Coverage is traceable from each support-matrix row to at least one positive or fail-closed test, including every security limit and OCI authentication/integrity promise. Regression tests required by implementation issues remain ordinary focused tests and are not deferred solely to this release suite.
- The suite records a positive discovered/executed test count; zero-test, unknown, or all-skipped suites fail.
- The suite runs without Node.js, `@devcontainers/cli`, an editor, or network access except tests explicitly serving/pulling controlled Feature fixtures.
- A checkout mutation guard proves only explicit `cdenv lock` and repository-defined commands can change checkout files.
- Release-mode bounded-buffer/long-running cases pass, and standard workspace quality checks remain green.