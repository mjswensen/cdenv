---
id: 70
created: 2026-09-07
depends-on:
  - 69
---

# Wire production down rebuild and recovery workflows

**Split from:** 68 — production prerequisite for broker lifetime and generation handoff.

## Goal

Connect the existing stop, rebuild, rollback, and interrupted-operation coordinators to the public `cdenv down` and `cdenv rebuild` commands for image/Dockerfile and Compose workspaces.

## Context

The real command dispatcher still returns `CommandUnavailable` for these commands. Issues 37, 47, and 48 supplied library coordination and test seams, not the complete executable composition. Reuse that work rather than redefining its preservation or partial-replacement contracts.

## Work

- Construct production adapters for `down_workspace`, image/Dockerfile rebuild, and Compose rebuild. Honor selected root/name/config, cached versus `--no-cache` builds, cancellation, output, and application error handling.
- Stop only verified workspace services: authenticated supervisor resources, then eligible lifecycle work, then the exact recorded container/Compose managed set. Repeated down must be idempotent where identity is known; ambiguity or unverified control state must not select or signal arbitrary resources.
- Build and validate replacement inputs before destructive mutation. Require a new primary ID and generation, re-establish agent/SSH/environment/lifecycle/forwarding readiness, and commit only after success.
- Preserve image-scenario rollback, operation-owned candidate cleanup, stable SSH identity, and warning-only post-success cleanup. Preserve Compose's intentionally non-atomic partial/interrupted recovery evidence rather than claiming project-wide rollback.
- Retain tracked/untracked checkout changes, named-volume data, user images, project networks where required, and unrelated services. Do not use broad prune, rewrite Git configuration, or remove credential permission records on down.
- Expose the verified active/candidate ownership and stop/handoff seams issue 77 needs. Do not pretend credential leases exist before the broker integration lands.

## Acceptance criteria

- [ ] Public `down` and `rebuild` invoke the production workflows rather than returning `CommandUnavailable`; tests cover the executable path.
- [ ] Image/Dockerfile and multi-service Compose fixtures pass create/down/up and cached/no-cache rebuild through installed or production binaries.
- [ ] Rebuild verifies a different primary container ID and incremented generation, reruns required readiness only, and retains the workspace SSH host key.
- [ ] Failure/cancellation at each stop/rebuild phase results in verified rollback or precise partial/interrupted state, never arbitrary container adoption or an unsafe lifecycle retry.
- [ ] Checkout, named volumes, configured credential grants, and unrelated services survive. Cleanup is restricted to verified operation-owned resources.
- [ ] Existing preservation, OpenSSH/profile, and strict workspace quality gates pass. Documentation and installed-smoke expectations now reflect the real command composition.

## Boundaries and implementation guidance

Issue 69 owns create/up composition. Issue 77 adds credential-stream stop, pre-hook candidate authorization, and broker lease handoff/rollback to these production paths. Load the Rust best-practices skill; use explicit recovery states, bounded cancellation, and narrow production adapters for existing seams.
