# ADR 0002: Explicit workspace-scoped host capabilities

- Date: 2026-09-07
- Issue: 68
- Status: architecture accepted; **permission and parser foundation only** implemented
- Related: [operations](../operations.md#host-credential-permissions-issue-68),
  [V1 profile boundary](../cdenv-devcontainer-v1-support.md#host-credential-capabilities)

## Context and implementation prerequisite

Git authentication working during a host clone does not make it portable to the
container. Copying credential stores, SSH configuration, private keys, keychains,
or host sockets is not an acceptable shortcut. Host author identity is a distinct,
non-authentication capability. Repository configuration cannot supply consent.

The current production CLI dispatch in `crates/cdenv-cli/src/lib.rs` wires
`create` only through checkout creation. `up`, `down`, and `rebuild` still return
`CommandUnavailable`. The provisioning, environment, lifecycle, rebuild, and
forwarding coordinators exist as library/test seams but have no production
command composition. In particular, no production caller currently invokes
`capture_for_readiness` or `start_detached_supervisor`.

Issue 68 therefore cannot yet provide its first-hook, reconnect, down/up, and
rebuild workflows. This change does **not** resolve issue 68 or advertise a
working credential broker. It supplies independently usable permission management
and bounded credential types, without creating an alternative lifecycle path.

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

A grant for an active generation is saved but the mutation returns nonzero with
explicit integration-unavailable guidance. No helper, agent endpoint, identity
lookup, lifecycle hook, or credential transport is started. Disable/deny persist
revocation first, then prove the supervisor absent/stopped using the ownership
foundation. An occupied lifetime lock or unknown control state returns
revocation-unconfirmed; it does not stop unrelated listeners or select a PID.
This is **not** the live reconciliation/acknowledgement protocol still required
by the issue.

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

Git field-surface version 1 accepts UTF-8 `protocol=https`, `host`, optional
`path`, and optional `username`. It accepts LF-delimited fields with a blank
terminator or EOF immediately after LF. Duplicate fields, trailing records,
NUL/CR/control characters, incomplete lines, conflicting context, `url`,
caller-supplied secrets, config instructions, refresh tokens, and unknown fields
are rejected without including their values in errors. The supported response
is `username`/`password`, optional matching echoed context, and optional decimal
`password_expiry_utc`. Missing or expired credentials return no result. A broker
must recheck expiry immediately before release, not just at parsing time.

The helper-operation type distinguishes `get`, `store`, and `erase`; it is not
an installed helper. No host `store`/`erase` adapter exists. Multiplexing frames,
stream/task/queue limits, deadlines, cancellation, and retry/backoff limits are
**not implemented yet**, and no transport compatibility is claimed from these
parser bounds.

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
  a generic credential section. This integration remains unimplemented.
- Its [automatic SSH key loading](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/ssh/ssh_add.go#L18-L75)
  is deliberately not copied. Agent selection requires host consent and must
  never scan keys or run `ssh-add` automatically.
- Its [default-on options](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/config/context.go#L27-L69)
  and [raw request logging](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/credentials/server.go#L113-L123)
  are deliberate differences: cdenv is opt-in and excludes raw payloads from
  diagnostics, state, snapshots, arguments, and environment.

## Remaining implementation and release evidence

Before issue 68 can close, compose the production lifecycle paths, install the
host adapter and supervisor/Exec broker, integrate the static helper and sockets,
prove helper-chain ownership including Git approve/store, implement live
revocation, and cover early/detached lifecycle and generation handoff/rollback.
SSH socket validation/refresh, noninteractive supported host helpers, and
missing-field-only author defaults also remain outstanding.

The permission/parser tests use fake Git and private filesystem fixtures; they
require no Docker, public network, keychain, or real secrets. They do not prove
HTTPS fetch/push, SSH signing, Git helper compatibility, or transported
revocation. No minimum/tested credential-helper matrix is claimed yet. Linux
x86_64/arm64 credential transport gates, authenticated HTTPS/SSH integration
fixtures, and Apple-silicon Docker Desktop/keychain/agent smoke are still required.

Local validation on 2026-09-07 (Linux aarch64, Rust 1.97.1):

| Gate | Result | Scope |
|---|---|---|
| `cargo xtask check` | Passed | Formatting, strict Clippy, workspace tests/docs, cargo-deny; duplicate-dependency warnings remain advisory. |
| `cargo xtask test-integration --suite openssh` | Passed, 4 discovered/executed | Existing OpenSSH transport interoperability, not credential forwarding. |
| `cargo xtask test-integration --suite devcontainer-v1` | Passed, 8 discovered/executed | Existing profile/Docker/Compose preservation coverage, not the issue-68 broker. |
| Linux x86_64 credential workflows | Not run / not implemented | Required before issue closure. |
| macOS Apple-silicon credential/keychain smoke | Not run / not implemented | Linux-container access to Docker Desktop is not a macOS host-helper test. |

These host capabilities are outside repository profile semantics. V1 parsing,
`customizations`, `secrets`, and `remoteEnv` behavior are unchanged. The existing
agent and supervisor protocols are not bumped merely for unused new code; the
future live bridge requires explicit protocol/compatibility changes on mutating
paths, not migration or repair during read-only/SSH operations.
