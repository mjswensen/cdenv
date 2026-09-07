---
id: 69
created: 2026-09-07
depends-on: []
---

# Wire production create and up lifecycle workflows

**Split from:** 68 — production prerequisite, not credential forwarding itself.

## Goal

Connect the existing planning, Docker, provisioning, lifecycle, and forwarding coordinators to the real `cdenv create` and `cdenv up` command paths. A production invocation must reach a verified, usable environment rather than stopping at checkout creation or returning `CommandUnavailable`.

## Context

At source HEAD `6814624`, `crates/cdenv-cli/src/lib.rs::invoke_with_root` runs only checkout creation for `create`; `up` has no production workflow. The coordinators delivered under issues 25, 28, 30, 32–36, and 41 exist behind library/test seams. Their closed issue status is not evidence that the executable composes them. Issue 68's working-tree permission/parser foundation does not fill this gap.

Reuse those coordinators and their commit/recovery boundaries. Do not implement a second simplified lifecycle path or silently drop supported image, Dockerfile, Feature, or Compose behavior.

## Work

- Construct production planner/runtime adapters and connect `create` after successful clone to the same reconciliation flow used by `up`. Honor root/name/config selection, explicit desired-state persistence, dependency preflight, cancellation, and typed application errors.
- Compose frozen Feature resolution, generated images/UID handling, Docker CLI/Compose operations, Bollard verification, host `initializeCommand`, agent/SSH asset provisioning, effective environment capture, lifecycle execution/checkpoints, and declared forwarding readiness.
- Cover initial creation, direct restart of recorded stopped containers, drift-safe Compose resume, and idempotent reconciliation of an already-running generation. Build/create drift must not become an implicit rebuild; existing-container drift inspection must not fetch Feature sources unnecessarily.
- Retain the successful checkout and recoverable state after every post-clone failure. Commit active state only after readiness, and preserve existing one-time/indeterminate lifecycle retry semantics.
- Wire SSH setup/consent and output/exit handling through the real executable without duplicate hooks or incidental protocol output.
- Provide a narrow early workspace-service readiness seam for issue 77. Until credential integration is available, configured capabilities must produce an explicit integration-unavailable failure before container lifecycle execution; do not guess which hooks need credentials or treat saved consent as installed integration. Capabilities-off behavior is this issue's baseline.

## Acceptance criteria

- [ ] Public `create` and `up` no longer fall through to `CommandUnavailable` or a checkout-only success. Installed/binary-level tests reach the production composition, not just fake orchestrator implementations.
- [ ] Image, Dockerfile/Feature, and Compose fixtures exercise successful creation, already-running idempotence, stopped-container resume, and configuration selection/drift with exact resource verification.
- [ ] Agent provisioning, readiness and post-readiness environment capture, foreground/detached lifecycle work, and declared forwarding are actually called in the documented order; resulting SSH access uses the committed generation.
- [ ] Fault/cancellation tests retain checkout/named-volume data and previous valid active state, diagnose duplicate/replaced containers, and do not replay indeterminate one-time work.
- [ ] Host clone authentication and host `initializeCommand` retain their ordinary behavior; this issue does not introduce a host credential cache, secret mount, or repository mutation.
- [ ] `cargo xtask check` and applicable production-command integration tests pass. Runtime documentation accurately distinguishes any remaining unwired commands, which issue 70 owns.

## Boundaries and implementation guidance

Issue 70 owns production `down`/`rebuild`; issue 77 owns credential-specific early readiness and generation leases. Load the Rust best-practices skill. Keep host I/O out of `cdenv-devcontainer`, preserve lock ordering and typed failures, and retain fake adapter coverage alongside real executable tests.
