# cdenv V1 operations guide

cdenv implements only the pinned
[`cdenv-devcontainer-v1`](cdenv-devcontainer-v1-support.md) profile. The support
matrix, not the broader Dev Container specification or a reference CLI, is the
runtime contract. V1 does not launch editors, update itself, use remote Docker,
implement Compose V1, install `sshd`, publish SSH port 22, or promise behavior
outside that profile.

## Supported hosts and exact dependency baseline

Release archives cover Linux x86_64, Linux arm64, and macOS arm64. Complete
automated Docker/OpenSSH/credential suites run on both Linux architectures. A
macOS arm64 archive is released only with a recorded Apple Silicon Docker Desktop
smoke.

| Dependency | Minimum verified V1 baseline |
|---|---:|
| Docker Engine | 29.6.2, API 1.55, local Unix socket |
| Docker CLI | 29.7.1 |
| Docker Compose V2 | 5.3.1 |
| Docker Buildx / BuildKit | 0.36.0 / 0.31.2 when building archives or Dockerfiles |
| OpenSSH client | 10.0p2 |
| Git | required by `create` |

CI runs clean, locked Linux x86_64 and arm64 checkouts against explicit minimum
and pinned Docker/Compose/OpenSSH jobs. Missing dependencies, wrong architectures, missing
fixtures, zero tests, skips, and below-baseline versions fail. Node.js, an editor,
and the reference Dev Container CLI are not runtime or release-suite dependencies.

## Install, root selection, and uninstall

The simplest installation uses the release installer. It detects Linux x86_64,
Linux arm64, and macOS arm64, downloads the matching release archive, verifies
its adjacent SHA-256 checksum, and installs `cdenv` into a writable directory
already present in `PATH`:

```bash
curl -fsSL https://github.com/mjswensen/cdenv/releases/latest/download/install.sh | sh
```

Set `CDENV_VERSION` to install a tagged release, `CDENV_REPOSITORY` to use a
fork, or `CDENV_INSTALL_DIR` to select a destination explicitly. The installer
requires `curl`, `tar`, and a SHA-256 utility (`sha256sum` on Linux or `shasum`
on macOS). It never modifies shell startup files. If no writable directory is
already on `PATH`, it stops and explains how to retry with
`CDENV_INSTALL_DIR`.

For a manual installation, download the archive for the host OS/architecture
together with its adjacent `.sha256`, verify the checksum before extraction,
and install its sole `cdenv` executable on `PATH`. See [release packaging](release-packaging.md)
for artifact identity and checksum details. There is no updater, privileged
helper, or installation-wide daemon.

By default cdenv uses its documented home location. Set `CDENV_HOME` or pass
`--root PATH` to select an absolute private root; `--root` is the explicit
per-command override. Do not share or copy roots between users or installations.
A root contains installation identity, checkout registry/state, private keys,
managed SSH configuration, forwarding control files, explicit credential
permission records, operation logs, and caches.
The only managed host path outside it is an explicitly consented SSH Include;
the only checkout write is an explicit Feature lock operation.

To uninstall:

1. run `cdenv down <name>` for every entry in `cdenv list`;
2. terminate any foreground `cdenv forward` commands;
3. remove the consented cdenv `Include` line from the user SSH config, if present;
4. remove the installed executable;
5. remove the selected root after retaining any support records desired.

This removes cdenv state and eligible cache/generated-image references. It never
removes a checkout or named volume; remove those separately and deliberately.

## Profile revisions, upgrades, and state migration

The current profile revision is `cdenv-devcontainer-v1`, pinned to the exact
specification/schema revision in the support matrix. Profile semantic changes
require a new profile revision. Releases do not silently reinterpret an active
workspace.

State and protocol schemas are versioned. A newer binary may perform only an
explicitly implemented compatibility migration during a mutating `up`/`rebuild`
path. Unknown, newer, corrupt, or indeterminate state fails closed. `doctor`,
`list`, `status`, and SSH connection never migrate, chmod, regenerate, start, or
repair state as a side effect. Before upgrading, run `down`, retain the root and
checkouts, install the checksum-verified replacement, run `doctor`, then `up`.
Incompatible agent build/protocol/generation mixtures are rejected and repaired
only by an explicit up-style reprovision or rebuild. Downgrades are unsupported
unless that release explicitly recognizes the stored schemas.

