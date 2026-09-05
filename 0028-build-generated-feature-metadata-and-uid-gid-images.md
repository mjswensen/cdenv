---
id: 28
created: 2026-08-20
depends-on:
  - 22
  - 25
  - 26
  - 27
  - 31
  - 53
  - 54
  - 55
  - 56
  - 57
  - 58
  - 60
---

# Build generated Feature, metadata, and UID/GID images

_Converted from [`implementation-chunks/28-generated-feature-and-uid-images.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §11.4–11.5 and Chunk 6](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-6-features-cache-lockfile-and-generated-images)

## Goal

Turn verified ordered Features and user-update intent into deterministic derived images outside the checkout.

## Work

- Generate build material under cdenv cache/tmp only. Install each ordered Feature as root in its own image layer and merge its environment, mounts, capabilities, entrypoints, lifecycle hooks, and metadata exactly once.
- Carry final `devcontainer.metadata` and cdenv ownership/retention labels on generated images.
- Implement the pinned Linux `updateRemoteUserUID` rules: resolve the effective account, derive host UID/GID without persistence, preserve root/conflicting-account invariants, and fail precisely when safe mutation is impossible.
- Use the architecture-matched static helper artifact supplied and verified by 31 together with Rust/Linux filesystem APIs; do not assume `usermod`, `groupmod`, shell, or distro utilities exist. Issue 31 must land first so this issue does not invent a second artifact pipeline.
- Integrate generated Dockerfiles/contexts with the Docker CLI adapter and verify final image identity/architecture through Bollard.
- Mark only cdenv-generated workspace images as eligible for later bounded cleanup; do not prune base/repository-tagged images or BuildKit cache.

## Rust guidance

Load the `rust-best-practices` skill. Keep generated-plan code pure where possible, avoid redundant material copies, use typed account/conflict errors, and isolate any platform-specific operations behind safe APIs.

## Acceptance criteria

- Reviewed generated-Dockerfile snapshots are deterministic for dependency/options/order and contain no host secret values.
- Debian and Alpine integration fixtures pass with root, named remote users, UID/GID update/no-update, conflict, and both supported architectures.
- Each Feature’s install/contributions occur exactly once and the final metadata label matches the effective plan.
- Generated material never appears in the checkout; only correctly labeled generated images are cleanup candidates.
- Frozen/offline Feature builds use the remediated source/cache/lock behavior from 53–57, generated-context handling preserves `.dockerignore` semantics from 58, and all standard workspace quality commands pass.