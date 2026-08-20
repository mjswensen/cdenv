---
id: 40
created: 2026-08-20
---

# Implement the agent bridge and scoped forwarding supervisor

_Converted from [`implementation-chunks/40-agent-bridge-and-forwarding-supervisor.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.9 and Chunk 10](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#119-declared-ports-and-forwarding-supervisor)
**Depends on:** 24, 31, 32, and 37

## Goal

Provide the private process/protocol that can own declared host listeners after `up` exits.

## Work

- Implement an agent forwarding bridge that connects from the primary container to validated `localhost` or Compose-service host/port targets and streams bytes with bounded backpressure.
- Implement a detached, per-workspace host supervisor using a private runtime control socket/state/lifetime lock under the workspace root; do not expose it as a public V1 command.
- Validate installation, workspace, generation, host build, agent build, and protocol identity before accepting control or forwarding traffic.
- Own prebound loopback TCP listeners and forward each connection through the verified active container/agent transport.
- Use authenticated/private control messages for status and `down`; never signal by reused PID alone.
- Keep listeners while the container is temporarily unavailable, retry with bounded backoff/logging, and exit on authenticated down or incompatible replacement.
- Add no systemd/launchd integration, global daemon, or reboot autostart.

## Rust guidance

Load the `rust-best-practices` skill, especially async/thread-safety guidance. Use structured concurrency, bounded channels, explicit runtime state, narrow static transport seams, and typed protocol errors.

## Acceptance criteria

- Process-level tests prove the supervisor outlives its starter, holds listeners, authenticates control calls, and exits cleanly on `down`.
- Tests reject wrong root/workspace/generation/build/protocol and demonstrate PID reuse cannot target another process.
- Forwarded binary streams are exact under concurrent clients, EOF, backpressure, cancellation, and temporary target loss.
- Crash/restart and bounded-log/backoff tests leave no global daemon or unrelated listener.
- Standard workspace quality commands and opt-in Docker bridge tests pass.