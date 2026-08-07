# 51 — Implement read-only `doctor`

**Parent phase:** [Implementation plan §10.12 and Chunk 18](../implementation-plan.md#1012-doctor)  
**Depends on:** [38](38-list-status-and-output.md), [41](41-declarative-forwarding-reconciliation.md), [42](42-ssh-identity-config-and-wrapper.md), [48](48-compose-rebuild-and-partial-recovery.md), [49](49-devcontainer-profile-release-gate.md), and [50](50-openssh-release-gate.md)

## Goal

Diagnose every required local/profile/runtime invariant without repairing anything.

## Work

Implement typed checks for:

- root ownership/modes/symlinks, installation/state schemas, installation ID, consent, and stale foreground operations;
- pinned profile/vendored schema identity and support revision;
- Git, OpenSSH, Docker Engine/CLI, Compose versions, Unix socket consistency, and Bollard connectivity;
- managed SSH syntax/Include visibility, client/host keys, known hosts, executable path, and permissions;
- both embedded agent artifacts, formats, architectures, build IDs, and protocol;
- duplicate/stale containers/projects, generated image/cache retention, Feature lock integrity, desired drift, and provision drift;
- lifecycle runner, forwarding supervisor/control identity/listeners/assignments/target health.

Use command-scoped checks where possible and continue independent checks after failures. Emit deterministic human output and a versioned JSON envelope; exit nonzero when a required invariant fails. Never chmod, migrate, regenerate, start, stop, delete, or repair.

## Rust guidance

Load the `rust-best-practices` skill. Model check outcomes as data, borrow shared inspection results, avoid one catch-all error, and keep output snapshots small and deterministic.

## Acceptance criteria

- Tests cover pass/warn/fail/not-checkable outcomes for every check category and exact exit policy.
- One Docker discovery result is reused across workspace checks; failures do not prevent independent local checks.
- Human and JSON output are redacted, deterministic, and JSON stdout is one valid document on failure.
- Before/after tests prove `doctor` changes no file bytes/mtimes, processes, listeners, or Docker resources.
- Corrupt/newer schemas and missing dependencies are reported, never rewritten; standard quality commands pass.