# 22 — Implement the specification-facing Docker CLI adapter

**Parent phase:** [Implementation plan §11.4, §12, and Chunk 5](../implementation-plan.md#chunk-5-docker-cli-and-bollard-image-scenario-adapters)  
**Depends on:** [18](18-ports-and-docker-options-planning.md), [20](20-lifecycle-model-and-immutable-plans.md), and [21](21-docker-endpoint-and-process-runner.md)

## Goal

Translate typed image/Dockerfile plans into safe Docker CLI pull, BuildKit build, and create invocations.

## Work

- Add typed operations for image pull, BuildKit build, generated/normal context handling, image tagging as needed, and container creation. Use argument arrays only.
- Apply stable cdenv installation/workspace/generation/profile labels and operation-owned names/tags directly in creation commands.
- Consume validated `runArgs`/`build.options` without reinterpretation; defensively reject reserved conflicts again at the adapter boundary.
- Generate temporary Dockerfiles/context material only under cdenv cache/tmp. Preserve Dockerfile context semantics, `.dockerignore`, contained symlinks, cancellation, and configured size bounds.
- Stream bounded/redacted logs while retaining concise typed IDs/results. Treat CLI output as a claim that later Bollard inspection must verify.
- Do not use this adapter for Compose orchestration, cdenv-owned discovery/control, or attached Exec.

## Rust guidance

Load the `rust-best-practices` skill. Keep command builders pure and heavily tested, borrow plan data, avoid intermediate argv collections where unnecessary, and return adapter-specific `thiserror` variants.

## Acceptance criteria

- Fake CLI tests assert exact pull/build/create arguments for image and Dockerfile fixtures, BuildKit configuration, labels, names, ports, mounts, users, and pass-through option ordering.
- Every reserved option form fails before Docker starts and names the exact argument.
- Context tests prove generated files remain outside the checkout and reject escaping symlinks/oversized material.
- Cancellation terminates the owned process group and preserves a bounded restricted log.
- Optional real-Docker tests produce claimed image/container IDs; ordinary workspace quality commands stay Docker-free and pass.