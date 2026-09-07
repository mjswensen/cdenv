---
id: 68
created: 2026-09-07
depends-on: []
---

# Forward host Git credentials with a workspace-scoped broker

## Goal

Make supported Git authentication that already works on the host available to Git in the running primary dev container, without copying the host's credential stores or private keys into it. Look up HTTPS credentials through host Git on demand and forward access to a selected host SSH agent. Also provide separately enabled inheritance of Git author name/email.

This is an implementation issue with an agreed product contract, informed by a source-level comparison with DevPod. It is not a request to execute Git operations on the host on behalf of the container, implement a general secret store, or promise that every host Git/SSH configuration is automatically portable.

## Agreed decisions

- Support HTTPS credential-helper authentication and existing SSH-agent authentication in the first release.
- Use a workspace-scoped host broker with a private container agent bridge over attached Docker Exec. Reuse the workspace supervisor's ownership/control foundation; do not tie availability to interactive SSH sessions.
- Guarantee integration for cdenv-launched lifecycle commands, SSH sessions, and their descendants. Arbitrary `docker exec`, unrelated container entrypoint processes, other container users, and Compose sidecars are not automatically integrated.
- All capabilities are off by default and require explicit host-side permission. Permission is installation/workspace scoped and persists through ordinary down/up; repository configuration cannot grant it.
- Scope HTTPS permission to an exact normalized scheme/hostname/port origin, not one repository path. Initially grant only the original repository's HTTPS origin unless the user supplies explicit origins; additional origins require explicit approval.
- For granted HTTPS origins in managed processes, the forwarding helper replaces other credential helpers. Preserve the underlying configuration and prevent other helpers from storing forwarded tokens. Ungranted origins retain their normal container configuration without gaining host access.
- An unavailable host backend degrades credential availability but does not by itself make `up` fail. Installation/configuration/transport-readiness failures do make the mutating command nonzero, without tearing down an otherwise running container. Hooks that actually need unavailable credentials fail normally.
- Introduce `cdenv credentials` for independent capability enable/disable and grant management. A successful disable revokes live access without stopping the container or unrelated services.
- Include optional, separately enabled host `user.name`/`user.email` inheritance, filling only missing values. GPG, commit-signing services, and Docker credentials are not part of this issue.

## 1. Command and permission model

### Extensible capability namespace

The initial capabilities are `git-https`, `ssh-agent`, and `git-identity`. Model them as independently granted capabilities under the `credentials` command group, not one boolean meaning "all current and future secrets".

Future capabilities such as `gpg-agent` or a narrowly scoped `git-signing` service may use the same command group, supervisor ownership, and versioned transport. They must have their own consent, policy, diagnostics, and threat model. Installing an upgrade or enabling one existing capability must never enable another. Unknown capability names/versions fail closed; do not create an arbitrary remote-command/plugin endpoint as an extensibility mechanism.

SSH-agent access is intrinsically broad: a program given a general agent socket may also use its keys for signing. Separate future signing configuration must not be presented as a cryptographic restriction on a raw agent capability already granted.

### Public surface

Provide the following command semantics, honoring the existing root selection and validated workspace names:

```text
cdenv credentials enable NAME git-https [--host HTTPS_ORIGIN ...]
cdenv credentials enable NAME ssh-agent [--socket auto|ABSOLUTE_HOST_SOCKET]
cdenv credentials enable NAME git-identity
cdenv credentials allow NAME git-https HTTPS_ORIGIN ...
cdenv credentials deny NAME git-https HTTPS_ORIGIN ...
cdenv credentials disable NAME [CAPABILITY ...]
cdenv credentials status NAME [--json]
```

