---
id: 24
created: 2026-08-20
depends-on:
  - 23
---

# Implement binary-clean Bollard Exec streaming

_Converted from [`implementation-chunks/24-bollard-exec-streaming.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §10.11, §12, and Chunk 5](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#1011-proxy)

## Goal

Provide attached and detached Docker Exec primitives with exact stream separation and cancellation behavior.

## Work

- Implement typed Exec create/start/inspect for attached and detached processes, including user, working directory, environment, stdin attachment, and command argv.
- Decode Docker multiplexed frames entirely inside the adapter: stdin to Exec stdin, stdout bytes exactly to caller stdout, and stderr bytes exactly to a separate sink.
- Never allocate a Docker TTY; SSH channels own PTYs later.
- Implement bounded backpressure, half-close/EOF propagation, cancellation, final exit-code inspection, and bounded control/API timeouts.
- Ensure no Docker framing, tracing, or diagnostic bytes can reach a caller’s stdout.
- Expose generic async reader/writer entry points usable with in-memory duplex streams and future proxy/agent orchestration.

## Rust guidance

Load the `rust-best-practices` skill, especially async error and pointer/thread-safety guidance. Prefer concrete generics/static dispatch, bounded channels, explicit cancellation flow, and no unnecessary `Arc<dyn Trait>`.

## Acceptance criteria

- Deterministic frame tests cover fragmented headers/payloads, interleaved stdout/stderr, binary/NUL data, large streams, malformed frames, and final partial data.
- Duplex tests prove bidirectional flow, backpressure, local/remote EOF, cancellation, and final nonzero exit status.
- A byte-for-byte assertion proves protocol stdout contains no frame or log bytes.
- Detached Exec create/start/inspect is covered separately from attached streaming.
- Optional Docker round-trip tests and all standard workspace quality commands pass.