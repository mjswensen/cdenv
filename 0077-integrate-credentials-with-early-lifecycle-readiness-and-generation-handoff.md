---
id: 77
created: 2026-09-07
depends-on:
  - 69
  - 70
  - 76
---

# Integrate credentials with early lifecycle readiness and generation handoff

**Split from:** 68, primarily section 6 plus the managed-process and environment requirements in sections 1–5.

## Goal

Connect the completed broker/capability services to production lifecycle and SSH execution so explicitly granted credentials are available before the first container hook, survive detached work, and remain correctly scoped through stop/restart/rebuild/recovery.

## Work

- Use issues 69/70's production command composition and issue 76's reconciliation/control APIs. After host initialization, provision/verify the agent, establish the authorized credential bridge, and enroll the initial effective environment before the first container lifecycle stage, not after `waitFor` or at the SSH handshake.
- Preserve explicit-name staged binding and interrupted-binding recovery. A staged grant plus explicit `create --name` must support a first-attempt private dependency fetch in `onCreateCommand` without a prior failed create or SSH connection. Ordinary host cloning and host `initializeCommand` continue using the host directly.
- Enroll foreground and detached lifecycle commands, PTY/non-PTY cdenv SSH children, pre-SSH `postAttachCommand`, and their descendants. Inject only the verified managed `SSH_AUTH_SOCK` after generic sanitization; do not weaken the blanket arbitrary `SSH_` snapshot filter or persist session-specific sockets.
- Keep a workspace owner alive with zero ports and zero SSH clients after `up` returns. Closing one shell/session must not revoke other sessions or detached jobs. Stable endpoints reconnect only to the same verified target within the usable generation.
- Restore services only through explicit `up` after supervisor loss/host reboot or supported compatibility changes. Ordinary SSH/status/doctor diagnose rather than repair; reconnects must reject wrong builds/users/protocols, ambiguous containers, or externally replaced generations.
- Authorize rebuild candidates only from host-verified, operation-owned resources. Maintain separate active/candidate leases where needed so candidate hooks can authenticate before readiness. Successful handoff revokes the old generation; rollback revokes only the failed candidate and retains valid old services. Never replay old-generation responses into a new connection.
- Apply explicit Compose partial-replacement/recovery semantics. Do not adopt a primary based on its claims or automatically extend integration to sidecars, arbitrary `docker exec`, other users, or unrelated entrypoints.
- Wire `down` to stop verified credential streams/bridge/eligible workspace owner resources while retaining grants. Preserve checkout, volumes, unrelated services, and existing lifecycle failure/indeterminate retry rules.
- Enforce the parent failure table: unavailable helpers/agents are backend degradation only; unsafe installation/configuration or failed required transport readiness makes a mutating command nonzero without tearing down an otherwise running container. Hooks that need missing credentials fail through ordinary checkpoint rules; missing identity is advisory.

## Acceptance criteria

- [ ] Production-command tests perform the staged explicit-create workflow and authenticate the first foreground hook before any SSH connection. Ordinary host clone behavior is unchanged.
- [ ] Foreground/detached hooks, postAttach, PTY/non-PTY sessions, and descendants receive only the enabled integrations. Capability-off environment/configuration behavior remains unchanged.
- [ ] Zero-port/zero-client workspaces, detached jobs, multiple SSH clients, and one-client disconnect demonstrate workspace rather than session ownership.
- [ ] Transport loss/reconnect, supervisor crash, explicit-up recovery, down/up, and compatibility repair have exact identity/lease tests; ordinary SSH/status/doctor do not repair or grant.
- [ ] Successful rebuild, rollback, cancelled/failed candidates, and Compose partial replacement demonstrate correct active/candidate authorization, old-lease revocation, stable endpoints, and no cross-generation replies.
- [ ] Backend-unavailable versus installation/readiness-failed cases produce the required distinct exit behavior while preserving running containers, checkout/named-volume data, unrelated ports, and one-time checkpoint semantics.
- [ ] New live command paths replace the foundation's blanket integration-unavailable behavior only when readiness is actually established. Numeric bounds, cancellation, revision checks, and read-only reporting remain enforced.
- [ ] `cargo xtask check` and applicable lifecycle, OpenSSH/profile, and preservation integration tests pass. Complete authenticated/platform release evidence is still required by issue 78.

## Implementation guidance

Load the Rust best-practices skill. Coordinate existing agent-environment, lifecycle, proxy, supervisor, image rebuild, and Compose recovery modules through narrow typed seams; do not create a parallel credential-specific lifecycle implementation or silently change V1 semantics.
