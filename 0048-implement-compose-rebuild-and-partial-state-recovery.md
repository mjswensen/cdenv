---
id: 48
created: 2026-08-20
depends-on:
  - 30
  - 41
  - 47
---

# Implement Compose rebuild and partial-state recovery

_Converted from [`implementation-chunks/48-compose-rebuild-and-partial-recovery.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §10.6, §11.10, and Chunk 16](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-16-rebuild-rollback-and-cleanup)

## Goal

Perform build-first Compose replacement while accurately representing its intentionally non-atomic failure modes.

## Work

- Resolve/build the complete desired Compose plan, Features, and UID-derived primary image before replacing services; pass Compose no-cache only for `--no-cache`.
- Force-recreate/reconcile the isolated project using explicit files/project/endpoint and the final primary image with build disabled at start.
- Require and verify a replacement primary ID when one previously existed, increment generation, rediscover the managed set, and fully rerun lifecycle/provision/environment/SSH/forwarding readiness.
- Treat project-wide replacement as non-atomic. On cancellation/failure, inspect all services and persist precise partial/interrupted/drift status rather than claiming rollback.
- Remove orphaned project containers only after desired managed services are healthy. Preserve named volumes, project data, unrelated manually started services, and checkout.
- Apply the same bounded generated-image cleanup and warning-only post-success cleanup policy as image rebuild.

## Rust guidance

Load the `rust-best-practices` skill. Use explicit partial-state values, clear side-effectful loops, static adapter fakes, and error tests that retain the failing service/phase without leaking Compose secrets.

## Acceptance criteria

- Multi-service fault tests cover failure/cancellation before recreation, during partial recreation, before health, and during post-success cleanup.
- Integration tests prove new primary ID/generation, complete managed set, lifecycle/provisioning, forwarding handoff, and stable SSH host key.
- Named-volume data and checkout changes survive; unrelated services are untouched.
- Orphans are removed only after health and no interpolated Compose model appears in logs/state.
- Cached/no-cache behavior, interrupted recovery, and standard workspace quality commands pass.