- Enable exactly the named capability. No implicit bundle and no default enable-all operation. Print a concise explanation of the granted authority; the explicit command is consent and must support noninteractive use.
- Enabling an existing capability is idempotent. Explicit origins add grants; omission does not silently expand an existing allowlist. `allow`/`deny` adjust an enabled HTTPS capability and never enable a disabled capability implicitly.
- On first HTTPS enable without `--host`, derive only the original, credential-sanitized HTTPS repository origin from trusted host workspace metadata. For SSH/local sources or an indeterminate origin, require explicit HTTPS origins rather than guessing a transport or port.
- Normalize HTTPS origins before persisting/comparing: canonical hostname representation and effective port, including default 443 and IPv6 handling. Reject userinfo, repository paths other than the empty/root path, query, fragment, wildcard origins, unsupported schemes, and ambiguous/control-character input. Explicit private/intranet Git servers are valid origins; do not reuse the public-Feature source policy to forbid them.
- Additional remotes, redirects, submodules, changed `origin`, or edited `devcontainer.json` cannot widen grants. Record grants only in private host-managed state, never in the checkout or container-supplied policy.
- `disable NAME` revokes every currently granted capability for that workspace; specifying capabilities revokes only those. Disabled capabilities lose their grants rather than silently retaining enabled authority. A later enable is a new explicit grant. Ordinary `down`, in contrast, stops live access but retains permission for the next `up`.
- Changes to an already running, verified generation reconcile without rerunning lifecycle hooks or stopping the container. If provisioning/agent compatibility needs repair, return explicit `up` guidance rather than repairing during read-only/SSH operations.
- Existing processes cannot have their environment rewritten. Enabling a previously absent integration may require a new cdenv SSH session; report that clearly. Policy changes apply to subsequent requests from already integrated clients, and revocation applies even to old clients.
- Expose the same credential health facts in existing `status`, `list` summaries, and read-only `doctor`; the dedicated status command must also work for staged grants described below. JSON output is versioned and contains no credential payloads.

### Permission before the first lifecycle hook

Do not require users to suffer a failed first `create` or open an SSH session before granting credentials needed by `onCreateCommand`.

Allow `credentials enable` to stage host-owned permission for an explicit not-yet-created workspace name. For a staged HTTPS grant, require explicit origins because there is no original repository metadata to inspect. The user then invokes `create --name NAME ...`. Bind the staged grant to the actual installation/workspace record before any container lifecycle command, and report staged versus bound permission distinctly. This operation creates no checkout or container and runs no helper/login operation.

Staged permission must not act as a wildcard for automatically selected workspace names, another root, or arbitrary name reuse after workspace identity replacement. Existing permission must not transfer silently to a different workspace identity. Bind/consume staged permission transactionally and preserve clear retry state if clone or creation fails.

Example workflow:

```sh
cdenv credentials enable project git-https --host https://github.com
cdenv credentials enable project ssh-agent
cdenv credentials enable project git-identity
cdenv create --name project https://github.com/example/project.git
cdenv ssh project
cdenv credentials disable project ssh-agent
```

## 2. Architecture and ownership

```text
Container Git over HTTPS -> cdenv-agent credential helper -> private credential socket --+
                                                                                       |
Container Git over SSH   -> container SSH client -> private SSH-agent socket ------------+
                                                                                       |
                                                                     cdenv-agent bridge
                                                                                       |
                                                        attached Docker Exec / framed stdio
                                                                                       |
                                                              host workspace supervisor
                                                                                       |
                                                    +----------------------------------+-----+
                                                    |                                        |
                                          host git credential fill                    selected host agent
                                          / trusted helper chain                      / signing requests
```

- One authenticated host supervisor owns workspace services, including credential forwarding when no TCP ports are declared. Keep credential capability/backend state separate from port-forwarding state so a missing keychain does not destroy unrelated listeners.
- The host creates the long-lived attached Exec to a verified primary container and agent. Use its bidirectional stdin/stdout for a bounded, versioned multiplexed protocol; stdout is protocol-only. No container-to-host TCP server, credential port publication, `host.docker.internal`, host credential-file mount, or dependency on cross-OS Unix-socket bind mounts.
- The container bridge owns private Unix sockets in controlled container-only runtime directories outside the checkout. Directories are mode 0700 and sockets/restricted files are owner-only, owned appropriately for the effective remote user. Reject unsafe ownership, symlinks, filesystem types, and stale/unverified replacements rather than weakening permissions.
- Runtime paths are stable for a workspace/user within a usable container generation and are not specific to an SSH connection. Reconnecting the bridge must not force detached jobs to discover a new socket path. Stable naming does not relax generation validation or authorize old generations.
- Bind each transport to host-verified installation, workspace, exact container ID, generation, selected user, host/agent build, protocol, and grant revision. A peer merely claiming these values is not authorization; the host's verified Exec target and private authenticated control plane supply authority.
- Only narrow typed operations cross the bridge: credential lookup, approved agent streams, identity metadata, and necessary control/health messages. The container cannot choose a host executable, helper command, working directory, agent path, configuration path, or environment.
- Specify finite frame/message/stream counts, total queued bytes, concurrent host-helper processes, per-operation timeouts, idle/handshake limits where appropriate, cancellation, and retry/backoff limits. Publish the limits and test their boundaries. Use backpressure instead of unbounded buffering or one unbounded task/process per request.