## Feature trust, lock policy, and source sanitization

Repository, Dockerfile, Compose, Feature, and lifecycle code is trusted code with
Docker-user authority. Review it before `create`, `up`, `lock`, or `rebuild`.
Access to the Docker daemon, cdenv executable, root, private SSH keys, or supervisor
control paths is privileged; cdenv is not a sandbox against trusted inputs.

An adjacent `devcontainer-lock.json` is frozen when present: every resolved
Feature, digest, integrity value, and dependency must match. Missing, stale,
inconsistent, or corrupt entries fail closed. Only `cdenv lock` creates or changes
the lock; lock creation is an explicit trust-on-first-use decision and is the sole
cdenv command permitted to write the checkout after clone.

Public OCI and unauthenticated HTTPS Features use normal TLS and hostname checks,
bounded redirects/download/decompression, SHA-256 integrity, strict media/layer
selection, and archive path/type containment. Local Features must remain beneath
the selected configuration directory. Credentials in URLs, insecure HTTP, private
Feature authentication, custom TLS bypass, traversal/absolute paths, escaping
links, devices, oversized input, and cross-origin credential forwarding are
rejected. Docker image credentials are not Feature credentials. Effective Compose
models, environment values, registry headers, and substituted secrets are not
persisted or logged.

## Lifecycle, recovery, SSH, and forwarding

Lifecycle checkpoints are generation-scoped. Successful one-time stages are not
repeated; `postStartCommand` follows an actual start and `postAttachCommand`
follows each new SSH transport. A definitely pending operation may retry. A
cancelled/crashed one-time stage left running is indeterminate: preserve the
checkout and named volumes and use `rebuild` rather than editing state. The current
state schema records the stage but not an authenticated Docker Exec runner identity,
so `down` and `rebuild` never guess a process or PID; they report the runner as
indeterminate when safe bounded cancellation cannot be proven. Compose replacement
is build-first but not atomic; exact per-service partial evidence is persisted after
post-recreation failure or cancellation. Ordinary `down` stops only the persisted
managed set and does not delete project networks or named volumes.

cdenv asks before adding its exact `Include` to the user SSH configuration.
Declining (`--no-modify-ssh-config`) does not block `cdenv ssh <name>` or
`ssh -F <root>/ssh/config <name>.cdenv`. Each authenticated connection is an
OpenSSH ProxyCommand stdio transport to the provisioned static agent. There is no
network SSH daemon or port 22 publication.

Declared `forwardPorts` use loopback listeners owned by a workspace-scoped
supervisor from successful `up` through `down`. `cdenv forward` is foreground,
all-or-none, ends on Ctrl+C, and rejects listener conflicts. Non-loopback exposure
requires explicit `--bind` and emits a warning because other hosts may connect.
`appPort` is Docker publication and follows the binding recorded in the support
matrix; inspect it separately from cdenv forwarding.

## Host credential permissions (issue 68)

**Implementation status: the opt-in permission, broker, backend, container
integration, and managed lifecycle workflows are implemented.** The workspace
supervisor owns a generation-scoped credential service even with no TCP listeners
or SSH clients. It creates one verified, selected-user Docker Exec to the static
agent and a protocol-only bounded stdio bridge. The agent creates stable
owner-only container Unix endpoints outside the checkout. Production create/up,
managed lifecycle, and SSH paths dispatch the trusted host Git lookup/identity
backends and selected host SSH-agent relay. Issue 78 tracks the remaining
cross-platform authenticated release evidence; configured CI jobs or component
tests are not substitutes for required successful platform observations. See
[ADR 0002](adr/0002-opt-in-host-capabilities.md).

Permissions are independent, off by default, installation/workspace-scoped, and
never derived from `devcontainer.json`, `customizations`, `remoteEnv`, or
advisory `secrets`. The explicit command is noninteractive consent to the named
authority; there is no enable-all default:

```text
cdenv credentials enable NAME git-https [--host HTTPS_ORIGIN ...]
cdenv credentials enable NAME ssh-agent [--socket auto|ABSOLUTE_HOST_SOCKET]
cdenv credentials enable NAME git-identity
cdenv credentials allow NAME git-https HTTPS_ORIGIN ...
cdenv credentials deny NAME git-https HTTPS_ORIGIN ...
cdenv credentials disable NAME [CAPABILITY ...]
cdenv credentials status NAME [--json]
```

