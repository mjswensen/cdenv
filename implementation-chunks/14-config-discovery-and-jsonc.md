# 14 — Implement config discovery and bounded JSONC parsing

**Parent phase:** [Implementation plan §10.2, §11.1, §11.3, and Chunk 4](../implementation-plan.md#chunk-4-dev-container-syntax-metadata-and-immutable-planner)  
**Depends on:** feasibility outputs from [04](04-feasibility-adr-and-gate.md), plus [05](05-rust-workspace-and-quality-baseline.md) and [13](13-git-and-create-transaction.md)

## Goal

Create the pure front end for the pinned `cdenv-devcontainer-v1` profile.

## Work

- Vendor the exact schema selected by the ADR with compile-time/profile identity checks; never download or track schema updates at runtime.
- In `cdenv-devcontainer`, implement discovery over an injected inventory using precedence: `.devcontainer/devcontainer.json`, `.devcontainer.json`, then `.devcontainer/<folder>/devcontainer.json`; fail on ambiguous third-form matches.
- Keep host filesystem reading/containment in `cdenv-cli`; pass bounded bytes and validated relative paths into the pure crate.
- Parse comments-capable JSONC with the audited parser selected in the ADR. Enforce configured limits for file bytes, nesting, strings, arrays, object sizes, and lifecycle groups.
- Preserve source spans/path context in syntax diagnostics. Reject trailing commas as required by the pinned profile/schema.
- Return a typed raw document plus diagnostics, without merging metadata, substituting variables, or invoking Docker/network services.

## Rust guidance

Load the `rust-best-practices` skill. Avoid ad-hoc comment stripping, return precise `thiserror` variants, borrow source text where practical, and use small snapshots only for span-rich diagnostics.

## Acceptance criteria

- Pure unit tests cover every precedence case, explicit selection, no config, and ambiguous folder discovery.
- Parser tests cover comments, rejected trailing commas, malformed tokens, duplicate/unknown raw keys as applicable, and every configured bound.
- Diagnostics include stable property/file locations and profile revision without giant snapshots.
- `cdenv-devcontainer` tests run with no filesystem, Docker, HTTP, or subprocess access.
- The vendored schema’s checksum/revision matches the ADR, and standard workspace quality commands pass.