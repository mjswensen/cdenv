# 41 — Reconcile declared forwarding plans transactionally

**Parent phase:** [Implementation plan §10.4, §11.8–11.9, and Chunk 10](../implementation-plan.md#chunk-10-declarative-port-publication-and-forwarding-supervisor)  
**Depends on:** [30](30-compose-managed-lifecycle.md), [36](36-create-and-up-reconciliation.md), [38](38-list-status-and-output.md), and [40](40-agent-bridge-and-forwarding-supervisor.md)

## Goal

Make configuration-declared forwards stable from successful `up` until explicit `down` and safely update them as runtime drift.

## Work

- Resolve each `forwardPorts` target to primary-container `localhost` or the declared Compose service host.
- Prebind all new/changed listeners, retain unchanged assignments, switch only when the full viable plan is ready, then release obsolete listeners. On failure, retain the previous complete plan.
- Reuse requested/assigned ports for an active generation where available. If occupied and `requireLocalPort=false`, choose an available loopback port; if true, leave the environment running but make `up` nonzero/degraded.
- Reject privileged local ports with guidance; never silently elevate. Configuration-declared forwards bind loopback only.
- Integrate supervisor start/verify/replace with `up`, stop with `down`, and status/list/doctor data. Read-only paths and proxy diagnose but never repair.
- Handle target loss, supervisor crash, build/protocol mismatch, and host reboot; a later `up` restores service without a global startup mechanism.
- Render requested versus assigned endpoints, labels, protocol URLs, and deferred UI/discovery warnings.

## Rust guidance

Load the `rust-best-practices` skill. Model reconciliation outcomes explicitly, keep listener ownership RAII-based, avoid cloning plan maps, and use focused process/network tests.

## Acceptance criteria

- Declared forwards remain reachable after `up` exits, survive transient target loss, and release on `down`.
- Tests cover alternate assignment, required-port degradation, stable reuse, all-or-nothing update, rollback to prior plan, Compose targets, conflicts, crash, mismatch, and reboot/`up` recovery.
- Read-only commands never spawn/replace a supervisor.
- Human/JSON output distinguishes requested and assigned endpoints and security/degradation warnings.
- Integration and standard workspace quality commands pass.