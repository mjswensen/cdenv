# 38 — Implement live `list`/`status` correlation and rendering

**Parent phase:** [Implementation plan §10.3, §10.7, and §16.2–16.3](../implementation-plan.md#103-list)  
**Depends on:** [08](08-cli-contract-and-output-foundation.md), [11](11-state-schema-fingerprints-and-migrations.md), [23](23-bollard-discovery-and-control.md), [36](36-create-and-up-reconciliation.md), and [37](37-down-and-operation-recovery.md)

## Goal

Report local intent and Docker live truth without repair or N+1 daemon calls.

## Work

- Enumerate local workspaces read-only and issue one `all=true` Docker list query filtered by cdenv identity, then correlate all workspaces in memory.
- Derive every independent status dimension, including missing/partial/stopped/running/ambiguous/Docker unavailable, operation locks, desired-versus-active drift, lifecycle, forwarding, and provision drift.
- Implement deterministic human `list`/`status` output and versioned JSON envelopes retaining warnings and requested/assigned ports.
- `status` includes source/path, selected config/profile, current Git branch when available, fingerprints, scenario/services, lifecycle, ports, container/architecture, remote user/folder, Feature digests, and agent build/protocol.
- Apply exit policies from §10: `list` succeeds when local enumeration succeeds even if Docker fails; `status` emits a valid JSON error envelope and nonzero status for requested-state failures. Build/create drift alone is a warning.
- Never migrate, repair, provision, start a supervisor, or alter mtimes.

## Rust guidance

Load the `rust-best-practices` skill. Use pure status derivation functions, borrow correlation data, keep snapshots small, and avoid formatting logic in core domain types.

## Acceptance criteria

- A many-workspace test observes exactly one Docker list call and deterministic name sorting.
- Matrix tests cover combined status dimensions, stale/duplicate/external resources, corrupt/newer state, Docker unavailable, and lock-held/interrupted operations.
- Human and JSON snapshots are stable, concise, redacted, and JSON stdout is exactly one document even on error.
- Read-only tests verify no file bytes/mtimes or Docker resources change.
- Exit-code tests match the contract; standard workspace quality commands pass.