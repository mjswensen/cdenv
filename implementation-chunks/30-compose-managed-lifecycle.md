# 30 — Manage Compose service sets, isolation, and drift-safe resume

**Parent phase:** [Implementation plan §10.5, §11.10, and Chunk 7](../implementation-plan.md#chunk-7-compose-v2-orchestration)  
**Depends on:** [29](29-compose-planning-and-adapter.md)

## Goal

Create, start, stop, and resume exactly the cdenv-managed Compose environment while preserving volumes and unrelated services.

## Work

- Determine the managed set as configured `runServices`, or all configured services by default, plus dependencies Compose actually starts and services recorded during creation.
- Create/start/reconcile only during initial creation or explicit rebuild. Record the verified primary ID and full managed set.
- On ordinary unchanged resume, start recorded containers as needed. When build/create drift exists, directly start recorded existing containers instead of allowing Compose to recreate/add services.
- Implement explicit `down` by stopping the persisted managed set through Compose V2, regardless of `shutdownAction`; preserve containers, project networks, named volumes, and checkout.
- Never stop a project service manually started later and not in the persisted managed set.
- Report partially running/missing/ambiguous service sets and interrupted operations precisely; do not silently repair read-only paths.

## Rust guidance

Load the `rust-best-practices` skill. Use clear cancellable orchestration loops, narrow static adapter seams, runtime enums for partial state, and tests that keep action/assertions visible.

## Acceptance criteria

- Two-workspace integration tests prove project/resource isolation from the same Compose file.
- Multi-service tests prove default/all and explicit `runServices` behavior, dependency recording, complete managed stop/resume, and unrelated manually started service preservation.
- Named volumes and data survive repeated create/down/up cycles.
- Drift tests prove ordinary `up` never invokes Compose reconciliation when build/create drift exists.
- Component tests cover partial/ambiguous/missing services, cancellation, and exact persisted managed sets; all standard quality commands pass.