---
id: 62
created: 2026-08-22
---

# Split Feature parsing and resolution responsibilities

## Description

`crates/cdenv-devcontainer/src/feature.rs` combines Feature reference parsing, metadata validation, option normalization, dependency ordering, and lock serialization. Split the concrete parsing/metadata boundary from dependency resolution and frozen-lock encoding, retaining pure APIs and descriptive unit tests.

## Acceptance criteria

- [ ] Reference and metadata parsing are independently testable from graph resolution.
- [ ] Dependency resolution and lock encoding have focused tests at their boundaries.
- [ ] Public Feature model and error behavior remain unchanged.
