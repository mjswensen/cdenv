# 12 — Add workspace locking, reservation, and read-only enumeration

**Parent phase:** [Implementation plan §9.5 and Chunk 2](../implementation-plan.md#95-locking)  
**Depends on:** [09](09-root-resolution-and-layout.md), [10](10-atomic-storage-and-installation-identity.md), and [11](11-state-schema-fingerprints-and-migrations.md)

## Goal

Make concurrent workspace operations deterministic without an installation-wide daemon.

## Work

- Wrap `std::fs::File` shared/exclusive locking in one small module with RAII guards and typed contention/errors; do not add `fs2`.
- Reserve names atomically under a brief global namespace lock, creating only the operation-owned workspace skeleton and rolling it back safely when reservation fails.
- Use `workspaces/<name>/.lock` for full-duration exclusive create/up/down/rebuild/lock operations.
- Support the proxy pattern: acquire a shared lock only through local resolve/live inspect/Exec attach and release it before streaming. Add a fail-fast acquisition mode for proxy/editor diagnostics.
- Interpret persisted foreground operations as active only while the exclusive lock is unavailable. If the lock is available, report an interrupted previous operation; do not auto-rewrite it from read-only code.
- Enumerate workspace directories and read states deterministically without migration persistence or repair.
- Define private runtime socket/state path ownership checks for future supervisors; do not implement their protocol yet.

## Rust guidance

Load the `rust-best-practices` skill. Use RAII instead of manual unlock paths, typed errors, clear side-effectful loops, and process-level tests rather than mocks for OS lock semantics.

## Acceptance criteria

- Multi-process tests prove exclusive/exclusive contention, shared/shared success, fail-fast proxy behavior, and release after normal exit and process death.
- Concurrent reservation of one name produces exactly one winner and never deletes the winner’s files.
- Read-only enumeration is sorted, does not alter bytes or mtimes, and reports corrupt/newer/interrupted state.
- Existing simulated SSH sessions do not hold lifecycle locks after attach setup.
- No non-standard locking crate is present; standard workspace quality commands pass.