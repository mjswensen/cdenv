# cdenv V1 operations guide

cdenv implements only the pinned
[`cdenv-devcontainer-v1`](cdenv-devcontainer-v1-support.md) profile. The support
matrix, not the broader Dev Container specification or a reference CLI, is the
runtime contract. V1 does not launch editors, update itself, use remote Docker,
implement Compose V1, install `sshd`, publish SSH port 22, or promise behavior
outside that profile.

## Supported hosts and exact dependency baseline

Release archives and complete automated Docker/OpenSSH suites cover Linux
Linux arm64 and macOS arm64. x86_64 hosts are temporarily unsupported. A macOS
arm64 archive is released only with a recorded Apple Silicon Docker Desktop smoke.

| Dependency | Minimum verified V1 baseline |
|---|---:|
| Docker Engine | 29.6.2, API 1.55, local Unix socket |
| Docker CLI | 29.7.1 |
| Docker Compose V2 | 5.3.1 |
| Docker Buildx / BuildKit | 0.36.0 / 0.31.2 when building archives or Dockerfiles |
| OpenSSH client | 10.0p2 |
| Git | required by `create` |

CI runs clean, locked arm64 checkouts against explicit minimum and pinned
Docker/Compose/OpenSSH jobs. Missing dependencies, wrong architectures, missing
fixtures, zero tests, skips, and below-baseline versions fail. Node.js, an editor,
and the reference Dev Container CLI are not runtime or release-suite dependencies.

## Install, root selection, and uninstall

The simplest installation uses the release installer. It detects Linux arm64 and
macOS arm64, downloads the matching release archive, verifies
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

**Implementation status: permission management and bounded credential parsing
only. Credential forwarding is not available in this build.** Production
`create` and `up` compose planning, Docker/Compose reconciliation, agent and SSH
provisioning, lifecycle execution, environment capture, and declared forwarding
when capabilities are disabled. Configured credential capabilities fail during
early preflight, before container lifecycle execution. Production `down` and
`rebuild` are wired for workspaces without configured credential capabilities;
they do not claim credential leases or broker handoff. Saving permission does not
make Git in a container authenticate. See [ADR 0002](adr/0002-opt-in-host-capabilities.md)
for the credential-specific boundary.

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
`auto`, not the current environment or a discovered socket. This foundation does
not resolve or connect either kind of selector.

For a not-yet-created explicit name, stage permission without creating a
checkout/container or running any helper/login operation:

```sh
cdenv credentials enable project git-https --host https://github.com
cdenv credentials enable project ssh-agent
cdenv credentials enable project git-identity
cdenv credentials status project --json  # staged; transport inactive
cdenv create --name project https://github.com/example/project.git
cdenv credentials status project        # bound; integration unavailable
```

This example currently demonstrates **permission binding and host cloning**, not
container credential forwarding. A successful explicitly named clone binds the
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

Successful stage/enable means **permission saved**, not backend availability.
For an existing active generation, enable/allow save permission but return
nonzero with explicit integration-unavailable guidance. `status`, `list`, and
`doctor` share staged/bound/stale permission facts. They report inactive
transport and uninspected backends; they never fetch a token, inspect identity
values, sign, run login, migrate, or start/repair a broker.

Disable without capabilities discards all current grants; selective disable
discards only those capabilities, including their origins/selectors. Deny and
disable persist revocation first. This build can confirm success only when the
supervisor is absent or its lifetime lock proves it stopped. Unknown control
state or a held lifetime lock returns **revocation unconfirmed** without
signalling any PID or stopping unrelated services. Retry after verified service
shutdown; do not mistake the persisted revocation for an acknowledgement from a
live broker. Live selective reconciliation is still part of the unfinished issue.

### Delegation and compatibility boundaries

The accepted runtime design delegates to trusted workspace code; it is not a
sandbox against container root, Docker authority, or deliberate exfiltration.
HTTPS tokens necessarily enter container memory and cannot be recalled after
delivery. Origin filtering does not restrict where a copied token is used or
which repositories its issuer permits. General SSH-agent access includes signing
and is neither Git-only nor destination-scoped. Author name/email is separately
enabled and must not inherit signing keys/configuration, GPG, or Docker credentials.

The eventual managed integration must cover cdenv lifecycle/SSH children and
their descendants, not arbitrary `docker exec`, entrypoints, other users, or
Compose sidecars. It must preserve native helper behavior on ungranted origins
and prevent granted-origin native store/cache helpers from receiving forwarded
tokens. No such helper configuration is installed by this foundation.

Host helpers must eventually run in a trusted neutral configuration context,
without checkout `includeIf gitdir`/`onbranch` portability promises, container
configuration injection, browser login, or cdenv token caching. Lookup-only
`erase` cannot repair rejected/stale tokens: repair/authenticate on the host and
retry a later lookup. URL userinfo, a one-time clone prompt, `.netrc`, custom HTTP
headers, URL rewriting, and arbitrary provider mechanisms are not implicitly
portable. Host aliases, `IdentityFile`, `ProxyJump`, `IdentityAgent`, and
`known_hosts` are not automatically replicated; never disable host-key or TLS
verification to work around missing setup.

No host Git/OpenSSH/helper interoperability matrix, noninteractive provider
behavior, SSH-agent refresh/confirmation result, or macOS keychain smoke has yet
been verified for live forwarding. The [ADR](adr/0002-opt-in-host-capabilities.md#current-bounded-surface)
publishes the implemented parser limits. Transport bounds, host-helper timeouts,
backpressure, socket refresh, lifecycle handoff, and authenticated real Git/SSH
fixtures remain required before shipping issue 68.

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
and nonempty Linux architecture profile/OpenSSH/installed workflows. The installed
workflow consumes an issue-52 archive, verifies its checksum, runs without Node.js
or an editor, exercises applicable Section 19 commands, and proves no `sshd` or
published port 22. See [release packaging](release-packaging.md).

Before a macOS release, complete and retain the versioned
[Docker Desktop Apple Silicon checklist](smoke/macos-docker-desktop-v1.md). It
records hardware, architecture, OS, Docker Desktop/dependency versions, archive,
checksum, date, and outcome. macOS and editor observations remain recorded smoke
evidence, not automated guarantees; editor notes are never gates.