Supply several origins after `--host`, or repeat `--host`. `--root` and `CDENV_HOME` retain
their normal precedence. HTTPS origins allow private/intranet servers, DNS/IDNA
names, and bracketed IPv6. The effective port is explicit in stored/displayed
origins, so `https://git.example`, `https://git.example/`, and
`https://git.example:443` name the same grant. Userinfo, wildcard, query, fragment,
non-root path, control characters, and ambiguous shorthand are rejected. Paths
and usernames are future lookup context, not permission scope.

First HTTPS enable for a bound workspace may derive only the original sanitized
HTTPS source origin. SSH/local sources require explicit origins. Re-enabling
without origins does not widen an existing allowlist, even after every origin
has been denied. `allow`/`deny` never enable a disabled capability. Omitted
`--socket` preserves an existing explicit selector; the first omission records
`auto`, not the current environment or a discovered socket. A mutating
reconciliation resolves `auto` from that invocation's `SSH_AUTH_SOCK`; an
explicit selector uses only its recorded absolute path and never falls back.

For a not-yet-created explicit name, stage permission without creating a
checkout/container or running any helper/login operation:

```sh
cdenv credentials enable project git-https --host https://github.com
cdenv credentials enable project ssh-agent
cdenv credentials enable project git-identity
cdenv credentials status project --json  # staged; transport inactive
cdenv create --name project https://github.com/example/project.git
cdenv credentials status project        # bound; transport healthy after readiness
```

The first create provisions and authenticates the generation-scoped bridge before
`onCreateCommand`, so a private dependency fetch can succeed without a prior SSH
connection or failed create. A successful explicitly named clone binds the
staged record before any future container lifecycle stage. Failed clones retain
staged bytes for retry. A later failure retains the checkout. An automatic name
cannot consume a staged grant; if that conflict is discovered after clone, the
checkout is retained and the command fails. Disable the old grants before
explicitly granting capabilities to that retained workspace. A durable binding
receipt left by an interrupted policy write is reported as `binding_pending` and
allows the explicit enable to retry against only that exact workspace.

State lives at `<root>/credentials/NAME.json` (schema 1, mode `0600`), under a
mode-`0700` directory, with a private receipt at
`<root>/workspaces/NAME/credential-binding.json`. Neither is inside the checkout.
Root/installation identity and the workspace receipt prevent transfer to another
root or replacement workspace; copying/restoring directories can invalidate a
binding. Unknown schemas/capabilities, unsafe owners/modes, symlinks, hard links,
and oversized records are rejected, not repaired by credential commands.
Existing workspace and agent schemas are unchanged.

The private wire protocol is version 1 and permits only Git lookup, approved
SSH-agent byte streams, identity metadata, health/cancellation, and lease stop.
Its published limits are 64 KiB per frame, 32 active streams, 64 queued frames,
512 KiB queued payload, four concurrent helper operations, a 5-second handshake,
30-second operation timeout, 60-second idle timeout, and three reconnect attempts
using 100 ms exponential backoff capped at two seconds. Unknown kinds/versions,
wrong scoped identities or revisions, old generations, unsafe endpoints, and
saturation fail closed. Payloads have redacted diagnostics and remain on private
pipes/memory.

When `git-identity` is reconciled, host Git is queried only for `user.name` and
`user.email` from the same trusted neutral configuration context used by HTTPS
lookups. Each missing or invalid field remains independently unavailable. The
closed, bounded metadata snapshot crosses only the authenticated broker identity
operation and is stored in a cdenv-owned mode-`0600` file outside the checkout.
The agent's Git wrapper probes each field in the invocation's original effective
system/global/local/conditional, `GIT_CONFIG_*`, `-c`, and `--config-env` context,
then supplies only absent fields as literal arguments. It does not edit Git files,
copy host configuration, interpret identity as shell, or alter signing and
credential settings. Explicit author/committer environment values retain Git's
normal precedence. Refresh atomically replaces only cdenv metadata; disable
removes only that metadata for later invocations.

Successful stage/enable means **permission saved**, not backend availability.
For an enrolled active generation, enable/allow/deny/disable reconcile the new
monotonic revision through the authenticated supervisor control channel without
rerunning lifecycle hooks or restarting listeners or the container. An active
generation that predates credential enrollment returns nonzero guidance to run
`cdenv up`; it is never repaired by status, doctor, or an SSH invocation.
`status`, `list`, and `doctor` share per-capability configured/bound/active,
transport, and backend facts. A healthy transport is reported separately from
an untested lookup backend. These commands never fetch a token, inspect identity
values, sign, run login, migrate, or start/repair a broker.

