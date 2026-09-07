---
id: 74
created: 2026-09-07
depends-on:
  - 71
---

# Relay explicitly selected host SSH agents through the credential bridge

**Split from:** 68, primarily section 4 and the SSH portions of sections 2, 6, and 7.

## Goal

Implement private container SSH-agent access to an explicitly selected host agent over the workspace Exec bridge, without copying keys, mounting host sockets, importing SSH configuration, or binding lifetime to an SSH session.

## Work

- Implement the host and static-agent stream handlers for a private selected-user container agent socket supplied by issue 71. Relay the standard binary agent protocol with independent concurrent connections, correct EOF, backpressure, cancellation, and bounded operation timeouts.
- Preserve supported agent protocol extensions rather than silently stripping them; declare compatibility/size limits and enforce stream/message/queued-byte admission limits.
- Resolve `auto` only from `SSH_AUTH_SOCK` in an explicit host mutating invocation. Persist the selector, not inherited environment or keys. Refresh changed automatic selection only via explicit reconciliation; an explicit selector must never fall back to another agent.
- Check/connect the selected host endpoint using appropriate Unix type/ownership safety rules. Diagnose absent, unreachable, empty, and host-confirmation-required agents without scanning the filesystem, reading private keys, running `ssh-add`, or starting a replacement agent.
- Reconnect when the selected agent is recreated at the same path. Do not authorize another path from container requests or opportunistic filesystem discovery.
- Supply a verified managed `SSH_AUTH_SOCK` enrollment value for issue 77 to inject after generic environment sanitization. Keep arbitrary/stale `SSH_` values excluded from reusable snapshots, including when the capability is disabled.
- Expose per-capability health and stream cancellation to issue 76. Hardware-key/agent confirmation may occur for an authorized signing request within a bounded timeout; it is not permission to start HTTPS login.

## Acceptance criteria

- [ ] Fake host agents exercise multiple simultaneous clients, binary/extension framing, partial I/O, EOF, slow readers/writers, oversized messages, concurrency limits, timeouts, and cancellation without unbounded tasks or buffers.
- [ ] Socket safety, explicit versus automatic selection, omitted-selector idempotence, same-path restart, and explicit automatic refresh are covered. Wrong-owner/type/symlink endpoints and container-chosen host paths are rejected safely.
- [ ] Absent/unreachable/empty/confirmation-required conditions are value-free backend health facts, not transport teardown or fatal readiness by themselves.
- [ ] Cancellation/revocation closes affected active streams and blocks further requests without stopping unrelated workspace services. Live command acknowledgement is exercised by issue 76.
- [ ] Stable container endpoints work independently of individual SSH clients. There are no host socket/credential-file mounts, copied private keys, automatic key loading, or `.ssh/config` imports.
- [ ] Existing environment sanitization is unchanged; the enrollment seam provides only the verified managed socket after sanitization. Full lifecycle/SSH injection is owned by issue 77.
- [ ] Documentation explains broad signing authority, host confirmation, missing-agent/key remediation, socket refresh, and non-portability of aliases, IdentityFile/IdentityAgent, ProxyJump, and known_hosts. It never recommends disabling host-key verification.
- [ ] Fake-based workspace tests and `cargo xtask check` pass. Real OpenSSH/agent fixtures and macOS host-agent smoke remain issue 78's release requirement.

## Implementation guidance

Load the Rust best-practices skill. Keep SSH-agent capability independent from HTTPS/identity and any future signing capability; use typed streams and cancellation, not a generic socket-proxy or host command API.