## 3. HTTPS credential behavior

### Lookup-only host delegation

Implement a Git credential helper using the static container agent; installing another runtime or provider-specific client inside the container is not required.

1. Parse Git's helper input using a bounded credential-protocol parser.
2. Normalize and validate the actual requested HTTPS origin and credential context.
3. Authorize the request against the current workspace grant on the host.
4. Invoke host `git credential fill` with controlled arguments and request data on stdin.
5. Validate the bounded result and grant revision before returning the supported credential fields to the container Git process.

Support `get` only as a host operation. `store` and `erase` are successful no-ops that do not write, approve, reject, or delete credentials in the host store, and must not log their often-secret input. An unavailable lookup returns no credential with a focused, value-free diagnostic rather than placeholder credentials. For a granted origin, failure must not silently invoke another configured container helper; ordinary Git terminal-prompt behavior remains the caller's concern, not a host-login tunnel.

Consult host helpers on demand, not by intercepting clone or persisting its result. Do not introduce a cdenv credential cache in this release. Host helpers remain responsible for their own existing storage, refresh, and caching. Rejected/stale credentials may require host-side repair because erase is not forwarded; explain this limitation and how to retry.

### Correct credential context

- Preserve actual protocol, hostname/port, repository path, and any supplied username. Do not collapse requests to hostname-only keys or lose account information.
- Configure the managed container integration to retain the HTTP path for approved origins (`credential.useHttpPath=true` or equivalent correct behavior), then let host Git apply its trusted matching policy.
- Never substitute the original workspace URL/path for a submodule, second remote, or other repository. Two paths and two usernames on one authorized server must remain distinguishable when host configuration uses them.
- Allowlist authorization is based on the normalized origin, while path/username are lookup context. Paths are not filesystem access or permission to execute host Git in the corresponding checkout.
- Define/version the supported Git credential fields and capabilities. The initial interoperability guarantee covers username/password-style HTTPS authentication, including tokens returned as passwords. Do not promise every advanced HTTP authentication mechanism or blindly forward unrecognized fields, conflicting URL representations, caller-supplied secrets, refresh tokens, or configuration instructions into host Git. Handle supported expiry information correctly and never add token caching that outlives it.

### Trusted host context and prompting

- Resolve host Git, helper configuration, and required environment from the host's explicit launch/configuration context. Invoke Git from a controlled neutral directory with repository discovery/config injection controlled; never use `git -C <container-writable checkout> credential fill` or execute helper instructions read from that checkout.
- Support trusted host user/system helper chains, per-URL account configuration, and trusted host includes. Checkout-dependent `includeIf gitdir`/`onbranch` behavior is not automatically reproduced by a neutral context; document that boundary. Do not fake compatibility by evaluating arbitrary checkout configuration. Any later contextual profile mechanism must be explicitly host-owned.
- Never import container environment variables into host subprocesses. Do not persist the host process environment wholesale. Configured host helpers are trusted host code; cdenv is not a sandbox against a user deliberately exposing that code/configuration to a container.
- Broker lookups are noninteractive on the host by default. Set `GIT_TERMINAL_PROMPT=0`, suppress Git askpass, use supported helper-specific noninteractive settings, and bound/cancel subprocesses. That Git variable alone does not prevent an arbitrary helper from launching a GUI; declare and test supported helper behavior rather than promising universal suppression.
- Do not automatically start browser/OAuth login from a container request. Return host reauthentication guidance and retry after the user authenticates on the host. Credentials provided only by a one-time clone prompt, URL userinfo, `.netrc`, custom HTTP headers, arbitrary URL rewriting, or a nonportable helper are not all implicitly covered by this feature.

### Helper configuration ownership

For granted origins in managed processes, reset the matching helper chain and install only cdenv's lookup helper. Preserve normal helper behavior for other origins. This must work in the presence of generic helpers, URL/path-specific helpers, includes, multiple global configuration locations, and existing Git command-environment configuration.

