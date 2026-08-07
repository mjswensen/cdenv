# 44 — Implement the host proxy and per-transport attach hook

**Parent phase:** [Implementation plan §10.11, §11.6, and Chunk 13](../implementation-plan.md#chunk-13-host-proxy-and-attach-lifecycle-through-bollard)  
**Depends on:** [12](12-workspace-locking-and-reservation.md), [24](24-bollard-exec-streaming.md), [34](34-agent-lifecycle-runner.md), [38](38-list-status-and-output.md), and [43](43-agent-ssh-auth-and-exec.md)

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
- Concurrent transports serialize hooks; multiplexed channels on one transport do not rerun the hook; forwarding-only coverage is completed in [46](46-direct-tcpip-and-ad-hoc-forward.md).
- Lock-release, cancellation, EOF, and final-status tests pass with standard workspace quality commands.