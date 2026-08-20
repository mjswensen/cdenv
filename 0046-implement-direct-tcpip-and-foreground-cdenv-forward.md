---
id: 46
created: 2026-08-20
---

# Implement `direct-tcpip` and foreground `cdenv forward`

_Converted from [`implementation-chunks/46-direct-tcpip-and-ad-hoc-forward.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §10.9, §15.8, and Chunk 15](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-15-direct-forwarding-and-ad-hoc-cdenv-forward)
**Depends on:** 41, 42, 44, and 45

## Goal

Support standard OpenSSH local forwarding and a safe, foreground convenience wrapper.

## Work

- In the agent, accept authenticated `direct-tcpip`, validate representable host/nonzero port, connect asynchronously from inside the container, and bridge both directions with bounded backpressure and clean EOF/cancellation.
- Permit concurrent forwarded connections without guessed quotas. Do not implement reverse, remote, Unix-socket, or agent forwarding.
- Implement all-or-none host preflight for one or more ad-hoc mappings. Detect local conflicts and conflicts with requested/assigned declarative supervisor endpoints; start none on any conflict.
- Default bind to `127.0.0.1` and target to container `localhost`. Require explicit `--bind` for non-loopback and emit a clear security warning.
- Invoke system OpenSSH directly:
  `ssh -F <config> -N -o ExitOnForwardFailure=yes -L ... <name>.cdenv`.
- Keep the command foreground/session-scoped, propagate OpenSSH status/Ctrl+C, and run `postAttachCommand` once for the forwarding-only transport.

## Rust guidance

Load the `rust-best-practices` skill. Use bounded async copy, typed address/mapping values, direct argv builders, and focused network/process tests without dynamic dispatch.

## Acceptance criteria

- Agent tests forward binary data over multiple simultaneous connections and cover refusal, EOF, target failure, backpressure, and cancellation.
- CLI tests verify exact SSH argv, loopback default, explicit non-loopback warning, all-or-none preflight, declarative conflicts, and propagated exit status.
- Integration tests reach primary-container and Compose-network services without config changes/rebuild.
- A forwarding-only connection executes one serialized attach hook and leaves no process/listener after exit.
- No reverse/Unix/agent-forward capability is exposed; standard quality commands pass.