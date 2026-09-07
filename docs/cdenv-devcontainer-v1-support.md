# `cdenv-devcontainer-v1` support matrix

`cdenv-devcontainer-v1` is pinned to Dev Container specification commit
[`c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421`](https://github.com/devcontainers/spec/commit/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421).
The vendored base schema has SHA-256
`a0883c0405ff433db188849d458fb20b9c0d73e0ba1a6e44c1d83f3b485408dd`.
The [feasibility ADR](adr/0001-cdenv-devcontainer-v1-feasibility.md)
records the evidence behind this contract.

This matrix fixes production semantics; it does not imply that the disposable
spike is a production implementation. Unknown behavioral properties and known
unsupported values fail with a property path and this profile revision.

## Parsing and scenarios

| Input | V1 disposition |
|---|---|
| Configuration discovery | Supported: explicit repository-relative selection, `.devcontainer/devcontainer.json`, then `.devcontainer.json`, with canonical checkout containment. |
| JSONC comments | Supported. |
| JSONC trailing commas | Rejected deliberately. The selected parser is configured strictly and the repository configuration has no trailing comma. |
| Encoding and limits | UTF-8 only; configuration ≤ 1 MiB; nesting ≤ 64; array/object ≤ 4096 entries; string/key ≤ 256 KiB. |
| Scenario choice | Exactly one of `image`, `build`, or `dockerComposeFile`. |
| Deprecated top-level `dockerFile`/`context` form | Unsupported; use `build.dockerfile` and `build.context`. |
| Unknown top-level behavioral properties | Rejected. `$schema` and tool-namespaced `customizations` are exceptions described below. |

## Top-level property matrix

| Property | Status and deliberate V1 interpretation |
|---|---|
| `$schema` | Accepted for authoring; runtime always uses the vendored schema and never follows this URI. |
| `name` | Supported as display metadata. It does not replace cdenv's validated workspace identity. |
| `image` | Supported public/private Docker image reference through the Docker CLI's normal credentials; mutually exclusive with Dockerfile and Compose scenarios. |
| `build` | Supported object form. `dockerfile`, `context`, `target`, string `args`, `cacheFrom`, and non-reserved `options` retain Docker CLI/BuildKit semantics. Context and Dockerfile paths must remain in the checkout. |
| `dockerComposeFile` | Supported string or ordered array for Compose V2 only. Files are explicit and checkout-contained. Interpolated effective Compose configuration is never persisted or logged. |
| `service` | Required and supported as the exact primary service in a Compose scenario. |
| `runServices` | Supported. The persisted managed set includes these services plus dependencies actually started by Compose; all configured services are used when absent. |
| `workspaceFolder` | Supported. Scenario defaults and staged substitutions are applied deterministically. |
| `workspaceMount` | Supported for image/Dockerfile scenarios after target/path and cdenv ownership validation. Compose receives a deterministic override instead. |
| `features` | Supported only for the source classes and Feature rules below. |
| `overrideFeatureInstallOrder` | Supported as a deterministic priority within dependency-valid rounds; it cannot violate hard `dependsOn` edges. |
| `containerEnv` | Supported and merged key-by-key, with later repository values winning. Effective values are not persisted on the host. |
| `remoteEnv` | Supported after actual container-environment capture. Effective values are stored only in restricted container-side state. |
| `containerUser` | Supported; later repository metadata wins. Docker user conflicts in cdenv-owned options are rejected. |
| `remoteUser` | Supported; later repository metadata wins and drives lifecycle/SSH execution. |
| `updateRemoteUserUID` | Supported for Linux named/non-root users when a conflict-free update can be proven. A generated UID/GID layer is used; unsafe/conflicting updates fail. |
| `userEnvProbe` | Supported for schema-defined values; probes run as the effective remote user and captured values are bounded and container-local. |
| `overrideCommand` | Supported with the schema default. Compose command enrichment is explicit in the generated override. |
| `init` | Supported; metadata values merge with boolean OR. |
| `privileged` | Supported with repository trust consent; metadata values merge with boolean OR. |
| `capAdd` | Supported; stable de-duplicating union in metadata order. |
| `securityOpt` | Supported; stable de-duplicating union in metadata order. |
| `mounts` | Supported string/object forms. The normalized target (`target`, `dst`, or `destination`) is the conflict key and the later repository contribution wins. |
| `runArgs` | Supported passthrough after rejecting cdenv-owned name/label, remove, mount/asset, attach/TTY/stdin, and user invariants. |
| `appPort` | Supported as Docker publication at create time. Numeric values bind loopback on the same port; explicit strings retain Docker syntax and non-loopback bindings warn. |
| `forwardPorts` | Supported by the workspace-scoped host supervisor. Integers target container localhost; `service:port` targets a Compose service. Explicit entries override automatic-ignore attributes. |
| `portsAttributes` | Supported for declared ports: `label`, `protocol`, `requireLocalPort`, URL rendering, and schema values described below. |
| `otherPortsAttributes` | Validated and reported, but does not trigger arbitrary process/listening-port discovery in V1. |
| `initializeCommand` | Supported command forms; runs on the host before container mutation and may run more than once. |
| `onCreateCommand` | Supported once per new generation, after Feature-contributed commands. |
| `updateContentCommand` | Supported once per new generation, after `onCreateCommand`. |
| `postCreateCommand` | Supported once per new generation and eligible for detached execution after `waitFor`. |
| `postStartCommand` | Supported after an actual start, not an idempotent `up` of an already-running environment. |
| `postAttachCommand` | Supported once per new SSH transport, including forwarding-only transports; not once per ControlMaster channel. |
| `waitFor` | Supported for schema-defined lifecycle stages. `up` may return after this checkpoint while later stages continue under the agent runner. |
| `shutdownAction` | Merged and displayed. There is no editor-close event; explicit `down` always stops only the persisted managed environment. |
| `hostRequirements` | Supported for CPU, memory, storage, and GPU as described below. |
| `customizations` | Tool-namespaced objects are accepted and preserved. Unknown tools are advisory and not interpreted. They cannot alter cdenv orchestration. |
| `secrets` | Accepted as advisory metadata only. V1 has no secret provider and never resolves or persists secret values. |

## Nested values and limits

### Substitutions

Supported variables are `${localWorkspaceFolder}`,
`${localWorkspaceFolderBasename}`, `${containerWorkspaceFolder}`,
`${containerWorkspaceFolderBasename}`, `${devcontainerId}`,
`${localEnv:NAME}`/`${localEnv:NAME:default}`, and
`${containerEnv:NAME}`/`${containerEnv:NAME:default}`. Host-stage planning
leaves container-environment expressions unresolved; runtime planning resolves
them from the actual active container. Unsupported expressions fail rather
than being copied literally. `${devcontainerId}` is the lower-case base-32
SHA-256 big-integer encoding of canonical sorted cdenv identity labels, padded
to 52 characters.

### Metadata merge

Metadata sources retain image/Feature order and repository configuration is
last. `init`/`privileged` use OR; capabilities/security options and forwarded
ports use stable union; environment and port-attribute objects merge per key;
mounts merge by target; lifecycle contributions concatenate in source order;
entrypoints concatenate; scalar user/probe/wait/shutdown values use the last
present value. CPU, memory, and storage requirements use the maximum, and GPU
requirements retain the strongest compatible request. Generic recursive JSON
merge is not part of V1.

### Lifecycle commands

String commands run through the applicable `/bin/sh`; arrays execute directly;
object entries launch concurrently in stable key order and all must succeed.
Closed/background stdin is used for parallel and detached work. Checkpoints are
atomic. A definitely pending operation can retry; a cancelled/crashed one-time
command left `running` is indeterminate and requires rebuild. Successful stages
are not duplicated.

### Ports

`requireLocalPort=false` may select an available loopback port;
`requireLocalPort=true` reports degraded forwarding and makes `up` nonzero
without destroying the environment. `onAutoForward: openBrowser` and
`openBrowserOnce` print a resolved URL; `openPreview` reports that no embedded
preview exists. `elevateIfNeeded` never silently elevates. Regex/range or
`otherPortsAttributes` behavior requiring automatic listener discovery is
advisory only. Declared and ad-hoc forwards never bind non-loopback without
explicit input.

### Host requirements

CPU, memory, and storage hard-fail only when reliable Docker/host evidence proves
them unmet; inability to measure produces a warning. Required GPU fails when a
reliable probe proves absence. Optional GPU does not fail, and cdenv never grants
GPU access unless requested. Binary units are normalized with checked overflow.

## Feature and lock support

| Source/value | V1 disposition |
|---|---|
| Fully qualified public OCI Feature | Supported over verified HTTPS, including anonymous Bearer challenge, accepted OCI/Docker manifest media types, exactly one Dev Container Feature layer, declared and streamed size bounds, SHA-256 verification, and digest cache. |
| Unauthenticated `https://` archive | Supported with normal certificate/hostname validation, bounded redirects/body/decompression, and required digest verification. URL credentials are rejected. |
| Contained `./` local Feature | Supported relative to the selected `.devcontainer` directory. Absolute paths and parent/root traversal are rejected. |
| Private/authenticated Feature registry or URL | Unsupported. V1 does not read Docker credentials for Features. |
| Insecure HTTP/custom TLS bypass | Unsupported. |
| Signatures/Sigstore | Not required in V1. TLS and digest provide integrity, not publisher identity; lock creation is trust on first use. |
| Options | Boolean/string schema types, defaults, proposals/enums, and unknown-option rejection are supported. |
| Dependencies | Recursive local/public resolution, `dependsOn`, soft `installsAfter`, conflict detection, and cycle errors are supported. Each Feature gets one ordered root build layer. |
| Lockfile | Adjacent `devcontainer-lock.json` is frozen when present. It records version, resolved manifest digest, blob integrity, and dependencies. Only explicit `cdenv lock` writes it. Corrupt cache and stale/inconsistent lock entries fail closed. |

Archives are limited to 64 MiB compressed download, 128 MiB expanded data,
4096 entries, and 1 MiB Feature metadata. OCI, HTTPS, cached, and local paths
apply the relevant bounds; local paths have no compressed download. Redirects
are limited to 5, archive paths to 4096 UTF-8 bytes, and expansion to 100 times
the compressed size. Cumulative sizes and expansion calculations use checked
arithmetic and fail closed on overflow. Traversal, absolute paths, escaping
symlinks, hard links, devices, and unsupported tar entry types are rejected;
extraction is confined to a newly controlled directory.

<!-- feature-source-limits: compressed=67108864 expanded=134217728 entries=4096 metadata=1048576 -->

## Compose and Docker interpretations

Repository Docker build contexts are passed directly to Docker without cdenv
walking their contents: Docker remains authoritative for `.dockerignore`,
Dockerfile-specific ignore files, negation, and context selection. cdenv bounds
and validates only its own generated contexts before materializing them, while
still canonicalizing the selected repository context and Dockerfile below the
checkout trust boundary.

Only `docker compose` V2 and BuildKit are supported. Legacy Compose V1, the
removed classic Docker builder, non-Docker orchestrators, broad project `down`
for normal stop, and implicit reconciliation during drift-safe resume are not.
Generated overrides are canonical JSON and add exact identity labels, workspace
bind/working directory, generated image/agent material, and profile enrichment.
Project identity is installation/workspace scoped. Ordinary resume starts the
persisted container IDs directly; ordinary `down` stops that exact managed set
and preserves containers, networks, and named volumes.

Agent provisioning uses only Docker archive upload, Docker Exec, and the static
Linux agent's filesystem/syscall implementation; it does not require a shell,
`tar`, `id`, `cp`, `install`, or `chmod` in the container. Every successful
`up`-style flow uploads and atomically reinstalls the selected architecture's
agent before verifying its final build ID and protocol as the remote user.
Executable destinations are tried in this order:

1. `/usr/local/libexec/cdenv/cdenv-agent`
2. `/usr/libexec/cdenv/cdenv-agent`
3. `/opt/cdenv/bin/cdenv-agent`
4. `/var/lib/cdenv/bin/cdenv-agent`
5. `/tmp/cdenv-<uid>/bin/cdenv-agent`

Candidates inside the checkout are excluded. The final fallback is accepted
only when its filesystem is executable; its directory and agent remain
root-owned and non-writable by the remote user. Private provisioned assets are
owned by the effective remote UID/GID with their exact restricted modes.
Read-only or `noexec` filesystems produce a typed provisioning failure rather
than weakened permissions.

The verified agent captures the actual Docker Exec environment as the effective
remote user. It runs `none`, login, interactive, or login-interactive probes
through that user's passwd-database shell, resolves deferred
`${containerEnv:...}` segments against the unmodified container environment,
and applies `remoteEnv` last. Snapshots use a bounded binary framing so valid
non-UTF-8 Unix names and values remain exact. They are atomically replaced in a
mode-`0700` selected-user directory as mode-`0600` generation files; the host
receives only the path and entry count. `PWD`, `OLDPWD`, `SHLVL`, `_`, and all
`SSH_` entries are removed before child reuse. Capture requests travel only over
attached Exec stdin, and probe output is suppressed from diagnostics. Readiness
work receives the first snapshot token; a second atomic capture is required
before the token can be published to new SSH transports.

`build.options` and `runArgs` are passed as argument arrays without a shell only
after reserved-option validation. cdenv owns Dockerfile/context selection,
result tags/outputs, identity labels, names, generated assets, user, and
attach/TTY modes. At minimum `-f`/`--file`, `-t`/`--tag`, `-o`/`--output`,
`--iidfile`, `--metadata-file`, and cdenv identity labels are reserved in build
options. The exact conflicting argument is reported.

## Host credential capabilities

Issue 68 adds a host-owned opt-in permission layer, not a Dev Container property
or secret provider. `git-https`, `ssh-agent`, and `git-identity` are independent
permissions under `cdenv credentials`. Repository `customizations`, `secrets`,
`remoteEnv`, remotes, and submodules cannot grant or expand them. Existing accepted
V1 configuration behavior is unchanged, including the removal of all arbitrary
`SSH_` entries from reusable environment snapshots.

**Currently implemented:** private staged/bound permission management,
explicit-name binding after successful host clone, exact HTTPS origin validation,
read-only permission facts, and bounded/redacted Git credential parser types.
**Not implemented:** live host-helper delegation, an Exec credential bridge,
managed Git helper/configuration ownership, SSH-agent relay/environment injection,
identity defaults, and lifecycle/rebuild integration. A saved grant must not be
interpreted as working container authentication. Production lifecycle command
composition remains a prerequisite; see [ADR 0002](adr/0002-opt-in-host-capabilities.md)
and the [operations guide](operations.md#host-credential-permissions-issue-68).

The future runtime guarantee is limited to managed lifecycle/SSH processes and
their descendants in the verified primary container. It excludes arbitrary
`docker exec`, other users, entrypoints, and Compose sidecars. It will not alter
profile defaults, perform Git mutations, or enable signing/GPG/Docker capabilities.
An observable change to existing profile semantics still requires the revision
policy below.

## Change policy

- Bug fixes and newly implemented properties/values may be additive within V1
  only when existing accepted configurations retain observable semantics.
- Tightening a bound for demonstrated denial-of-service protection requires a
  documented security rationale and compatibility review.
- Any change to parsing acceptance, defaults, substitution stage, merge order,
  identity hash, Feature order/lock meaning, lifecycle timing/retry meaning,
  port binding, Compose ownership, or accepted source trust requires a new
  profile version.
- Updating the upstream specification/schema pin always requires a reviewed ADR
  and compatibility gate; it is never automatic.
