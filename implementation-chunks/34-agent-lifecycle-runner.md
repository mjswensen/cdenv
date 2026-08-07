# 34 — Implement lifecycle execution, checkpoints, and the background runner

**Parent phase:** [Implementation plan §11.6 and Chunk 8](../implementation-plan.md#116-lifecycle-execution-and-recovery)  
**Depends on:** [20](20-lifecycle-model-and-immutable-plans.md), [32](32-agent-tool-free-provisioning.md), and [33](33-effective-remote-environment.md)

## Goal

Run one generation’s immutable container lifecycle plan recoverably, including stages that outlive `up`.

## Work

- Implement agent lifecycle execution: strings via applicable `/bin/sh`, arrays as direct argv, and object entries concurrently with all required to succeed.
- Apply the captured effective environment and authoritative workspace folder/user. Parallel object and background stages receive closed stdin; synchronous scalar forms support the caller’s allowed stdin mode.
- Prefix multiplexed logs by stable object key without altering child byte streams. Keep restricted logs/checkpoints bounded.
- Store generation/build/protocol identity, immutable lifecycle/runtime plan, before/running/after checkpoints, failure, and indeterminate status inside the container.
- Implement `lifecycle-runner` for stages after `waitFor`, including intentional long-running healthy commands, stop-on-failure ordering, and eventual exit.
- Add a host control API to inspect/verify an existing runner so repeated `up` never duplicates commands.
- Support graceful cancellation followed by bounded TERM/KILL cleanup and distinguish definite from indeterminate one-time execution.

## Rust guidance

Load the `rust-best-practices` skill. Use explicit runtime state enums, structured concurrency and bounded channels, `Send + Sync + 'static` errors where tasks require it, and no panics in production paths.

## Acceptance criteria

- Tests cover every command form, order, Feature-before-repository input, environment/cwd/user, closed stdin, log prefixing, and exit propagation.
- Checkpoint fault tests cover crash before start, while running, after success, failure, restart verification, and indeterminate cancellation.
- Repeated runner-start requests execute each one-time command at most once.
- Long-running later commands remain reported healthy/running; a later failure skips subsequent stages.
- Cancellation leaves no owned child except a process intentionally detached under documented rules; standard quality commands pass.