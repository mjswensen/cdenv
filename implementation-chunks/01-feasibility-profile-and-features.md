# 01 — Spike profile planning and Features

**Parent phase:** [Implementation plan §5, §11, and Chunk 0](../implementation-plan.md#chunk-0-disposable-feasibility-and-profile-spike)  
**Depends on:** [00](00-feasibility-baseline.md)

## Goal

Prove the profile semantics that are risky to implement without the official Dev Container CLI.

## Work

Using disposable spike code and the baseline fixtures:

- Parse bounded JSONC and produce deterministic raw/effective configuration snapshots using the candidate schema and property-specific metadata merge rules.
- Exercise staged substitutions, workspace defaults, `${devcontainerId}`, image metadata, and image/Dockerfile scenarios.
- Prove Dockerfile context and BuildKit behavior, supported `build.options`, and rejection of options reserved by cdenv.
- Resolve public OCI Features through anonymous bearer negotiation, plus unauthenticated HTTPS and contained local Features. Verify media types, sizes, digests, lock generation, caching, and hostile archive rejection.
- Prove Feature options, recursive dependencies/order, generated per-Feature layers, metadata contributions, and UID/GID update layers on Debian and Alpine for both target architectures where CI supports them.
- Capture reviewed effective-plan and lockfile snapshots for migration into production tests.

Do not broaden V1 to private registries, insecure HTTP, signatures, or undocumented reference-CLI behavior.

## Rust guidance

Load the `rust-best-practices` skill. In particular, use narrow typed errors, borrow parsed data where possible, avoid intermediate collections in dependency ordering, and write one-behavior tests with small reviewed snapshots.

## Acceptance criteria

- A documented spike command passes for image and Dockerfile fixtures without Node.js or `@devcontainers/cli`.
- Repeated runs produce byte-identical normalized plans, Feature ordering, and lockfiles.
- Corrupt digests, dependency cycles/conflicts, archive traversal, escaping symlinks, oversized input, unsupported sources, and reserved Docker options fail closed with actionable diagnostics.
- Generated Feature/UID images run on the required Debian/Alpine and architecture fixtures available to the spike environment.
- Findings and reusable fixtures are ready for the final ADR; no spike API is used by production code.