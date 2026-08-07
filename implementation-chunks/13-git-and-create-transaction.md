# 13 — Implement Git handling and the durable create transaction

**Parent phase:** [Implementation plan §10.1–10.2, §16.5, and Chunk 3](../implementation-plan.md#chunk-3-git-and-durable-create-transaction)  
**Depends on:** [08](08-cli-contract-and-output-foundation.md), [10](10-atomic-storage-and-installation-identity.md), [11](11-state-schema-fingerprints-and-migrations.md), and [12](12-workspace-locking-and-reservation.md)

## Goal

Create a workspace checkout safely and leave a recoverable state before environment orchestration exists.

## Work

- Add command-scoped Git dependency/version detection and a subprocess adapter that passes arguments directly, preserves interactive credential prompts, supports process-group cancellation, and writes bounded restricted logs.
- Pass the original clone source only to Git, after `--`; never invoke a shell or log the raw argument.
- Store/display a sanitized source: remove HTTP userinfo/query/fragment, preserve ordinary SSH usernames, and canonicalize local paths.
- Derive or validate the name, reserve it atomically, write operation `creating`, and clone into `checkout/<name>/`.
- Validate explicit config paths as UTF-8 repository-relative regular files canonically contained within the checkout, rejecting `..` and symlink escapes.
- On clone failure/cancellation, remove only the incomplete workspace and retain a sanitized global operation log. After clone, retain checkout/state/error on failure or cancellation.
- End this chunk with a valid workspace whose environment may be `missing`; later `create` will call shared `up` orchestration.

## Rust guidance

Load the `rust-best-practices` skill. Borrow paths/strings in adapter APIs, use layered Git/create errors, use loops for cancellable I/O, and share test setup rather than action/assertion helpers.

## Acceptance criteria

- Tests clone local normal and bare repositories without network access and verify the exact checkout layout and sanitized state.
- A fake Git executable verifies argument boundaries, no shell use, command-scoped dependency checks, exit/cancellation propagation, and raw-source redaction.
- Fault tests prove pre-clone failures leave no workspace, while every post-clone failure retains checkout and recoverable state.
- Containment tests reject absolute, missing, non-UTF-8, `..`, and symlink-escaping config selections.
- Create does not modify Git state after clone, and standard workspace quality commands pass.