Use cdenv-owned container configuration fragments and/or process overlays, outside the checkout. Preserve existing container configuration files and their effective unrelated semantics. Compose with existing `GIT_CONFIG_*` settings deliberately; do not simply overwrite a count or replace the global config with an incomplete copy. Never remove an entire `[credential]` section or use a blanket helper unset as cleanup.

The owned integration contains executable/socket paths and nonsecret configuration only. Test the full Git `get` then approve/store sequence: native `store`/cache helpers for granted origins must not receive a token returned by cdenv. Disabling/reconfiguring removes only cdenv-owned integration for future invocations and restores underlying behavior. Deliberate user/container overrides remain trusted code, not something this feature can prohibit.

## 4. SSH-agent behavior

- Create a private container-side SSH-agent socket and relay the standard agent protocol to the selected host agent. Do not read/copy private keys or implement a new key store.
- `auto` selects `SSH_AUTH_SOCK` from the current explicit host mutating invocation. An explicit absolute host socket selector supports alternative agents without importing `~/.ssh/config` or mounting its contents. Persist the selector, not keys or an inherited environment dump.
- An explicit `up`/enable-style reconciliation refreshes automatic host socket selection. Reconnect if the selected agent is recreated at the same path; if automatic selection has changed, refresh through the explicit host invocation rather than choosing an arbitrary socket from the filesystem or trusting a container-supplied path. Never silently replace an explicit selector with another agent.
- Verify/connect the selected host endpoint with appropriate Unix ownership/type checks. Diagnose absent/unreachable agents, no identities, and required host confirmation. Do not scan `~/.ssh`, automatically run `ssh-add`, or start a replacement host agent.
- Support independent concurrent agent connections, binary framing, backpressure, EOF, cancellation, and revocation. Relay protocol extensions correctly within documented compatibility/size bounds; do not silently strip extensions needed by a supported agent.
- Host hardware-key/agent confirmation is permitted as the normal result of an authorized SSH signing request and remains subject to a bounded operation timeout. It is distinct from automatically initiating HTTPS login.
- Inject the verified managed `SSH_AUTH_SOCK` after generic effective-environment sanitization into enrolled lifecycle/SSH children. Existing snapshot code strips all `SSH_` entries; do not weaken that filter globally or persist stale session sockets in reusable snapshots. With this capability disabled, retain cdenv's pre-feature environment behavior, including its existing SSH-entry sanitization.
- Host `IdentityFile` keys not in the selected agent, host aliases, `ProxyJump`, `IdentityAgent` config parsing, and host `known_hosts` are not automatically replicated. Preserve normal SSH host-key verification and provide setup guidance rather than disabling verification.

## 5. Optional Git author identity

`git-identity` is separately enabled and defaults off. Read only host `user.name` and `user.email` from the trusted host configuration context and make them available as defaults in managed container processes.

- Fill each genuinely missing field independently. Preserve existing container/system/global/local Git identity, conditional configuration, command overrides, and explicit author/committer environment overrides. Do not use a high-precedence blanket override that changes an existing author identity.
- Use reversible cdenv-owned configuration defaults, not edits to the checkout or wholesale copying of host `.gitconfig`. Values are data, not shell command fragments.
- Refresh through explicit reconciliation; disabling removes cdenv defaults for future invocations. Already created commits or running processes cannot have their metadata recalled.
- Missing host name/email is advisory and cannot block readiness. Status reports configuration/availability without dumping identity values by default.
- Do not copy `user.signingKey`, enable `commit.gpgsign`, import GPG trust/keys, configure a signing helper, or grant another credential capability.

## 6. Lifecycle, reconnect, and failure contract

### Start and reconciliation

Provision/verify the agent, establish the credential bridge, and enroll the initial effective environment before the first credential-using container lifecycle stage. This applies to initial create, resume, rebuild candidates, foreground hooks, detached post-readiness work, and pre-SSH `postAttachCommand`; it must not depend on the later SSH handshake or existing port-forwarding readiness checkpoint. Host `initializeCommand` continues using the host directly.

Keep one workspace owner alive after `up` returns even when there are no declared ports or SSH clients. A shell closing must not revoke a detached job's access. The guarantee lasts while the host broker/transport is available, not while the host is asleep/offline. No systemd/launchd integration, global daemon, reboot autostart, or repair from read-only commands is introduced.

