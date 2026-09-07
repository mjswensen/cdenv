---
id: 71
created: 2026-09-07
depends-on: []
---

# Build the versioned workspace credential broker and private Exec bridge

**Split from:** 68, primarily sections 2, 6, 7, and 8. Issue 68 remains the normative product/security contract.

## Goal

Implement the shared, authenticated transport and ownership substrate for workspace credential capabilities: one host workspace supervisor, a long-lived attached Docker Exec to the verified primary agent, and private container-only endpoints independent of SSH sessions or declared ports.

## Existing foundation

Reuse `cdenv-core` credential identifiers/parser types, private staged/bound permission records in `crates/cdenv-cli/src/credentials.rs`, and the supervisor ownership/control foundation in `forwarding.rs`. The permission/parser foundation is currently working-tree implementation, not a live broker; see ADR 0002. This issue must not recreate permissions or treat merely claimed identity fields as authorization.

## Work

- Generalize workspace service ownership while keeping credential capability/backend state separate from TCP-forwarding state. A verified credential service lease must be possible with zero listeners and zero SSH clients.
- Define versioned typed protocol/health/lease identities bound to installation/root, workspace receipt, exact container ID, generation, selected user, host/agent build, protocol, and grant revision. Verify the actual host-selected Exec target, uniqueness, and agent compatibility; never adopt a target from container claims.
- Implement the host and static-agent ends of bounded multiplexed stdin/stdout transport. Only credential lookup, approved agent streams, identity metadata, and necessary control/health messages are permitted. No arbitrary host command, executable, config, environment, working-directory, plugin, or socket-path operation may be exposed.
- Provision controlled container-only runtime directories outside the checkout (0700) and owner-only endpoints for the effective remote user. Reject unsafe ownership, symlinks, wrong filesystem object types, and stale/unverified replacements. Paths remain stable across reconnects within a usable workspace/user generation.
- Introduce generation lease start/readiness/stop and operation-owned candidate lease APIs. The surrounding orchestrators, not the peer, will supply candidate authority in issue 77.
- Select and publish numeric bounds for frames/messages, active streams, queues/total queued bytes, helper admission, operation/handshake/idle timeouts, and retries/backoff. Use structured concurrency and backpressure; cancellation/EOF must release owned tasks and endpoints without unbounded work.
- Retry only a transient connection to the same reverified target. Reject unknown protocols/capabilities, wrong builds/users/identities, replacement/ambiguity, and old-generation replies. Keep stdout protocol-only and all diagnostics bounded and payload-free.
- Version agent/control state explicitly and provide mutating compatibility hooks. Read-only/SSH operations must never migrate, start, or repair the broker.

## Acceptance criteria

- [ ] A component fixture establishes, authenticates, uses, disconnects/reconnects, and stops the private bidirectional bridge without any TCP credential listener, published port, host socket mount, or SSH session.
- [ ] Stable endpoints survive authorized reconnects; replacement generations and unverified stale sockets cannot inherit access.
- [ ] Boundary/adversarial tests cover malformed/truncated/oversized frames, stream limits, queued-byte limits, saturation/backpressure, cancellation, timeouts, retry exhaustion, and every scoped identity/build/protocol/revision mismatch.
- [ ] Lease cancellation prevents reuse/replay, cleans only verified owned resources, and does not terminate unrelated port forwards or sessions. The ownership API supports separately verified active and candidate generations without granting candidates automatically.
- [ ] Stub backend tests exercise only narrow typed handlers; unavailable backends do not destroy transport/listeners. Real HTTPS/SSH/identity backends are supplied by issues 72–75, not a general execution escape hatch.
- [ ] Secret markers in raw requests, responses, and binary streams do not appear in logs, debug output, state, snapshots, arguments/environment, or diagnostic stdout/stderr.
- [ ] Protocol/bound documentation, migration tests, and `cargo xtask check` pass. Permission remains opt-in, and this substrate alone does not claim that issue 68's public lifecycle workflows work.

## Boundaries and implementation guidance

Issue 76 owns live credential command reconciliation/revocation and complete health rendering; issue 77 wires lifecycle ownership. Load the Rust best-practices skill. Keep shared wire/domain logic pure and host Docker/helper I/O out of `cdenv-devcontainer`.
