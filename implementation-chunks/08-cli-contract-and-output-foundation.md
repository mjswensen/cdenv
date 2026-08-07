# 08 — Define the CLI contract and output foundation

**Parent phase:** [Implementation plan §10 and §16.2–16.3](../implementation-plan.md#10-cli-contract)  
**Depends on:** [05](05-rust-workspace-and-quality-baseline.md), [06](06-core-identities-and-workspace-names.md), and [07](07-core-status-and-errors.md)

## Goal

Make the complete V1 command-line grammar and machine-output boundary compile before command behavior is added.

## Work

- Implement parsing for global root/SSH-consent flags and every V1 command: `create`, `list`, `up`, `down`, `rebuild`, `status`, `ssh`, `forward`, `lock`, `proxy`, and `doctor`.
- Include exact positional/options behavior from §10, including repo-relative `--config`, `rebuild --no-cache`, one-or-more forward mappings, `--bind`, JSON flags, and `ssh -- <remote argv...>` preservation.
- Keep `main.rs` limited to parsing, runtime setup, library invocation, rendering, and exit status.
- Add versioned success/error envelope types and a renderer that guarantees one JSON document on stdout under `--json`; incidental diagnostics belong on stderr.
- Establish typed application errors and exit-code mapping without implementing adapters or command workflows.
- Ensure help does not advertise non-goals or editor-specific behavior.

## Rust guidance

Load the `rust-best-practices` skill. Use `thiserror` in the library, reserve `anyhow` for the binary boundary if needed, document intentionally public APIs, and keep snapshots small and reviewed.

## Acceptance criteria

- Parser tests cover every documented invocation and reject zero ports, malformed mappings, invalid names, conflicting global flags, and unknown commands/options.
- `ssh` tests prove remote arguments remain distinct arguments after `--`.
- Small snapshots cover top-level and per-command help plus JSON success/error envelopes.
- Help lists every V1 command and no non-goal command.
- The binary and libraries compile cleanly and all standard workspace quality commands pass.