Retry transient transport failure only to the same verified target with bounded backoff. Reject unknown/newer protocol, wrong build, user/identity mismatch, ambiguous containers, and externally replaced generations. Explicit `up` restores services after host reboot or supervisor loss and performs supported compatibility repair. Ordinary SSH/status operations only diagnose.

Rebuild may temporarily need separate leases for the currently active generation and a host-verified, operation-owned candidate so its hooks can authenticate before readiness. Never adopt a candidate based on its own claims. On successful handoff, revoke the old generation; on rollback, revoke the failed candidate and retain the valid old services. Do not replay old-generation replies into a new connection. Compose partial replacement follows existing explicit recovery semantics.

### Availability versus configuration failure

| Condition | Required behavior |
|---|---|
| Capability disabled or no live container | Report disabled/inactive; no lookup/login side effects. |
| Valid bridge, host agent absent/empty, helper missing, or host login required | Report the affected backend unavailable/degraded; otherwise successful `up` remains successful. |
| Helper credentials not yet queried | Report untested/unknown availability, not a claim that authentication will succeed. |
| Unsafe policy/path, incompatible protocol/build, bridge installation failure, or failure establishing required bridge readiness | Mutating command returns nonzero; preserve checkout, volumes, and otherwise running container. |
| Hook needs unavailable/rejected credentials | Hook fails through existing lifecycle/checkpoint rules. Enabling credentials later must not silently retry an indeterminate one-time stage. |
| Running broker/backend later degrades | Credential operations fail promptly; do not terminate unrelated SSH sessions, background jobs, or port forwards. |
| Identity metadata missing | Advisory only. |

Do not actively fetch tokens, trigger login, perform signing, or erase credentials to make `status`/`doctor` appear conclusive. Read-only checks may inspect authenticated supervisor state and perform non-mutating endpoint health inspection, without starting/repairing services.

### Revocation and stop

- Recheck grant authority at dispatch and before releasing a newly obtained HTTPS response. Use a grant revision/epoch so in-flight results from revoked permission are not delivered.
- A successful capability disable/origin deny takes effect in the live broker before reporting success. Cancel affected pending lookups and close affected active SSH-agent streams; leave other capabilities and services running.
- Persist revocation so restarting a broker cannot restore removed permission. If a live broker cannot acknowledge revocation and cannot be safely proven stopped, return nonzero with explicit revocation-unconfirmed guidance; never claim success or signal an unverified PID. Unknown control state is not permission to select another process.
- `down` stops credential streams/bridge and eligible supervisor resources while retaining configured grants for the next explicit `up`. Stop only verified owned resources.
- Revocation prevents future broker use; it cannot recall an HTTPS credential or identity already delivered, an already completed signature, or an authenticated network connection established using those credentials.

## 7. Security and preservation invariants

- This is an explicit delegation to trusted workspace code, not a sandbox against container root, Docker-daemon authority, or deliberate credential exfiltration.
- HTTPS credentials necessarily enter container memory. Origin filtering controls which host credentials can be requested, not where an extracted token can subsequently be used or which repositories its issuer permits. General SSH-agent access is not Git-only or destination-scoped authorization.
- Never mount/copy host credential stores, `.ssh`, private keys, `.gitconfig`, keychain databases, or host sockets into the container as a shortcut. Reading trusted host configuration and providing the explicitly selected identity fields is not wholesale configuration replication.
- No cdenv credential response/request bodies in logs, debug output, error summaries, JSON status, persisted host state, snapshots, process arguments, or environment variables. Use private pipes/memory for payloads; executable/socket paths and nonsecret integration metadata are allowed in configuration/environment.
- Raw `get` requests may themselves contain secrets. Bound and validate input before use, suppress raw helper stdout/stderr from diagnostics, and use redacted debug representations. Do not merely redact a guessed password field after recording a complete request.
- Validate origin/field syntax and conflicting representations; reject newline/NUL injection, malformed/oversized frames, invalid protocol versions, cross-workspace/generation requests, and attempts to supply host execution context. Unrecognized fields must not bypass policy.
- Honor existing atomic writes, ownership/mode/symlink checks, lock ordering, exact resource identity, and checkout/named-volume preservation. Keep credential RPC stdout separate from diagnostics and lifecycle output.
- No Git mutations after clone beyond existing user/repository code and the explicit Feature lock exception. In particular, do not rewrite remotes, inject token-bearing URLs, repair SSH URLs, or write the shared `.git/config`.

