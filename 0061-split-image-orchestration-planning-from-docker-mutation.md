---
id: 61
created: 2026-08-22
depends-on:
  - 63
---

# Split image orchestration planning from Docker mutation

## Description

`crates/cdenv-cli/src/image_orchestration.rs` combines pure existing-container classification and image-plan/runtime-verification decisions with Docker image/container create, start, verification, and rollback mutation. Split these into planning/classification and mutation/rollback modules while retaining the current orchestration boundary and typed errors.

This issue owns orchestration decisions over already-mapped domain facts. Issue 63 owns Bollard endpoint discovery, request/response mapping, and low-level resource operations; do not duplicate those adapter responsibilities here.

## Acceptance criteria

- [ ] Existing-resource classification and runtime/image verification decisions have focused tests independent of Docker mutations.
- [ ] Create/start sequencing, operation-owned cleanup, cancellation, and primary-error-plus-cleanup-error behavior remain covered by focused tests.
- [ ] The orchestrator consumes the inspection/domain boundary from issue 63 rather than remapping daemon responses.
- [ ] Public API, observable operation order, and typed error behavior remain unchanged.
