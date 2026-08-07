# 35 — Orchestrate lifecycle stages and readiness on the host

**Parent phase:** [Implementation plan §10.4, §11.6, and Chunk 9](../implementation-plan.md#chunk-9-lifecycle-orchestration-and-core-cli-environment-flows)  
**Depends on:** [25](25-image-scenario-orchestration.md), [28](28-generated-feature-and-uid-images.md), [30](30-compose-managed-lifecycle.md), [32](32-agent-tool-free-provisioning.md), [33](33-effective-remote-environment.md), and [34](34-agent-lifecycle-runner.md)

## Goal

Coordinate host/container lifecycle work through the configured `waitFor` readiness boundary without yet implementing full CLI reconciliation.

## Work

- Execute `initializeCommand` on the host checkout before container mutation for create/up/rebuild; allow reruns and use exact command-form/stdin semantics.
- For a new generation, execute `onCreateCommand`, `updateContentCommand`, `postCreateCommand`, and actual-start-only `postStartCommand` in specification order.
- Honor `waitFor`. After the selected stage succeeds, always upload/provision the expected agent, capture/recapture environment, invoke a forwarding-readiness seam, and only then report `up` readiness.
- Start or verify the background runner for later stages; never launch duplicates.
- Persist no active generation here. Return an explicit readiness result/checkpoints so the command transaction can atomically commit later.
- On cancellation/failure, classify safe retry, definite failure, background failure, and indeterminate one-time execution; later stages never run after failure.
- Leave `postAttachCommand` to the proxy transport chunk.

## Rust guidance

Load the `rust-best-practices` skill. Keep the sequence readable with explicit loops/steps, introduce only narrow statically dispatched seams proven by fakes, and preserve layered errors rather than flattening them.

## Acceptance criteria

- Fake-adapter component tests assert exact ordering for each `waitFor` value and scenario.
- Tests prove agent upload happens on every successful readiness path, `postStartCommand` runs only after an actual start, and background work is never duplicated.
- Failure/cancellation tests assert the correct checkpoint/retry classification and no active-state commit.
- Environment is recaptured after readiness, and forwarding failure keeps the environment running but makes readiness unsuccessful as specified.
- Standard workspace quality commands pass.