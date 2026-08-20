---
id: 47
created: 2026-08-20
---

# Implement image/Dockerfile rebuild, rollback, and cleanup

_Converted from [`implementation-chunks/47-image-rebuild-rollback-and-cleanup.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §10.6 and Chunk 16](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-16-rebuild-rollback-and-cleanup)
**Depends on:** 27, 36, 41, and 46

## Goal

Replace an image/Dockerfile generation with best-effort rollback while preserving checkout and stable SSH identity.

## Work

- Recompute/validate the full desired plan and frozen lock before destructive mutation. Map `--no-cache` to Docker no-cache behavior; cached rebuild is default.
- Run `git status --porcelain` only to print whether changes exist; never block or expose filenames.
- Build the replacement image before stopping active resources.
- Rename the old container to a private operation-owned backup, create/start/verify a replacement, and restore the old name/container on failure where feasible.
- Require a different primary container ID when one existed, increment generation, and run complete lifecycle readiness, agent/SSH/environment provisioning, and transactional forwarding handoff.
- Commit active state only after success, then remove operation-owned backup/candidates and only unreferenced cdenv-generated workspace images outside a small history window.
- Never prune base/repository-tagged images, named volumes, unrelated BuildKit cache, or checkout content. Cleanup failure is a warning after successful replacement.

## Rust guidance

Load the `rust-best-practices` skill. Model rollback phases explicitly with runtime enums/owned resources, use RAII cleanup guards carefully, preserve layered errors, and avoid clever typestate across persisted recovery.

## Acceptance criteria

- Component tests fault every phase and verify rollback/restoration or precise interrupted state with no arbitrary adoption.
- Integration tests prove tracked/untracked checkout changes survive, generation/ID changes, lifecycle/provisioning reruns, forwards hand off, and SSH host key stays stable.
- `--no-cache` and cached command arguments are exact; dirty-worktree output never contains filenames.
- Cleanup tests retain bounded history and never remove prohibited resources.
- Cancellation/retry/rebuild-only lifecycle recovery and standard quality commands pass.