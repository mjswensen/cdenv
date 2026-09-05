---
id: 43
created: 2026-08-20
depends-on:
  - 31
  - 33
  - 42
---

# Implement agent SSH authentication and non-PTY exec

_Converted from [`implementation-chunks/43-agent-ssh-auth-and-exec.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §15.1–15.6 and Chunk 12](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-12-agent-ssh-handshake-authentication-and-exec)

## Goal

Serve authenticated SSH over a generic stdio stream with exact command/output semantics.

## Work

- Expose a generic async stream entry point used by in-memory tests and Tokio stdin/stdout production wiring.
- Load the persistent workspace host key and accept only synthetic username `cdenv` with the exact provisioned client public key. Reject password and keyboard-interactive authentication.
- Enforce stdout discipline: `ssh-server --stdio` writes only protocol data to stdout; tracing is non-ANSI stderr.
- Model each channel with runtime state `Pending -> RunningProcess` or future `Forwarding`; reject duplicate/invalid shell/exec transitions.
- Start from the captured effective environment. Accept only `LANG`, `LC_*`, `TERM`, `COLORTERM`, and `NO_COLOR`; add synthetic SSH variables and exact `SSH_ORIGINAL_COMMAND`.
- Use the recorded workspace folder and resolve shell from passwd, captured `SHELL`, then `/bin/sh`.
- For non-PTY exec, invoke `<shell> -c <exact command string>` with the command as one argument; bridge stdin/stdout/stderr and report exit status/signal exactly.
- Bound channel buffers without guessed channel-count quotas.

## Rust guidance

Load the `rust-best-practices` skill. Use runtime enums rather than typestate for protocol requests, structured async tasks, `Send + Sync` errors, borrowed environment data, and no stdout-oriented logging in server code.

## Acceptance criteria

- In-memory protocol tests cover valid/invalid user/key/auth methods and host-key behavior.
- Exec tests verify exact argv/string semantics, cwd/environment allowlist, binary stdin/stdout, separate extended stderr, EOF, nonzero status, signal, and cancellation.
- Multiple concurrent non-PTY channels remain isolated and bounded.
- A source/test guard detects stdout printing/logging reachable from `ssh-server` outside the protocol writer.
- Linux fixtures and all standard workspace quality commands pass.