## 8. Integration and compatibility work

Likely integration points (paths are relative to the source worktree):

- `crates/cdenv-core`: validated capability/policy identifiers, protocol/health types, and safe serialization where shared.
- `crates/cdenv-cli/src/command_line.rs`, `main.rs`, `paths.rs`, and state/storage modules: credential commands, host-owned staged/bound grants, schema changes, and authenticated control.
- `crates/cdenv-cli/src/forwarding.rs` and `forwarding_reconciliation.rs`: generalize workspace service ownership without making credential health a prerequisite for unrelated port forwarding.
- `crates/cdenv-cli/src/git.rs` and a focused host credential adapter: lookup-only subprocess execution with a secret-safe path distinct from ordinary operation logging.
- `crates/cdenv-cli/src/agent_environment.rs`, `lifecycle_orchestration.rs`, `proxy.rs`, and image/Compose rebuild flows: early readiness, process integration, compatibility repair, and generation handoff.
- `crates/cdenv-agent`: static helper/bridge operations, private sockets, controlled process overlays, SSH/lifecycle injection, and cleanup.
- `reporting.rs`, `doctor.rs`, and shared status schemas: configured/active/live/backend dimensions and actionable read-only diagnostics.

Keep host I/O and helper execution out of `cdenv-devcontainer`; repository `customizations`, `remoteEnv`, and advisory `secrets` must not become permission sources. Load the Rust best-practices skill when implementing; prefer narrow test seams, typed errors, structured concurrency, and redacted secret-bearing types rather than a general remote-execution framework.

Add an ADR describing the explicit opt-in host capability layer and its DevPod comparison. Update `docs/operations.md`, `docs/cdenv-devcontainer-v1-support.md`, and the implementation plan's previous non-goals/CLI/runtime descriptions. Preserve existing accepted configuration behavior when capabilities are off. Version/migrate state and agent/control protocols through explicit mutating paths; never reinterpret old grants as new capabilities or migrate during SSH/status/doctor. If implementation requires changing existing profile semantics, follow the repository's profile-version policy rather than silently changing V1.

Document minimum/tested host and container Git/OpenSSH/helper combinations, the bounded credential-protocol surface, noninteractive helper behavior, missing-host-key/agent remediation, socket refresh, revocation limitations, and the managed-process-only integration boundary. Do not claim automatic portability of every configuration that happened to clone successfully on the host.

## 9. Acceptance criteria

### Product workflows

- [ ] The three initial capabilities are independently opt-in; unknown/new future capabilities cannot inherit grants. `credentials` commands support enable, origin adjustments, selective/all disable, staged grants, and read-only human/JSON status.
- [ ] A staged grant plus explicit `create --name` allows a private dependency fetch in the first `onCreateCommand` without a prior failed create or SSH connection. Ordinary host cloning retains its existing authentication behavior.
- [ ] HTTPS fetch/push in lifecycle hooks and managed PTY/non-PTY SSH sessions uses current host helper credentials. A token rotation is observed on later lookup without cdenv token persistence.
- [ ] Origin grants handle default/explicit ports, canonical hosts/IPv6, username context, private servers, and actual paths. Two accounts/repositories on one server and private submodules select the right credentials. Adding a remote or submodule cannot authorize a new origin.
- [ ] For granted origins, fake preexisting generic and path-specific store/cache helpers receive neither lookup nor store/erase traffic or forwarded tokens. Ungranted origins retain normal container helper behavior without host access. Existing configuration files are byte-for-byte preserved.
- [ ] Lookup-only semantics and stale/rejected-token recovery are explicit. Host login is not initiated by a container request, and unavailable helpers/agents produce useful value-free diagnostics rather than hangs.
- [ ] SSH operations use the selected host agent without a private key or host socket mount in the container. Cover concurrent clients, host confirmation, absent/empty agents, same-path agent restart, explicit versus automatic selection, and explicit socket refresh.
- [ ] Author identity inheritance is independently enabled, fills missing name/email separately, respects per-repository/config/environment/command overrides, and does not enable signing. Disable restores cdenv-owned defaults without editing user files.

### Lifecycle and isolation