Disable without capabilities discards all current grants; selective disable
discards only those capabilities, including their origins/selectors. Deny and
disable persist revocation first, cancel affected pending results, and require
the exact supervisor to acknowledge the durable revision. SSH streams for a
revoked agent grant are closed without stopping unrelated capabilities,
listeners, SSH sessions, jobs, or containers. Unknown/mismatched control state
or acknowledgement timeout returns **revocation unconfirmed** without signalling
any PID; retry retains the durable revocation. If the verified supervisor is
absent, the lifetime lock and owned-resource checks must prove it stopped before
success is reported.

### Delegation and compatibility boundaries

The accepted runtime design delegates to trusted workspace code; it is not a
sandbox against container root, Docker authority, or deliberate exfiltration.
HTTPS tokens necessarily enter container memory and cannot be recalled after
delivery. Revocation likewise cannot undo completed signatures, erase identity
metadata already read by a process, or terminate already authenticated remote
connections. Origin filtering does not restrict where a copied token is used or
which repositories its issuer permits. General SSH-agent access includes broad
signing authority and is neither Git-only nor destination-scoped. A host agent or hardware key may require confirmation for
a signing request; confirm it on the host within the bounded operation deadline.
If the selected agent is missing or has no keys, start/repair that exact agent or
load the intended key on the host, then run an explicit mutating reconciliation.
An `auto` selector is refreshed only from that invocation's `SSH_AUTH_SOCK`;
explicit paths never fall back, and an agent recreated at the same path reconnects.
No host socket is mounted, no key or SSH configuration is copied, and cdenv never
runs `ssh-add` or starts an agent. Author name/email is separately enabled and must
not inherit signing keys/configuration, GPG, or Docker credentials.

The managed HTTPS integration is enrolled for cdenv lifecycle/SSH children and
their descendants in production. It does not cover arbitrary `docker exec`, entrypoints,
other users, or Compose sidecars. A private mode-`0600` fragment outside the
checkout is included by appending to (never replacing) existing `GIT_CONFIG_*`
command-environment entries. For each exact granted origin it resets the effective
helper chain, installs only the static cdenv helper, and enables
`credential.useHttpPath`; ungranted origins retain all native behavior. Existing
system/global/local/include files remain unchanged. Removing the owned fragment
and enrollment restores underlying behavior for later processes.

Host HTTPS lookup uses the explicit host launch context and runs `git credential
fill` from a canonical neutral, non-repository directory. The environment is
cleared, then only `HOME`, `PATH`, locale, XDG configuration, explicit trusted
Git global/system config paths, DBus/GitHub CLI config, and the GCM credential
store selector are retained. Git repository/config injection variables are not
imported. The actual HTTPS host/port, path, and supplied username travel on
private stdin; fixed arguments contain no credential values. Normal trusted
user/system helpers, includes, and per-URL matching therefore apply. Conditional
`includeIf gitdir`/`onbranch` rules are intentionally not reproduced because a
neutral lookup has no trusted checkout or branch context.

Lookups are uncached and lookup-only. They permit four concurrent Git processes,
32 KiB each for request and stdout, 8 KiB discarded stderr, and a 30-second
execution deadline followed by at most a two-second termination grace. Timeout,
cancellation, disconnect, or future revocation kills the owned process group and
reaps Git. The private subprocess path writes no operation log and never returns
helper stdout/stderr in diagnostics. Missing Git/helpers, helper failure,
login-required state, incomplete/expired credentials, unsupported fields,
output overflow, timeout, and saturation become typed unavailable outcomes; they
do not tear down unrelated workspace services.

Host lookup sets `GIT_TERMINAL_PROMPT=0`, suppresses Git/SSH askpass, and declares
Git Credential Manager (`GCM_INTERACTIVE=Never`, `GCM_GUI_PROMPT=0`) and GitHub
CLI (`GH_PROMPT_DISABLED=1`) noninteractive modes. These settings cover the
tested helper boundary but cannot prevent arbitrary trusted custom helper code
from launching a GUI. cdenv never intentionally starts browser/OAuth login.
Lookup-only `erase` cannot repair rejected/stale tokens: repair/authenticate on
the host and retry a later lookup. URL userinfo, a one-time clone prompt,
`.netrc`, custom HTTP headers, URL rewriting, and arbitrary provider mechanisms
are not implicitly
portable. Host aliases, `IdentityFile`, `ProxyJump`, `IdentityAgent`, and
`known_hosts` are not portable through the agent protocol and are not automatically
replicated.
Configure destination and host-key trust inside the container; never disable
host-key or TLS verification to work around missing setup.

