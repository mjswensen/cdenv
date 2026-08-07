# 19 — Model Features and deterministic dependency ordering

**Parent phase:** [Implementation plan §11.5 and Chunk 6](../implementation-plan.md#115-features-and-lockfiles)  
**Depends on:** [15](15-profile-raw-model-and-validation.md)

## Goal

Implement the pure Feature semantics before adding registry or filesystem transport.

## Work

- Normalize public OCI, verified HTTPS tarball, and checkout-contained local Feature references; reject credentials, private/insecure/deprecated forms at the model boundary.
- Define typed Feature metadata, options/defaults, install identity, version/digest/integrity, dependencies, `installsAfter`, and contributions (environment, mounts, capabilities, entrypoints, lifecycle hooks, metadata).
- Validate option types/allowed values and recursively merge equal dependency requests; report conflicting options with the dependency path.
- Implement the specification’s deterministic round algorithm for `dependsOn`, `installsAfter`, and `overrideFeatureInstallOrder`.
- Detect cycles and produce an actionable graph error. Guarantee each resolved Feature and each contribution is applied exactly once.
- Define lockfile domain values for later parsing/generation, but perform no source I/O and do not write files.

## Rust guidance

Load the `rust-best-practices` skill. Use iterators for graph transformations where readable and explicit loops for ordered/error-producing rounds; avoid needless collections/clones and test error variants directly.

## Acceptance criteria

- Tests cover defaults, invalid options, recursive dependencies, equal and conflicting duplicate requests, soft-order hints, overrides, disconnected graphs, and cycles.
- Ordering remains identical across randomized input map order and repeated runs.
- Every contribution appears exactly once in the resolved result.
- Unsupported references fail before any future adapter could perform I/O.
- Pure tests and all standard workspace quality commands pass.