# 32 — Implement tool-free agent identity and provisioning

**Parent phase:** [Implementation plan §13.3–13.5 and Chunk 8](../implementation-plan.md#134-tool-free-provisioning-flow)  
**Depends on:** [24](24-bollard-exec-streaming.md) and [31](31-agent-build-identity-and-embedding.md)

## Goal

Install and verify the correct agent in a running Linux container without relying on container utilities.

## Work

- Implement agent `identity` output for effective UID, GID, home, and shell, executed as the selected Dev Container user.
- Implement root-only `provision` using Rust/Linux filesystem/syscall APIs: validate a staged manifest, create directories, atomically install agent/assets, set ownership/modes, and clean operation-owned staging.
- Upload a tar archive containing a uniquely named staging agent and provision assets through Bollard, then invoke staging identity/provision and final `version` with typed machine output.
- Try the documented ordered executable locations; detect unwritable/read-only/noexec cases and record the actual secure path. Never weaken private modes or write into checkout.
- Always upload and provision on every successful `up`/rebuild path, even when an existing agent reports the same ID.
- Verify final build ID/protocol as the remote user before callers may commit provisioned state.

Do not depend on `sh`, `cp`, `install`, `chmod`, `id`, `tar`, or distro-specific tools inside the container.

## Rust guidance

Load the `rust-best-practices` skill. Isolate Linux OS details, expose safe wrappers with documented invariants, use RAII staging cleanup, and return precise provision/path/permission errors.

## Acceptance criteria

- Debian, Alpine, and minimal tool-free fixture tests provision successfully for root and a custom remote user.
- Tests prove every `up`-style call uploads again, then verifies exact build/protocol/user/path before success.
- Read-only, noexec, ownership, malformed manifest, wrong architecture/build, upload, and final-version failures are actionable and never update active state.
- Installed agent/private assets have exact ownership/modes and no provision file appears in checkout.
- macOS reports typed unsupported behavior; all standard workspace quality commands pass.