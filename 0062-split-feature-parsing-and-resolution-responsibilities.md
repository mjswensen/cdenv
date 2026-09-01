---
id: 62
created: 2026-08-22
---

# Split Feature parsing and resolution responsibilities

**Blocks:** 65

## Description

`crates/cdenv-devcontainer/src/feature.rs` combines Feature reference parsing, metadata validation, option normalization, dependency ordering, and lock serialization. Split reference/metadata parsing, graph resolution, and frozen-lock encoding into explicit responsibilities, retaining pure APIs and descriptive unit tests.

This issue covers the pure Dev Container Feature model. It does not move or redesign source download, OCI authentication, cache, or archive extraction in `cdenv-cli`; issue 65 may target both areas after these boundaries are stable.

## Acceptance criteria

- [ ] Reference normalization and metadata/option parsing are independently testable from graph resolution.
- [ ] Dependency resolution and frozen-lock parse/encode/staleness validation have focused tests at explicit boundaries.
- [ ] Security-sensitive reference rejection, deterministic ordering, canonical lock serialization, and cycle/error diagnostics remain unchanged.
- [ ] Public Feature model and typed error behavior remain unchanged.
