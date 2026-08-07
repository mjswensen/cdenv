# 07 — Model multidimensional status and core errors

**Parent phase:** [Implementation plan §9.2 and §16.1](../implementation-plan.md#92-status-dimensions)  
**Depends on:** [05](05-rust-workspace-and-quality-baseline.md) and [06](06-core-identities-and-workspace-names.md)

## Goal

Represent independent workspace conditions without collapsing live truth into one status enum.

## Work

- Add serializable dimensions for environment, foreground operation, configuration drift/validity, lifecycle, forwarding, and local/provision health.
- Model combinations explicitly: for example, running plus build drift plus background lifecycle plus degraded forwarding must all be representable.
- Add small domain values needed by status, including requested versus assigned forwarding endpoints and concise error/warning facts. Keep presentation strings out of core.
- Define focused core validation/state errors using `thiserror`; do not create a global cross-crate catch-all.
- Ensure values crossing future async boundaries can satisfy `Send + Sync + 'static` without introducing `Arc<dyn Trait>` preemptively.
- Document which dimensions come from persisted intent and which must be supplied by live inspection.

Do not implement Docker correlation, state files, human tables, or CLI exit policy yet.

## Rust guidance

Load the `rust-best-practices` skill, especially error handling, small `Copy` enums, runtime-state modeling, documentation, and one-behavior tests. Avoid compile-time typestate because these states are persisted and discovered dynamically.

## Acceptance criteria

- Unit tests construct and serialize representative combined states, including Docker unavailable, ambiguous containers, interrupted operations, lifecycle failure, and missing supervisor.
- Tests verify meaningful error variants and stable messages rather than a generic string error.
- JSON representations retain every independent dimension and requested/assigned endpoints.
- `cdenv-core` remains platform-neutral and free of host adapter dependencies.
- The standard workspace quality commands pass.