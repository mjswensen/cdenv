# 10 — Add secure atomic storage and installation identity

**Parent phase:** [Implementation plan §7.3–7.4 and §9.4](../implementation-plan.md#94-atomic-writes)  
**Depends on:** [09](09-root-resolution-and-layout.md)

## Goal

Provide the single safe primitive used to create/update cdenv-managed files, then use it for installation metadata and the fingerprint secret.

## Work

- Implement same-directory atomic replacement: unique `create_new` temp file, final mode before exposure, write/flush/sync, atomic rename, and parent-directory sync where supported.
- Refuse managed symlinks, ownership mismatches, non-regular targets, and unsafe parent components. Clean operation-owned temp files on known failure without deleting unrelated files.
- Create/tighten managed directory and file modes exactly as specified in §7.4; never chmod inside the checkout.
- Implement versioned `installation.json` with stable random installation ID and SSH Include consent (`unknown`, `accepted`, `declined`).
- Create the independent random `0600` `fingerprint.key`; expose keyed-digest capability without logging or serializing the key.
- Detect key loss/replacement so callers can mark plan fingerprints unknown rather than silently trusting old values.

## Rust guidance

Load the `rust-best-practices` skill. Return layered `thiserror` failures, use RAII for temp cleanup, avoid `unwrap`/`expect`, and document security invariants rather than narrating obvious file operations.

## Acceptance criteria

- Fault-injection tests at each write/sync/rename step leave either the old complete file or the new complete file, never partial content.
- Unix permission tests verify `0700`, `0600`, and `0644` policies and refusal of symlink/wrong-owner/wrong-kind targets.
- Reopening preserves installation ID, consent, and fingerprint key; a simulated missing/replaced key reports fingerprints as unknown.
- Temp files are unique under concurrent writers and bounded cleanup never removes foreign files.
- Standard workspace quality commands, including `cargo deny check` if dependencies changed, pass.