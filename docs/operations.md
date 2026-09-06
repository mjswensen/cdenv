# cdenv V1 operations guide

cdenv implements only the pinned
[`cdenv-devcontainer-v1`](cdenv-devcontainer-v1-support.md) profile. The support
matrix, not the broader Dev Container specification or a reference CLI, is the
runtime contract. V1 does not launch editors, update itself, use remote Docker,
implement Compose V1, install `sshd`, publish SSH port 22, or promise behavior
outside that profile.

## Supported hosts and exact dependency baseline

Release archives and complete automated Docker/OpenSSH suites cover Linux
x86_64 and Linux arm64. A macOS arm64 archive is released only with a recorded
Apple Silicon Docker Desktop smoke. Intel macOS and other hosts are unsupported.

| Dependency | Minimum verified V1 baseline |
|---|---:|
| Docker Engine | 29.6.2, API 1.55, local Unix socket |
| Docker CLI | 29.7.1 |
| Docker Compose V2 | 5.3.1 |
| Docker Buildx / BuildKit | 0.36.0 / 0.31.2 when building archives or Dockerfiles |
| OpenSSH client | 10.0p2 |
| Git | required by `create` |

CI runs clean, locked x86_64/arm64 checkouts against explicit minimum and pinned
Docker/Compose/OpenSSH jobs. Missing dependencies, wrong architectures, missing
fixtures, zero tests, skips, and below-baseline versions fail. Node.js, an editor,
and the reference Dev Container CLI are not runtime or release-suite dependencies.

## Install, root selection, and uninstall

Download the archive for the host OS/architecture together with its adjacent
`.sha256`, verify the checksum before extraction, and install its sole `cdenv`
executable on `PATH`. See [release packaging](release-packaging.md) for artifact
identity and checksum details. There is no installer, updater, shell startup
mutation, privileged helper, or installation-wide daemon.

By default cdenv uses its documented home location. Set `CDENV_HOME` or pass
`--root PATH` to select an absolute private root; `--root` is the explicit
per-command override. Do not share or copy roots between users or installations.
A root contains installation identity, checkout registry/state, private keys,
managed SSH configuration, forwarding control files, operation logs, and caches.
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
checkout and named volumes and use `rebuild` rather than editing state. Compose
replacement is build-first but not atomic; partial replacement is reported as
interrupted/drifted. Ordinary `down` stops only the persisted managed set and does
not delete project networks or named volumes.

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