The release compatibility matrix is deliberately narrower than “all Git
helpers”:

| Surface | Minimum / tested | Evidence |
|---|---|---|
| Host Git | Git 2.39+ standard credential protocol | Real `git credential fill`; controlled shell-helper, trusted include, per-URL/path/account, rotation, expiry, and failure fixtures. |
| Container Git | Git 2.39+ when HTTPS forwarding is used | Static cdenv helper and native-helper isolation tests plus a proper-CA authenticated smart-HTTP fetch/push/stale-token recovery fixture with two paths/accounts and a private submodule; retained clean native architecture runs remain an explicit release blocker. |
| Host/container OpenSSH | OpenSSH 10.0p2+ client and standard agent protocol | Real OpenSSH host-key-verified transport plus controlled real/fake host agents. |
| Git Credential Manager | Noninteractive environment contract only | Prompt suppression is simulated; no provider/keychain login is claimed. |
| GitHub CLI helper | `GH_PROMPT_DISABLED=1` contract only | Simulated environment verification; no provider login is claimed. |
| macOS Keychain/helper | Not yet verified | Requires the retained Apple-silicon Docker Desktop smoke record. |

A trusted custom helper may still display a GUI despite the declared suppression
variables. Fake agent fixtures cover extension framing, bounds, concurrent
connections, same-path refresh, and cancellation. The packaged credential suite
uses a controlled hostname-valid TLS certificate signed by its private test CA
for real smart-HTTP fetch/push, stale-token recovery, two path/account grants, a
private submodule, native-helper isolation, token rotation, and denied-origin
checks. It also uses a real host `ssh-agent` for an identity-backed signing
operation, same-path socket restart and unavailable-backend recovery, and through
create, foreground/detached hooks, postAttach, PTY/non-PTY and concurrent SSH
children, down/up, rebuild, controlled
supervisor-loss recovery, and live revocation that preserves an old SSH session
while removing credential enrollment from a new child. The [ADR](adr/0002-opt-in-host-capabilities.md#current-bounded-surface)
publishes parser and helper limits. The static helper bounds and validates `get`
before opening the private socket; `store` and `erase` are successful no-ops and
unknown operations fail closed.

## Diagnostics, retention, and release evidence

Use `cdenv status <name>` for desired/active/live dimensions and `cdenv doctor`
for independent read-only checks. `doctor --json` emits one versioned support
document. If Docker, Compose, OpenSSH, or Git is missing or unsupported, install
the declared version and rerun; cdenv never substitutes another implementation.
Duplicate labels, external replacement, drift, invalid desired configuration,
and stale control files are reported rather than resolved by selecting arbitrary
resources. Never hand-edit state as a repair.

Ordinary down/up and rebuild retain checkouts and named volumes. cdenv-generated
images and downloaded Feature cache entries become cleanup candidates only after
no verified workspace/lock reference retains them. Stop workspaces first, inspect
labels/references, then remove only unreferenced cdenv-generated images and the
selected root's cache/tmp data. Do not run broad Docker prune as cdenv cleanup.
Removing the root is the final uninstall cleanup and discards recovery evidence.

Automated guarantees consist of the strict Rust/`cargo-deny` gate, package checks,
and nonempty Linux x86_64/arm64 profile, credential, OpenSSH, and installed
workflows. The installed
workflow consumes an issue-52 archive, verifies its checksum, runs without Node.js
or an editor, exercises applicable Section 19 commands, and proves no `sshd` or
published port 22. See [release packaging](release-packaging.md).

Before a macOS release, complete and retain the versioned
[Docker Desktop Apple Silicon checklist](smoke/macos-docker-desktop-v1.md). It
records hardware, architecture, OS, Docker Desktop/dependency versions, archive,
checksum, date, and outcome. macOS and editor observations remain recorded smoke
evidence, not automated guarantees; editor notes are never gates.
