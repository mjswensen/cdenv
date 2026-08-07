# 37 — Implement `down` and interrupted-operation recovery

**Parent phase:** [Implementation plan §10.5, §9.5, and Chunk 9](../implementation-plan.md#105-down)  
**Depends on:** [12](12-workspace-locking-and-reservation.md), [30](30-compose-managed-lifecycle.md), [34](34-agent-lifecycle-runner.md), and [36](36-create-and-up-reconciliation.md)

## Goal

Stop the full managed environment predictably while preserving checkout, containers, networks, and named volumes for resume.

## Work

- Acquire the exclusive workspace lock and persist an explicit stopping operation.
- Call a forwarding-supervisor stop seam first; missing/degraded supervisors should be diagnosed without stopping unrelated processes.
- Request background lifecycle cancellation, wait a bounded grace period, and force cleanup when required while recording definite versus indeterminate outcomes.
- Stop image/Dockerfile primary containers through Bollard. Stop the persisted Compose managed service set through Compose V2, regardless of `shutdownAction`.
- Preserve checkout, state intent, containers, project networks, images, and named volumes. Never stop an unrelated service manually started later.
- Interpret stale persisted foreground operations using lock availability. Recover only transitions with known outcomes; retain interrupted/last-error status otherwise.
- Make repeated `down` idempotent against already-stopped or known-missing resources while still failing on ambiguity.

## Rust guidance

Load the `rust-best-practices` skill. Use RAII locking, clear side-effectful cancellation loops, layered stop errors, and tests that exercise errors as first-class outcomes.

## Acceptance criteria

- Tests assert stop order: forwarding, lifecycle runner, then complete managed environment.
- Image and Compose integration tests preserve checkout changes and named-volume data across repeated down/up.
- Unrelated Compose services remain running; ambiguous duplicate resources fail rather than selecting one.
- Crash/lock tests distinguish active operation from stale/interrupted state and never rewrite state from read-only code.
- Repeated `down` is safe, and all standard workspace quality commands pass.