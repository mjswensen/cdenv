# 23 — Implement Bollard discovery, verification, and control

**Parent phase:** [Implementation plan §12 and Chunk 5](../implementation-plan.md#12-docker-cli-compose-and-bollard-adapters)  
**Depends on:** [06](06-core-identities-and-workspace-names.md), [21](21-docker-endpoint-and-process-runner.md), and the exact Bollard decision in [04](04-feasibility-adr-and-gate.md)

## Goal

Provide cdenv-owned Docker operations through one exact-pinned, typed Bollard adapter.

## Work

- Connect Bollard to the resolved Unix socket and implement ping with bounded API timeout.
- Implement one-call `all=true` container listing with installation/workspace/generation label filters, plus in-memory correlation inputs for callers.
- Add typed container/image inspect, label/name/project/service/generation verification, running-state checks, image identity and architecture detection.
- Add start, stop, rename, archive upload, and narrowly scoped operation-owned container/image cleanup.
- Map only amd64/x86_64 and arm64/aarch64 architectures; fail clearly on unsupported values.
- Preserve duplicate/stale matches for status reporting. Never select an arbitrary container or silently adopt an externally replaced one.
- Keep attached/detached Exec out of this chunk.

## Rust guidance

Load the `rust-best-practices` skill. Keep adapter errors precise and `Send + Sync + 'static`, use static fakes at orchestration seams, avoid global clients/trait objects, and borrow filters/IDs where ownership is unnecessary.

## Acceptance criteria

- Mock/API tests cover ping, exact list filters, inspect mapping, every verification mismatch, duplicate matches, architecture aliases, and control failures/timeouts.
- A many-workspace test demonstrates one list request rather than one request per workspace.
- Cleanup tests prove only operation-owned, correctly labeled resources can be removed.
- Optional local-Docker tests inspect/start/stop/rename/upload a fixture container through the resolved socket.
- Standard workspace quality commands pass.