# 02 — Spike Docker, Compose, lifecycle, and declared forwarding

**Parent phase:** [Implementation plan §5 and Chunk 0](../implementation-plan.md#chunk-0-disposable-feasibility-and-profile-spike)  
**Depends on:** [00](00-feasibility-baseline.md), with profile fixtures from [01](01-feasibility-profile-and-features.md)

## Goal

Validate Docker/Compose ownership boundaries and recoverable lifecycle/forwarding behavior before production orchestration is designed.

## Work

- Resolve one local Unix Docker socket and demonstrate that Docker CLI, Compose V2, and Bollard all use it.
- Prove CLI-created image and Compose primary containers receive and retain the required installation/workspace/generation/profile labels, then verify them through Bollard.
- Generate deterministic Compose overrides. Run two workspace identities from one Compose fixture and prove isolated projects, exact primary-service discovery, managed dependency tracking, complete stop, and named-volume preservation.
- Demonstrate drift-safe resume without accidental Compose reconciliation.
- Exercise lifecycle stage ordering, parallel groups, `waitFor`, checkpoints, detached later stages, cancellation, and definite versus indeterminate recovery.
- Prototype a workspace-scoped forwarding supervisor whose declared listener survives `up` process exit, tolerates temporary target loss, and exits on authenticated `down`.
- Capture exact external command lines and sanitized outputs; never persist interpolated Compose configuration or effective secrets.

## Rust guidance

Load the `rust-best-practices` skill. Prefer static fakes at proven seams, explicit runtime state enums, bounded async channels, and layered `thiserror` errors. Avoid a generic orchestration framework.

## Acceptance criteria

- A Docker-backed spike command proves all relevant [§5.3](../implementation-plan.md#53-spike-success-criteria) Docker, Compose, lifecycle, and forwarding criteria; declared CI environments fail rather than skip when dependencies are absent.
- Two workspaces have distinct project/resource identity, and `down` stops only each persisted managed service set without deleting named volumes.
- A declared forward remains usable after the initiating command exits and is removed by `down`.
- Cancellation tests distinguish safe retry from indeterminate one-time lifecycle work and never duplicate a background stage.
- All retained command/output fixtures are deterministic and redacted.