---
id: 44
created: 2026-08-20
depends-on:
  - 24
  - 34
  - 38
  - 43
---

# Implement the host proxy and per-transport attach hook

_Converted from [`implementation-chunks/44-host-proxy-and-post-attach.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §10.11, §11.6, and Chunk 13](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-13-host-proxy-and-attach-lifecycle-through-bollard)
**Legacy dependencies (untracked):** 12

## Goal

Connect system OpenSSH to the exact provisioned active container while keeping proxy stdout binary-clean.

## Work

- Normalize workspace name/host, fail quickly on lifecycle lock contention, and hold a shared lock only through state resolution, live inspect, and successful Exec attach setup.
- Require exactly the recorded running container/generation and matching installation labels, agent path/build/protocol, remote user, and workspace folder. Fail with concise `cdenv up <name>` guidance on drift/replacement/upgrade.
- Before `ssh-server`, serialize and execute `postAttachCommand` once for this new transport using the active generation’s immutable plan/environment.
- Serialize attach hooks per container. Closed/background stdin rules apply; hook output goes only to restricted logs/stderr. A definite failure rejects this transport; a later transport retries and success clears only attach-specific degradation.
- Start attached agent Exec as the recorded user/folder with provisioned assets and bridge local stdin, decoded stdout, and stderr with bounded backpressure/EOF/cancellation.
- Inspect final Exec status. Proxy never provisions, repairs the environment, or starts a missing supervisor.

## Rust guidance

Load the `rust-best-practices` skill. Keep lock/attach ownership explicit with RAII, use typed proxy diagnostics, structured cancellation, and static stream/adapter seams.

## Acceptance criteria

- System OpenSSH executes a remote command through generated ProxyCommand with no exposed port 22 or permanent SSH process.
- Byte tests prove proxy stdout is exact SSH protocol data under errors, tracing, hook output, and Docker multiplexing.
- Tests cover stopped/missing/ambiguous/replaced/mismatched/locked states and concise stderr-only failures.
- Concurrent transports serialize hooks; multiplexed channels on one transport do not rerun the hook; forwarding-only coverage is completed in 46.
- Lock-release, cancellation, EOF, and final-status tests pass with standard workspace quality commands.