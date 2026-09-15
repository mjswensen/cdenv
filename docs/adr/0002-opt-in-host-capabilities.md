# ADR 0002: Explicit workspace-scoped host capabilities

- Date: 2026-09-07
- Issue: 68
- Status: accepted; production credential workflows implemented, release evidence pending
- Related: [operations](../operations.md#host-credential-permissions-issue-68),
  [V1 profile boundary](../cdenv-devcontainer-v1-support.md#host-credential-capabilities)

## Context and implementation prerequisite

Git authentication working during a host clone does not make it portable to the
container. Copying credential stores, SSH configuration, private keys, keychains,
or host sockets is not an acceptable shortcut. Host author identity is a distinct,
non-authentication capability. Repository configuration cannot supply consent.

At the time this ADR was accepted, production CLI dispatch wired `create` only
through checkout creation; `up`, `down`, and `rebuild` still returned
`CommandUnavailable`, and the lifecycle coordinators had no production caller.
That prerequisite and issues 73–77 have since landed. Production now composes the
credential lease with create/up/down/rebuild, managed lifecycle work, and managed
SSH children, including readiness before the first hook and live policy
reconciliation.

This implementation does not by itself resolve issue 68. Issue 78 retains the
required authenticated HTTPS/SSH, lifecycle, secret-surface, Linux architecture,
and Apple-silicon release evidence. Documentation distinguishes those blockers
from component tests and configured-but-unobserved CI jobs.

## Decision

Use explicit, installation/workspace-scoped capabilities named `git-https`,
`ssh-agent`, and `git-identity`. Each is disabled unless explicitly enabled.
Unknown fields, capabilities, and policy schemas fail closed. Enabling an existing
capability cannot authorize a different future capability.

HTTPS permission is an exact normalized HTTPS scheme/host/effective-port origin,
not a repository path. The first bound enable may derive only the original,
credential-sanitized host workspace source. Staged grants need explicit origins;
new remotes, submodules, redirects, or repository edits cannot widen them. DNS
names use URL/IDNA canonicalization, case folding, and removal of one root dot;
IPv6 is bracketed and canonical; port 443 is explicit. Ambiguous IPv4 shorthand,
userinfo, URL repairs, wildcards, and non-origin URL components are rejected.
Private and single-label servers are permitted.

Permissions live in a separate, versioned private host store. A staged grant
binds only after a successful explicitly named clone, before future container
lifecycle work. Installation ID and root directory identity prevent cross-root
reuse, even if installation metadata is copied. A random private binding receipt
also checks workspace directory identity, creation timestamp, and sanitized-source
digest. A replacement workspace does not inherit a former workspace's grants.
Copying/restoring a root or replacing its directories may require new explicit
grants; it must not silently restore authority.

The runtime design remains a single authenticated workspace supervisor owning
both port forwarding and separately managed credential leases. A verified primary
container receives a long-lived attached Docker Exec to the static agent, carrying
a bounded versioned multiplexed protocol. Private container-only sockets are
stable within the selected user/generation. There is no host TCP credential
server, port publication, socket mount, remote-command endpoint, or interactive
SSH-session lifetime dependency.

Only typed HTTPS lookups, approved SSH-agent streams, identity metadata, and
health/control operations will cross that bridge. Authority must be checked at
dispatch and before releasing each lookup, using a grant revision. Revocation
must cancel affected operations and be acknowledged before reporting success;
an unverified PID must never be signalled. Backend absence must not destroy
unrelated forwarding or fail otherwise successful readiness.

## What this change implements

- The complete permission-command grammar, with noninteractive independent
  enable, exact-origin allow/deny, selective/all disable, and human/JSON status.
- Idempotent private permission persistence, monotonic non-wrapping revisions,
  staged/bound receipts, explicit-name binding after clone, and safe retry state.
- No grant-derived checkout or Git configuration changes. With capabilities off,
  create writes no credential store or binding receipt.
- Strict ownership, mode, symlink, hard-link, record-size, and schema checks;
  value-free errors, including invalid CLI origins containing userinfo.
- Consistent read-only permission facts in list, detailed status, and doctor.
  Transport is inactive and backends uninspected, never falsely healthy.
- Private, zeroizing Git credential request/result types with redacted `Debug`,
  bounded parsing, actual path/username preservation, context checks, and expiry
  validation. They deliberately have no general-purpose serialization API.
- A lookup-only host Git adapter that executes current trusted host helpers from
  a neutral directory with fixed arguments, a closed launch-environment surface,
  explicit noninteractive settings, private bounded pipes, finite admission and
  deadlines, process-group cancellation/reaping, typed availability, and no
  cdenv credential cache or ordinary subprocess log.
- Independently optional, bounded Git identity metadata read only from that
  neutral trusted context. The authenticated identity operation carries its
  closed schema to a cdenv-owned container file. A data-only Git wrapper probes
  the invocation's original effective configuration and fills only absent name
  or email fields, preserving normal config, environment, and signing behavior.
- A version-1 binary multiplexing protocol whose closed operation set contains
  only credential lookup, approved agent streams, identity metadata, health,
  cancellation, and stop. Frames and secret-bearing values have redacted debug
  forms and value-free errors.
- Exact installation/workspace receipt/container/generation/user/build/protocol/
  revision lease identity, plus separately operation-owned active and candidate
  generation APIs. Candidate readiness never grants active authority implicitly.
- A verified selected-user Docker Exec owned by the workspace supervisor even
  with zero listeners, and static-agent mode-0700 runtime directories with stable
  owner-only endpoints. Exact same-generation reconnect may replace verified
  owned sockets; stale/symlink/wrong-owner/wrong-kind replacements fail closed.
- Structured bounded queues, finite stream/helper admission, operation/handshake/
  idle deadlines, cancellation cleanup, and a finite same-target retry schedule.

An enrolled active generation accepts policy changes only through its exact
authenticated supervisor control identity and acknowledges the monotonic durable
revision. Reconciliation changes independent broker authority without rerunning
a lifecycle hook or restarting listeners or the container. Revocation suppresses
queued and in-flight affected results and closes affected agent streams. An
active generation that predates enrollment returns explicit `cdenv up` guidance.
Disable/deny persist revocation first; an occupied lifetime lock, unknown control
state, identity mismatch, or acknowledgement timeout returns
revocation-unconfirmed and never selects or signals a PID. A retry cannot forget
the durable revocation.

## Current bounded surface

| Boundary | Limit |
|---|---:|
| Exact HTTPS grants per workspace | 64 |
| Explicit origin before parsing | 2048 bytes |
| Private permission/receipt input | 256 KiB |
| Git credential message, including separators | 32 KiB |
| Git field, including name and `=` | 8 KiB |
| Git fields per message | 8 |
| Grant revision | positive `u64`, no wrapping |
| Broker frame payload | 64 KiB |
| Active broker streams | 32 |
| Queued frames / aggregate payload | 64 / 512 KiB |
| Concurrent host helper operations | 4 |
| Host helper request / stdout / discarded stderr | 32 KiB / 32 KiB / 8 KiB |
| Host helper execution / termination grace | 30 s / 2 s |
| Handshake / operation / idle timeout | 5 s / 30 s / 60 s |
| Same-target reconnect attempts | 3; 100 ms exponential, capped at 2 s |

Git field-surface version 1 accepts UTF-8 `protocol=https`, `host`, optional
`path`, and optional `username`. It accepts LF-delimited fields with a blank
terminator or EOF immediately after LF. Duplicate fields, trailing records,
NUL/CR/control characters, incomplete lines, conflicting context, `url`,
caller-supplied secrets, config instructions, refresh tokens, and unknown fields
are rejected without including their values in errors. The supported response
is `username`/`password`, optional matching echoed context, and optional decimal
`password_expiry_utc`. Missing or expired credentials return no result. A broker
must recheck expiry immediately before release, not just at parsing time.

The installed static helper distinguishes `get`, `store`, and `erase`. It
validates a bounded `get` before using the private bridge; `store` and `erase`
return successfully without reading or forwarding input. Host delegation
implements only `credential fill`; no host approve/store/reject/erase adapter
exists. Each request executes
the current helper chain, so cdenv does not cache rotation. Broker protocol 1
rejects unknown frame kinds, versions, identities, and old generations before
backend dispatch. The production workspace supervisor dispatches the trusted Git
and selected-agent adapters; backend degradation does not destroy the transport
or unrelated listeners.

## DevPod comparison

The source reference is DevPod commit
[`5a0efcbff6610ab114b421f68a890739a452e66b`](https://github.com/loft-sh/devpod/tree/5a0efcbff6610ab114b421f68a890739a452e66b),
not a runtime interoperability test or a claim about DevPod Pro:

- Its [lookup-only helper](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/cmd/agent/git_credentials.go#L47-L78)
  and [host Git delegation](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L203-L230)
  motivate on-demand lookup rather than clone interception or token caching.
- Its [service transport](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/tunnel/services.go#L35-L150)
  supports shared ownership. cdenv instead chooses verified private Docker Exec
  and container Unix sockets, not another HTTP/SSH/gRPC service stack.
- Its [path handling](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L233-L265)
  motivates preserving the actual repository/account context, never replacing
  it with the initial workspace path.
- Its [helper configuration](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L33-L93)
  motivates reversible, cdenv-owned integration rather than editing/removing
  a generic credential section. cdenv implements that as an owned fragment and
  appended process configuration, scoped to exact granted origins.
- Its [automatic SSH key loading](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/ssh/ssh_add.go#L18-L75)
  is deliberately not copied. Agent selection requires host consent and must
  never scan keys or run `ssh-add` automatically.
- Its [default-on options](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/config/context.go#L27-L69)
  and [raw request logging](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/credentials/server.go#L113-L123)
  are deliberate differences: cdenv is opt-in and excludes raw payloads from
  diagnostics, state, snapshots, arguments, and environment.

## Remaining implementation and release evidence

Production create/up now establishes bridge readiness before foreground hooks,
enrolls foreground and detached lifecycle work and SSH children, keeps a
zero-listener workspace owner alive, and scopes restart/down/rebuild behavior to
the verified generation. Issue 78 retains the authenticated cross-platform public
workflow and release evidence required before issue 68 can close.

The permission/parser and host-adapter tests use fake Git/helpers, private
filesystem fixtures, and local system Git; they require no Docker, public
network, keychain, or real secrets. They verify standard helper input, trusted
includes/per-URL matching, declared noninteractive variables, rotation, bounds,
and cleanup. The packaged credential suite additionally exercises a controlled
real host `ssh-agent` through create, first hook, postAttach, managed SSH,
down/up, rebuild, and live disable. These results do not prove authenticated
smart-HTTP fetch/push, real SSH signing and host-key workflows, provider-specific
helper compatibility, or every lifecycle and secret surface. No live-provider
helper matrix is claimed.

Evidence status on 2026-09-15 (Linux aarch64, Rust 1.97.1):

| Gate | Result | Scope |
|---|---|---|
| `cargo xtask check` | Passed | Formatting, strict Clippy, workspace tests/docs, cargo-deny; duplicate-dependency warnings remain advisory. |
| `cargo xtask test-integration --suite openssh` | Previously passed, 4 discovered/executed | Existing OpenSSH transport interoperability, not credential forwarding. |
| `cargo xtask test-integration --suite devcontainer-v1` | Previously passed, 8 discovered/executed | Existing profile/Docker/Compose preservation coverage, not the issue-68 broker. |
| Packaged credential workflow | Implemented and strict CI matrix configured; no retained clean native run yet | A nested-container attempt discovered two tests but could not use container-private bind paths with its Docker Desktop daemon; this is a failed prerequisite, not a skip or release result. |
| Linux x86_64 and arm64 credential workflows | Required observation pending | Both clean native architecture jobs must pass the exact discovered count. |
| macOS Apple-silicon credential/keychain smoke | Required observation pending | Linux-container access to Docker Desktop is not a macOS host-helper test. |

These host capabilities are outside repository profile semantics. V1 parsing,
`customizations`, `secrets`, and `remoteEnv` behavior are unchanged. Credential
broker/control compatibility is explicit and versioned; read-only commands do not
migrate, repair, or silently replace incompatible live state.