- [ ] Services exist with zero forwarded ports and zero SSH sessions, outlive the initiating `up`, support detached jobs, and remain usable when one of several SSH sessions disconnects.
- [ ] Credentials are available before foreground/detached lifecycle work and before `postAttachCommand`; SSH snapshot sanitization still excludes arbitrary stale `SSH_` values.
- [ ] Endpoint loss, supervisor crash, reconnect, host reboot plus explicit `up`, down/up, rebuild success/rollback, and Compose partial replacement have tested ownership, grant, and generation behavior. Stable endpoints do not route requests to an unauthorized generation.
- [ ] Disable/deny suppress pending responses, close affected streams, and prevent later requests/restart from restoring revoked grants. Success means confirmed revocation; unconfirmed failure is reported honestly. Unrelated ports, SSH sessions, and containers remain running.
- [ ] Backend unavailability alone does not fail otherwise successful `up`; unsafe/failed bridge setup does return nonzero; hooks retain their existing failure/retry/checkpoint semantics. Status distinguishes configured, active, healthy transport, untested credentials, degraded, and revoked/inactive facts.
- [ ] Read-only `status`/`list`/`doctor` do not grant access, migrate state, start/repair credential services, retrieve tokens, sign, or trigger login. Establishing an SSH transport does not grant or repair credential services; authorized hook/user Git requests remain allowed. Repository configuration cannot grant capabilities.

### Security and release evidence

- [ ] Unit/component tests cover malformed/truncated/oversized frames, bounds, cancellation/timeouts, concurrency/backpressure, wrong installation/workspace/container/user/generation/build/protocol/grant revision, unsafe socket paths, and attempted host command/config/environment injection.
- [ ] Secret-marker tests inspect cdenv-managed files, logs/debug output, stdout/stderr diagnostics, status JSON, process arguments/environment, and helper configuration. Markers appear only in the intended private credential transport/test recipient; never in cdenv persistence or native container storage helpers.
- [ ] Ordinary workspace tests use fake host Git/helpers/agents and require no Docker, public network, live keychain, or real credentials. Add nonempty opt-in integration coverage using real Git/SSH and controlled authenticated HTTPS/SSH fixtures with proper host-key/TLS trust, not verification bypasses.
- [ ] Run the supported Linux architecture gates; record macOS Apple-silicon Docker Desktop smoke for the private Exec transport, host keychain/helper compatibility, and SSH agent selection. Document which provider interactions are simulated versus verified; never skip missing required prerequisites into a passing gate.
- [ ] Existing `cargo xtask check`, profile/OpenSSH interoperability, checkout preservation, and release/protocol compatibility gates pass. Updated documentation and the ADR describe the actual implemented guarantees and limitations.

## Reference: DevPod comparison

The reference point is the default-branch source at commit [`5a0efcbff6610ab114b421f68a890739a452e66b`](https://github.com/loft-sh/devpod/tree/5a0efcbff6610ab114b421f68a890739a452e66b), not a claim of runtime interoperability or equivalence with DevPod Pro's server-managed credentials.

- [Lookup-only helper](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/cmd/agent/git_credentials.go#L47-L78) and [host `git credential fill`](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L203-L230) validate on-demand host delegation and get-only scope.
- [SSH-stdio/gRPC service transport](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/tunnel/services.go#L35-L150) supports sharing service ownership with forwarding, but cdenv uses private Unix sockets and existing Docker Exec rather than container HTTP plus another SSH/gRPC layer.
- [HTTP-path handling](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L233-L265) motivates preserving actual path/account context; cdenv must not substitute the initial repository's path for unrelated requests.
- [Helper configuration/removal](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/gitcredentials/gitcredentials.go#L33-L93) motivates owned, non-destructive integration instead of rewriting/removing the generic credential section.
- [Browser SSH backhaul](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/cmd/up.go#L823-L889) and [early setup helper](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/cmd/agent/container/setup.go#L164-L201) motivate explicit workspace lifetime and availability before hooks.
- [Automatic host key loading](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/ssh/ssh_add.go#L18-L75) is deliberately not copied; provide explicit agent selection and diagnostics instead.
- [Missing author identity setup](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/cmd/agent/container/credentials_server.go#L154-L191) motivates separately enabled name/email defaults.
- [Default-on context options](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/config/context.go#L27-L69) and [raw debug request logging](https://github.com/loft-sh/devpod/blob/5a0efcbff6610ab114b421f68a890739a452e66b/pkg/credentials/server.go#L113-L123) are deliberate differences: cdenv requires explicit grants and excludes payloads from all diagnostics.
