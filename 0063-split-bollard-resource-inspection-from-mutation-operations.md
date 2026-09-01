---
id: 63
created: 2026-08-22
---

# Split Bollard resource inspection from mutation operations

**Blocks:** 61

## Description

`crates/cdenv-cli/src/bollard.rs` combines daemon discovery/ping, container discovery, inspection mapping, verification, and image/container start, stop, rename, upload, and cleanup operations. Keep the existing `exec` split and separate read-side discovery/inspection mapping from control and mutation operations so exact requests, mappings, and cleanup guards are auditable independently.

This issue owns the Bollard adapter and daemon-to-domain mapping only. Issue 61 owns higher-level image orchestration classification, sequencing, cancellation, and rollback decisions.

## Acceptance criteria

- [ ] Endpoint/ping, discovery filters, correlation inputs, and inspect response mapping have focused tests independent of mutations.
- [ ] Start/stop/rename/upload and guarded container/image cleanup have focused exact-request and refusal-path tests.
- [ ] Shared timeout/response/error handling has one clear owner and is not duplicated across the split.
- [ ] The existing `exec` module boundary remains intact.
- [ ] Public adapter API and typed error behavior remain unchanged.
