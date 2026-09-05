---
id: 25
created: 2026-08-20
depends-on:
  - 20
  - 22
  - 23
  - 24
---

# Orchestrate image and Dockerfile container creation

_Converted from [`implementation-chunks/25-image-scenario-orchestration.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §11.11, §12, and Chunk 5](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#1111-stable-identity-and-discovery)

## Goal

Create a verifiable primary container from an image/Dockerfile plan, without yet running Features or lifecycle provisioning.

## Work

- Evaluate injected host-requirement evidence before destructive mutation.
- Pull/build through Docker CLI, create with stable labels/name/generation, and verify every claimed result through Bollard: ID, image, labels, mounts, users, workspace folder, ports, state, and architecture.
- Track operation-owned candidate resources so known failures can clean them without touching prior active or unrelated resources.
- Implement initial start and direct restart of a recorded unchanged stopped image container through Bollard.
- Detect missing, stale, duplicate, current-generation ambiguous, and external-replacement states; never adopt or choose arbitrarily.
- Return typed creation facts for later provisioning/state commit. Do not write `active` state before callers complete readiness.
- Do not implement Features, Compose, lifecycle hooks, forwarding, SSH, or rebuild rollback here.

## Rust guidance

Load the `rust-best-practices` skill. Keep orchestration a clear cancellable sequence, use narrow statically dispatched adapter traits only where fakes prove useful, and preserve source errors in layered operation errors.

## Acceptance criteria

- Component tests with fakes cover success and every CLI/Bollard mismatch, unsupported architecture, duplicate/external replacement, cancellation, and operation-owned cleanup.
- Optional integration tests create image and Dockerfile fixtures with exact labels and independently inspected mounts/users/image/architecture.
- Attached Exec byte round-trip remains exact after creation.
- Failures never modify an existing active record or remove unrelated/labeled prior resources.
- Standard workspace quality commands pass.