---
id: 29
created: 2026-08-20
---

# Implement Compose project planning and the V2 adapter

_Converted from [`implementation-chunks/29-compose-planning-and-adapter.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.10, §12, and Chunk 7](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#1110-compose-v2-behavior)
**Depends on:** 20, 21, 23, and 28

## Goal

Translate a Compose scenario into an isolated, deterministic project and verifiable primary-service plan.

## Work

- Enforce the ADR’s minimum Compose V2 version/capabilities; reject V1.
- Derive a stable Docker-safe project name from installation ID and workspace name, truncating with a stable hash.
- Generate deterministic JSON-compatible overrides under cdenv tmp/cache for labels, workspace settings, environment, commands/entrypoints, and profile enrichment. Always pass explicit files, project name, cwd, and Docker endpoint without a shell.
- Parse only required typed Compose output. Never log the interpolated model because it may contain secrets.
- For Feature/UID enrichment, ask Compose to pull/build the declared base primary service, identify/tag that exact image, build the derived image, override the primary service to it, and disable build for final start.
- Locate and verify the primary container/service/project/image through Bollard; reject substitutions or ambiguity.

## Rust guidance

Load the `rust-best-practices` skill. Keep override generation pure and snapshot-tested, use adapter-specific errors, static fakes, borrowed config paths, and no generic Compose abstraction.

## Acceptance criteria

- Unit tests prove stable project names, truncation/hash behavior, deterministic overrides, exact argv/endpoint, and secret-safe diagnostics.
- Fake/real tests reject old Compose, wrong project/service/image, ambiguous primary containers, and failed capabilities.
- Compose Feature fixtures verify the exact built base image is enriched and final start cannot rebuild/substitute it.
- Generated overrides stay outside checkout and contain required cdenv labels.
- Standard workspace quality commands and opt-in Compose tests pass.