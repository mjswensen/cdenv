---
id: 45
created: 2026-08-20
depends-on:
  - 43
  - 44
---

# Add PTYs, signals, cleanup, and OpenSSH multiplexing

_Converted from [`implementation-chunks/45-pty-signals-and-multiplexing.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §15.7, §15.9, and Chunk 14](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-14-pty-multiplexing-signals-and-cleanup)

## Goal

Support editor-independent interactive OpenSSH behavior while confining all necessary unsafe Linux PTY code.

## Work

- Isolate PTY/session OS operations in one private Linux module; prefer safe `nix`/standard APIs and expose only safe wrappers with documented invariants.
- Allocate a controlling PTY, establish session/process group, apply supported terminal modes/dimensions, expose actual `SSH_TTY`, and start the resolved login shell without assuming Bash.
- Handle `window-change` via `TIOCSWINSZ` and map at least INT, TERM, HUP, QUIT, KILL, USR1, and USR2 to the child process group.
- On disconnect send HUP, then bounded TERM/KILL escalation. Allow a process intentionally daemonized into a new session/group to survive.
- Support multiple concurrent exec/PTY channels, no-session ControlMaster connections, keepalive/global requests, clean reconnect, and one attach hook per transport rather than channel.
- Return typed unsupported behavior on non-Linux builds.

## Rust guidance

Load the `rust-best-practices` skill, especially unsafe, comments/documentation, pointer, and async testing guidance. Every unsafe block needs a narrow `// SAFETY:` justification; enable `unsafe_op_in_unsafe_fn`; never use unsafe for optimization.

## Acceptance criteria

- Linux agent tests cover shell, terminal modes, resize, Ctrl+C/signals, process-group delivery, disconnect escalation, and detached helper survival.
- OpenSSH ControlMaster tests run concurrent exec and PTY channels, keep an idle master, and reconnect cleanly.
- A repository check confirms unsafe appears only in the private Linux PTY module and each block has a safety justification.
- Debian and Alpine tests pass; macOS compilation returns typed unsupported PTY behavior.
- Standard format/Clippy/test/doc checks pass with warnings denied.