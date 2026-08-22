---
id: 61
created: 2026-08-22
---

# Split image orchestration planning from Docker mutation

## Description

`crates/cdenv-cli/src/image_orchestration.rs` combines pure existing-container classification and image-plan decisions with Docker image/container create, start, verification, and rollback mutation. Split these into planning/classification and mutation/rollback modules while retaining the current orchestration boundary and typed errors.

## Acceptance criteria

- [ ] Pure existing-resource classification has focused tests independent of Docker mutations.
- [ ] Docker mutation and rollback ownership remains covered by focused tests.
- [ ] Public API and error behavior remain unchanged.
