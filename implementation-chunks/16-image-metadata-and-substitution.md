# 16 — Implement image metadata merge and staged substitution

**Parent phase:** [Implementation plan §11.3 and Chunk 4](../implementation-plan.md#113-parsing-paths-metadata-and-substitutions)  
**Depends on:** [15](15-profile-raw-model-and-validation.md)

## Goal

Produce deterministic effective values from repository config and injected image metadata without generic JSON merging.

## Work

- Encode the pinned specification’s property-specific image `devcontainer.metadata` merge table. Preserve ordering rules and apply repository configuration last where required.
- Accept inspected base/final image metadata as typed planner inputs; do not inspect images in this crate.
- Implement only allowed substitutions, at the correct stage: local/workspace inputs first and container/runtime values only when actual container data is available.
- Compute `${devcontainerId}` from canonical sorted stable identity labels exactly as selected in the ADR.
- Preserve unresolved runtime-only values as typed plan inputs rather than guessing or reading host globals.
- Validate merged/effective output again and retain useful source/property context in failures.
- Ensure resolved `localEnv`, environment, build arguments, and secret-like values are never included in serializable persisted plan summaries or diagnostics.

## Rust guidance

Load the `rust-best-practices` skill. Prefer borrowed views during merge, iterators for pure transformations, explicit loops when order/error short-circuiting is clearer, and precise typed errors.

## Acceptance criteria

- Fixture tests cover every property-specific merge strategy, conflicting metadata, repository-last ordering, and validation after merge.
- Substitution tests cover allowed/disallowed properties, missing values, stage boundaries, escaping, and stable `${devcontainerId}` across rebuild generations.
- Reordering input object keys does not change normalized effective output.
- Secret-marker tests prove snapshots, errors, and persisted summaries contain no substituted values.
- Pure tests need no Docker/network/filesystem access and all workspace quality commands pass.