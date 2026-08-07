# 03 — Spike the OpenSSH-to-agent packet flow

**Parent phase:** [Implementation plan §5.1, §15, and Chunk 0](../implementation-plan.md#51-required-packet-flow)  
**Depends on:** [00](00-feasibility-baseline.md) and a runnable Docker fixture from [02](02-feasibility-docker-compose-lifecycle.md)

## Goal

Prove the complete high-risk transport before building production SSH or proxy modules.

## Work

Implement the disposable vertical path:

```text
OpenSSH → ProxyCommand → Bollard Exec attach → cdenv-agent/Russh over stdio
```

Prove:

- Docker multiplexed frames are decoded internally and stdout contains only exact SSH protocol bytes; diagnostics stay on stderr.
- Bollard Exec supports bidirectional, binary-clean, cancellation-aware streaming with bounded backpressure and correct EOF propagation.
- Russh can serve a generic async stream and authenticate an exact Ed25519 client key/user.
- System OpenSSH can execute a command, run a PTY shell, resize it, deliver Ctrl+C/signals, and open `direct-tcpip` channels.
- `ControlMaster` supports concurrent exec/PTY/forward channels and a long-lived no-session master.
- Disconnect cleanup does not leave a permanent SSH daemon and intentionally detached helpers can survive.

Use a test-only host key and client key; do not design production key storage in this spike.

## Rust guidance

Load the `rust-best-practices` skill, especially its async error, pointer/thread-safety, testing, and unsafe guidance. Keep any PTY unsafe code isolated and justify every unsafe block with `// SAFETY:`.

## Acceptance criteria

- An automated OpenSSH script verifies exact stdout, stderr, and exit status through the full packet path.
- Binary payload and long-lived stream tests show no Docker framing or tracing bytes on protocol stdout.
- Automated tests cover ControlMaster concurrency, PTY resize, Ctrl+C, direct forwarding, disconnect cleanup, and cancellation.
- The container has no `sshd`, published port 22, or persistent agent SSH process after clients disconnect.
- Packet-flow, API limitations, cancellation behavior, and selected Russh/Bollard APIs are captured for Chunk 04.