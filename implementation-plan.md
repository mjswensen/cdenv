# cdenv Implementation Plan

**Project:** `cdenv` — Containerized Development Environment
**Website:** `cdenv.sh`
**Implementation:** Rust 2024 edition
**Hosts:** macOS on arm64 (Apple Silicon); Linux on x86_64 and arm64
**Containers:** Local Docker, managed by cdenv’s Dev Container implementation
**Workspace format:** Development Containers (`devcontainer.json`)
**Dev Container compatibility:** Versioned cdenv V1 profile pinned to an upstream specification revision
**Docker integration:** Docker CLI for specification-facing operations; Bollard for cdenv-owned operations
**Compose integration:** Docker Compose V2
**Editor integration:** Standard OpenSSH; no editor-specific implementation

**Issue 68 implementation status:** The opt-in permission commands, staged/bound
host storage, bounded credential parser, versioned multiplexed broker protocol,
verified Docker Exec bridge, private static-agent endpoints, and active/candidate
lease primitives are implemented. The uncached, noninteractive, bounded host Git
lookup adapter is also implemented. Production capability reconciliation and
lifecycle handoff remain pending, as do adapter dispatch, SSH/identity backends,
and managed container helper integration. Runtime workflows below remain the target contract,
not evidence that saving a grant enables container authentication.
See [ADR 0002](docs/adr/0002-opt-in-host-capabilities.md).

---

## 1. Product Summary

`cdenv` is a CLI-only tool for creating, running, rebuilding, listing, and connecting to local Docker-backed development containers.

```bash
cdenv create https://github.com/example/project.git
cdenv list
cdenv up project
ssh project.cdenv
```

Multiple independent workspaces may use the same repository:

```bash
cdenv create https://github.com/example/project.git --name project-feature-a
cdenv create https://github.com/example/project.git --name project-feature-b
```

Each workspace owns:

- a Git checkout under `~/.cdenv/workspaces/<name>/checkout/<name>/`;
- metadata and bounded logs under `~/.cdenv/workspaces/<name>/`;
- a cdenv-created Dev Container environment and its primary development container;
- an injected static Rust SSH agent;
- a stable SSH hostname, `<name>.cdenv`;
- a stable SSH host key.

The container does not run OpenSSH `sshd` and does not publish port 22. OpenSSH starts a local ProxyCommand, which creates a Docker Exec process through Bollard and runs:

```text
cdenv-agent ssh-server --stdio
```

SSH protocol bytes flow through stdin/stdout. Each SSH connection has one host proxy process and one container agent process. There is no installation-wide daemon or permanent container SSH daemon. A workspace may have a scoped host supervisor while configuration-declared ports or a credential lease are active, and a temporary container lifecycle runner while background lifecycle work remains. Credential state is independent from listener state, so an authenticated static-agent bridge can remain available in zero-port/zero-SSH-client workspaces.

---

## 2. Goals

### 2.1 Primary goals

1. Provide a simple repository-first CLI for local Docker development containers.
2. Keep cdenv-managed host files under one configurable root, defaulting to `~/.cdenv`.
3. Implement the documented `cdenv-devcontainer-v1` compatibility profile without a Node.js or `@devcontainers/cli` runtime dependency.
4. Use the Docker CLI for specification-facing pull/build/create operations and Compose V2 for orchestration.
5. Use Bollard for cdenv-owned Docker discovery, verification, upload, status, stop/start, and exec operations.
6. Expose workspaces through standard OpenSSH behavior without `sshd` or published SSH ports.
7. Preserve the host checkout across stop, start, and rebuild.
8. Support configuration-declared published and forwarded ports plus ad-hoc forwarding.
9. Support concurrent SSH clients, multiplexed channels, PTYs, commands, signals, and local TCP forwarding.
10. Install one host binary containing Linux x86_64 and arm64 static agent artifacts.
11. Produce actionable diagnostics without ever corrupting the SSH byte stream.
12. Keep modules and implementation chunks small enough to test independently without pre-inventing abstractions.

### 2.2 Declared and ad-hoc port forwarding

`appPort`, `forwardPorts`, and applicable `portsAttributes` are part of the supported Dev Container profile. Published `appPort` mappings are fixed when the container is created. Declared `forwardPorts` mappings are maintained by a scoped host forwarding supervisor from successful `up` until explicit `down`.

A user can additionally expose a container service without changing `devcontainer.json` or rebuilding:

```bash
cdenv forward project 8080:3000
cdenv forward project 8080:3000 --bind 0.0.0.0
cdenv forward project 8080:3000 5432:5432
```

The command is foreground and session-scoped. It delegates to system OpenSSH local forwarding and ends on Ctrl+C. Binding to a non-loopback address is explicit and produces a security warning.

### 2.3 Compatibility promise

V1 promises the Dev Container behavior explicitly listed by `cdenv-devcontainer-v1` and verified by the in-repository profile suite, plus the SSH behavior verified by the OpenSSH compatibility suite. It does not promise every upstream property or undocumented reference-CLI extension. Named editors are not release gates. Documentation may record editor versions observed to work, but cdenv contains no editor-specific behavior.

---

## 3. Non-goals for V1

- remote Docker daemons, repository synchronization, or remote workspace placement;
- Kubernetes, cloud providers, Windows hosts, or Windows containers;
- automatic editor launching;
- full compatibility with every current or future Dev Container property or undocumented `@devcontainers/cli` behavior;
- Dev Container template, prebuild, publish, or configuration-generation commands;
- legacy Docker Compose V1 or reimplementation of the Compose specification;
- an installation-wide always-on daemon or permanent container SSH daemon;
- automatic self-update;
- background management of ad-hoc user-requested forwards beyond configuration-declared `forwardPorts`;
- SFTP, OpenSSH session-scoped agent forwarding, reverse forwarding, or arbitrary Unix-socket forwarding (issue 68's explicit workspace-scoped agent capability is separate);
- destructive workspace/repository deletion commands;
- Git pull, branch, reset, clean, stash, or general credential-store management; issue 68 permits only explicitly granted lookup-only Git credentials, a selected SSH agent, and separate author identity defaults;
- snapshots or background synchronization;
- automatic process-based port discovery or an embedded browser/preview UI;
- guaranteed editor-server support in every image, especially minimal/musl images;
- shell completions unless added after core V1 work is complete.

Remote Docker is not being pre-designed. Local transport assumptions must remain inside the Docker CLI/Compose/Bollard adapters, but remote support is not expected to be “just another adapter”: repository placement and Dev Container execution would also need design.

---

## 4. Required Host Dependencies

The host must provide:

- a reachable local Docker Engine or Docker Desktop daemon;
- a compatible Docker CLI;
- the Docker Compose V2 plugin for Compose configurations;
- Git;
- an OpenSSH-compatible `ssh` client.

`cdenv` invokes the Docker CLI for specification-facing image pull, build, and container creation operations. It invokes Compose V2 for Compose orchestration. Bollard remains the adapter for cdenv-owned discovery, verification, upload, status, stop/start, and Exec operations. All adapters target the same resolved daemon.

Commands must perform only the dependency checks they need. For example, `list` remains useful when Docker is unavailable. `create` performs generic Git/Docker/agent preflight before clone and configuration-specific Docker/Compose/profile preflight immediately after discovery, before image work.

### 4.1 Docker endpoint consistency

Resolve one local Unix-domain Docker socket at startup:

1. accept a standard `DOCKER_HOST` only when it is a Unix socket;
2. support known Docker Desktop, rootless, and `/var/run/docker.sock` local paths;
3. reject TCP, SSH, and remote context endpoints in V1;
4. construct Bollard from the resolved socket;
5. pass the same socket to Docker and Compose subprocesses as `DOCKER_HOST=unix://...`.

This prevents Docker CLI, Compose, and Bollard from accidentally targeting different daemons.

### 4.2 External and profile version support

The feasibility spike determines minimum supported Docker Engine, Docker CLI, and Compose V2 versions from verified behavior. V1 must:

- reject older external versions with an actionable message;
- accept newer versions unless a real capability check fails;
- record detected versions in operation logs and `doctor` output.

Pin the exact upstream Dev Container specification commit selected during Chunk 0 and expose it as `cdenv-devcontainer-v1`. Do not download schemas or silently track upstream at runtime. Additive support may remain within V1; observable semantic changes require a new profile version. Required CI uses the declared minimum and repository-pinned Docker/Compose versions. A scheduled upstream/latest compatibility job is not a V1 requirement.

---

## 5. Feasibility-First Phase

Before production architecture work, complete a disposable vertical spike proving the highest-risk assumptions.

### 5.1 Required packet flow

```text
OpenSSH
  → ProxyCommand
  → Bollard Docker Exec attach
  → cdenv-agent / Russh over stdio
  → command, multiplexed channels, PTY, and direct-tcpip
```

### 5.2 Spike fixtures

Prove at least:

1. an image-based configuration;
2. a Dockerfile configuration with a public OCI Feature and lifecycle commands;
3. a two-service Compose V2 configuration with a primary service, dependency, and declared forwarding;
4. image metadata merge and local/HTTPS Feature fixtures.

For Compose, prove that two cdenv workspace identities result in isolated projects and that complete managed service sets stop without deleting named volumes.

### 5.3 Spike success criteria

- the pinned Dev Container schema and merge rules produce deterministic effective plans;
- Docker CLI-created image- and Compose-backed primary containers receive stable cdenv labels;
- Dockerfile context, BuildKit, `build.options`, and reserved-option validation are feasible;
- public OCI anonymous bearer negotiation, digest verification, and lockfile generation work without Node.js;
- generated Feature/UID Dockerfiles work on supported architectures and Debian/Alpine fixtures;
- Compose V2 override generation, managed service discovery, and project isolation are deterministic;
- lifecycle checkpointing, `waitFor`, cancellation, and detached background execution are recoverable;
- a declared forward remains available after `up` exits and is removed by `down`;
- Bollard Exec supports binary-clean, bidirectional, cancellation-aware stdio;
- Docker stdout/stderr framing never reaches SSH stdout;
- Russh can serve a connection over a generic stdio stream;
- system OpenSSH completes authentication and command execution;
- an OpenSSH `ControlMaster` can multiplex concurrent sessions;
- a PTY shell, resize, Ctrl+C, and `direct-tcpip` work sufficiently to validate the approach;
- disconnect cleanup is feasible without leaving a permanent container SSH daemon.

### 5.4 Spike outputs

Spike code is disposable. Retain:

- an ADR recording the pinned specification commit and exact Docker/Compose, Bollard, and Russh versions/APIs;
- the V1 property support matrix and deliberate interpretations;
- verified command lines, effective-plan snapshots, and JSON fixtures;
- Feature, Compose-label, lifecycle, drift, and isolation findings;
- packet-flow, cancellation, forwarding-supervisor, and multiplexing findings;
- reusable fixture definitions;
- black-box tests that can be migrated without carrying spike abstractions forward.

Do not promote an abstraction merely because it exists in the spike.

---

## 6. Rust Workspace and Engineering Standards

### 6.1 Workspace layout

```text
cdenv/
├── Cargo.toml
├── Cargo.lock
├── rust-toolchain.toml
├── README.md
├── LICENSE
├── crates/
│   ├── cdenv-core/
│   ├── cdenv-devcontainer/
│   ├── cdenv-cli/
│   └── cdenv-agent/
├── xtask/
├── tests/
│   ├── integration/        # non-published workspace package
│   └── fixtures/
├── docs/
│   └── adr/
└── .github/workflows/
```

The internal module names in each crate are illustrative, not prescribed. Start with cohesive modules around proven boundaries and split only when names clarify responsibilities or responsibilities genuinely diverge.

### 6.2 Crate responsibilities

#### `cdenv-core`

Platform-neutral domain types and serialization:

- validated workspace names and hosts;
- installation/workspace state schemas;
- status dimensions;
- architecture and protocol identifiers;
- domain-level validation errors.

It must not depend on Bollard, Russh, Tokio CLI presentation, or host filesystem orchestration.

#### `cdenv-devcontainer`

Pure, independently testable Dev Container profile logic:

- pinned raw and effective configuration models;
- discovery, JSONC parsing, validation, and variable substitution;
- image/Feature metadata merge rules;
- mount, port, Feature, lockfile, and lifecycle models;
- deterministic Feature dependency ordering;
- immutable build, create, runtime, and lifecycle plans;
- profile identifiers, capability reporting, and typed errors.

It must not invoke Docker, Compose, HTTP, credential helpers, or host subprocesses. Keep I/O adapters in `cdenv-cli`; introduce narrow native-async traits only at proven test boundaries.

#### `cdenv-cli`

Host application library and `cdenv` binary:

- CLI parsing and exit rendering;
- local state, locking, Git, and paths;
- Docker CLI, Compose V2, Bollard, Feature-source, and forwarding-supervisor adapters;
- lifecycle and Dev Container plan orchestration;
- agent installation;
- SSH identity/configuration;
- proxy transport and system-SSH wrappers;
- human and JSON output.

`src/lib.rs` contains testable application behavior. `src/main.rs` is limited to parsing, runtime setup, invoking the library, and rendering an exit status.

#### `cdenv-agent`

Agent library and Linux-targeted binary:

- `version`, `identity`, `provision`, environment probe/capture, lifecycle runner, forwarding bridge, and `ssh-server` commands;
- restricted container-side lifecycle checkpoint/log management;
- Russh server behavior;
- process, PTY, signal, and forwarding support;
- strict stdout discipline.

`src/lib.rs` contains testable protocol/process behavior. `src/main.rs` only parses and starts commands.

The crate remains compilable on macOS. Linux-only PTY/process behavior is behind `cfg(target_os = "linux")`; unsupported platforms return a typed error. Distribution accepts only Linux musl targets.

#### `xtask`

- builds both static agent targets;
- generates one shared build ID;
- stages/embeds agent artifacts;
- builds host release artifacts;
- runs integration/compatibility suites;
- verifies release contents and checksums.

### 6.3 Toolchain

- Rust 2024 edition;
- pin the current stable toolchain selected when implementation starts;
- set the same workspace `rust-version`;
- upgrade deliberately with dependency/API verification.

### 6.4 Linting and documentation from Chunk 1

Required checks:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps
cargo deny check
```

Workspace policy:

- deny unsafe code in `cdenv-core` and `cdenv-cli`;
- deny unsafe by default in `cdenv-agent`;
- deny broken intra-doc links;
- require documentation for intentionally public library APIs;
- enable appropriate Rust future-incompatibility and Clippy correctness/performance lints;
- use narrowly scoped, justified lint expectations instead of broad suppressions;
- no untracked `TODO` comments; reference an issue when necessary.

### 6.5 Unsafe-code boundary

Unsafe code is allowed only when required for Linux PTY/session setup:

- isolate it in one private Linux OS/PTY module;
- expose safe wrappers with documented invariants;
- add a `// SAFETY:` justification to every unsafe block;
- enable `unsafe_op_in_unsafe_fn`;
- prefer safe `nix`/standard-library APIs;
- never add unsafe code merely for optimization.

### 6.6 Rust API practices

- borrow `&str`, `&Path`, and slices when ownership is unnecessary;
- use small `Copy` enums/identifiers by value where appropriate;
- avoid redundant clones and intermediate collections;
- use iterators for transformations and clear loops for side-effectful/cancellable flows;
- use native async trait methods/static dispatch at test seams;
- do not introduce `async_trait`, `Arc<dyn Trait>`, or boxed dispatch without a demonstrated runtime need;
- keep traits narrow and introduce them only at proven adapter/test boundaries;
- avoid compile-time typestate where persisted/runtime enums are clearer;
- never use `unwrap()` or `expect()` in production paths;
- measure before optimizing and benchmark release builds only.

### 6.7 Dependency policy

- centralize versions in `[workspace.dependencies]`;
- commit `Cargo.lock` and use `--locked` in CI/releases;
- exact-pin volatile integration crates such as Bollard and Russh after the spike;
- use compatible requirements for stable dependencies;
- enable only required features; do not use Tokio’s `full` feature by default;
- use standard-library file locking rather than `fs2`;
- use an audited JSONC parser rather than ad-hoc comment stripping;
- use a maintained HTTP/TLS client for public Feature downloads with default certificate verification;
- use `thiserror` for library/module errors;
- reserve `anyhow` for binary boundaries and test helpers;
- run `cargo-deny` for advisories, licenses, sources, and explicit bans.

---

## 7. Paths, Installation Identity, and Permissions

### 7.1 Root resolution

Precedence:

```text
--root <absolute-path> > CDENV_HOME > ~/.cdenv
```

`CDENV_HOME` is a documented advanced override, not a hidden test hook. Resolve the root once into a validated `CdenvRoot` and pass it through the application.

Required cdenv, executable, checkout, selected config, and local-source paths must be representable as valid UTF-8. Paths embedded in OpenSSH configuration must also reject unrepresentable control/token cases.

### 7.2 Host layout

```text
~/.cdenv/
├── installation.json
├── fingerprint.key
├── workspaces/
│   └── project/
│       ├── checkout/
│       │   └── project/       # Git checkout / local workspace folder
│       ├── state.json
│       ├── .lock
│       ├── runtime/            # private supervisor control socket/state
│       └── logs/
├── ssh/
│   ├── config
│   ├── known_hosts
│   ├── id_ed25519
│   ├── id_ed25519.pub
│   └── host_keys/
├── cache/devcontainer/
│   ├── blobs/                  # content-addressed Feature artifacts
│   └── generated/              # bounded generated build material
├── logs/
└── tmp/
```

The checkout basename intentionally equals the workspace name. The effective Dev Container plan computes the authoritative container workspace folder from the pinned profile’s defaulting, metadata, substitution, and scenario rules.

Do not create `config.toml` until a real global setting exists.

### 7.3 Installation record

`installation.json` contains operational installation metadata, including:

- schema version;
- stable random installation ID;
- SSH Include consent: `unknown`, `accepted`, or `declined`.

The installation ID namespaces Docker resources shared by multiple users or roots. `fingerprint.key` is a separate random `0600` secret used only to compute keyed build/create/runtime plan digests. Never persist substituted secret values or unkeyed digests of effective plans. If the key is lost or replaced, mark fingerprints unknown and recalculate them only during a successful mutating flow.

### 7.4 Permissions and symlinks

For exclusively cdenv-managed paths:

```text
root, SSH, runtime directories  0700
private keys                    0600
state, fingerprint key, logs    0600
private runtime files           0600
public keys                     0644
```

Automatically tighten these managed permissions. Refuse symlinks and ownership mismatches rather than following or rewriting them. Do not change permissions inside the Git checkout. For a symlinked user-owned `~/.ssh/config`, refuse automatic editing and show manual instructions.

---

## 8. Domain Types and Workspace Naming

Use validated newtypes at important boundaries without wrapping every string:

- `WorkspaceName`;
- `WorkspaceHost`;
- `CdenvRoot`;
- `WorkspaceRoot` / `RepositoryPath`;
- `ContainerId`;
- `InstallationId`;
- `AgentBuildId`;
- `ProtocolVersion`.

A workspace name:

- contains lowercase ASCII letters, digits, and hyphens only;
- begins and ends with a letter or digit;
- is at most 63 characters;
- is safe as a directory component, Docker label value, and SSH hostname label;
- is unique within the configured cdenv root.

Default derivation supports HTTPS, SSH, SCP-like Git URLs, `file://` URLs, and local Git paths:

1. take the final repository path component;
2. strip trailing `.git`;
3. lowercase;
4. replace invalid runs with `-`;
5. trim leading/trailing hyphens;
6. validate;
7. fail with `--name` guidance when empty, too long, or already present.

Never silently add a numeric suffix.

Local Git paths are a V1 feature. Canonicalize their stored source description while still delegating clone behavior to Git.

---

## 9. State Model, Atomicity, and Locking

### 9.1 State is not live truth

Docker determines live container state. State records user intent, operation recovery, and the last successfully provisioned container.

Do not persist derivable `repositoryDirectory` or a free-form `statusHint`.

Suggested shape:

```json
{
  "schemaVersion": 1,
  "installationId": "...",
  "name": "project",
  "repositorySource": "https://github.com/example/project.git",
  "devcontainerProfile": "cdenv-devcontainer-v1",
  "desiredDevcontainerConfig": ".devcontainer/devcontainer.json",
  "desiredFingerprints": {
    "build": "keyed:...",
    "create": "keyed:...",
    "runtime": "keyed:..."
  },
  "createdAt": "...",
  "lastUpAt": "...",
  "operation": {
    "kind": "idle",
    "id": null,
    "startedAt": null
  },
  "lastError": null,
  "active": {
    "generation": 3,
    "scenario": "compose",
    "containerId": "...",
    "imageId": "...",
    "composeProject": "...",
    "managedServices": ["app", "db"],
    "buildFingerprint": "keyed:...",
    "createFingerprint": "keyed:...",
    "runtimeFingerprint": "keyed:...",
    "featureDigests": {},
    "lifecycle": {
      "completedThrough": "updateContentCommand",
      "running": "postCreateCommand",
      "indeterminate": false
    },
    "forwarding": {
      "supervisorBuildId": "...",
      "requested": [],
      "assigned": []
    },
    "provisioned": {
      "remoteUser": "vscode",
      "remoteWorkspaceFolder": "/workspaces/project",
      "containerArchitecture": "aarch64",
      "agentPath": "/usr/local/libexec/cdenv/cdenv-agent",
      "agentBuildId": "...",
      "protocolVersion": 1,
      "environmentPath": "..."
    }
  }
}
```

The exact serde representation may change during Chunks 1–2, but it must preserve these distinctions.

### 9.2 Status dimensions

Model independently:

- **environment:** running, stopped, partially running, missing, ambiguous, Docker unavailable;
- **operation:** idle, creating, starting, rebuilding, stopping, locking;
- **configuration:** valid/current, runtime drift applied, build/create drift, invalid, profile unsupported;
- **lifecycle:** complete, running in background, failed, indeterminate;
- **forwarding:** active, degraded, missing supervisor, target unavailable, not configured;
- **local health:** valid, interrupted, last operation failed, corrupt, provision drift.

Human output derives a concise display; JSON retains all dimensions and requested-versus-assigned forwarding endpoints. A running environment, build drift, background lifecycle stage, and forwarding failure can all be true.

### 9.3 Schema compatibility

- reject newer unsupported schemas without modification;
- migrate older schemas through explicit tested steps in memory;
- persist migration only during a mutating command;
- never silently replace corrupt state with defaults.

### 9.4 Atomic writes

For managed state/config updates:

1. create a unique temporary file in the same directory with `create_new`;
2. set final permissions before exposing content;
3. write and flush;
4. sync the file;
5. rename atomically;
6. sync the parent directory on Unix where supported.

### 9.5 Locking

Use `std::fs::File` shared/exclusive locking behind a small lock module.

- reserve workspace names atomically under a brief global namespace lock;
- use `workspaces/<name>/.lock` for create/up/down/rebuild/lock;
- mutating commands hold the exclusive lock for the full operation;
- read-only commands do not rewrite stale state;
- proxy takes a shared lock only through resolve/inspect/Exec attach, then releases it;
- proxy fails quickly when a lifecycle operation holds the exclusive lock;
- existing SSH sessions never hold lifecycle locks.

A persisted foreground operation is active only while the lock is unavailable. A background lifecycle runner records separate container-side checkpoints and does not retain the host workspace lock after `up` readiness. If state records a foreground operation but the lock is available, report an interrupted previous operation. Recover only transitions whose outcome is known; an indeterminate one-time lifecycle command requires rebuild.

---

## 10. CLI Contract

```text
cdenv [--root <path>] [--modify-ssh-config | --no-modify-ssh-config] <command>
```

Commands:

```text
create
list
up
down
rebuild
status
ssh
forward
lock
proxy
doctor
credentials
```

### 10.1 `create`

```bash
cdenv create <git-source> [--name <name>] [--config <repo-relative-devcontainer.json>]
```

Behavior:

1. preflight required dependencies and embedded agent availability;
2. derive/validate and atomically reserve the name;
3. create initial state with operation `creating`;
4. clone with system Git into `checkout/<name>/`;
5. preserve a sanitized source description in state;
6. validate an explicit config path inside the checkout;
7. call the same internal orchestration as `up`;
8. install/provision the agent and SSH assets;
9. generate cdenv SSH files and apply remembered Include consent;
10. print workspace and connection details.

Providing a repository source is sufficient trust consent; do not add a repository-code confirmation prompt. Document that Dev Container configuration executes repository-defined code.

Failure transaction:

- clone failure: remove the incomplete workspace, retain a sanitized operation log under the global logs directory;
- failure after clone: retain checkout and state, record the error, and allow plain `up` to retry only when the recorded stage is safe; failed/indeterminate one-time lifecycle work requires rebuild;
- cancellation during clone acts like clone failure;
- cancellation after clone retains the workspace.

The Git subprocess receives the source as an argument after `--`; do not construct a shell command. Preserve credential prompts where interactive. Do not log the raw source argument.

### 10.2 Dev Container config selection

Explicit paths are repository-relative, must exist, must be valid UTF-8, and must not escape the canonical checkout through `..` or symlinks.

```bash
cdenv up project --config .devcontainer/alternate/devcontainer.json
cdenv rebuild project --config .devcontainer/alternate/devcontainer.json
```

An explicit selection is persisted as desired intent before lifecycle execution. The last successfully provisioned container record is updated only after complete success. A plain retry uses the desired config.

Without `--config`, use pinned specification precedence:

1. `.devcontainer/devcontainer.json`;
2. `.devcontainer.json`;
3. `.devcontainer/<folder>/devcontainer.json`.

The first existing higher-precedence path wins. If only the third form exists and multiple folders qualify, fail with `--config` guidance. Do not prompt or select arbitrarily.

### 10.3 `list`

```bash
cdenv list [--json]
```

- enumerate local workspace directories;
- read states without mutating them;
- query Docker once with both installation/workspace label filters and `all=true`;
- correlate in memory, avoiding one Docker call per workspace;
- sort deterministically by name;
- remain successful when local enumeration works but Docker is unavailable, representing the failure in status/warnings.

### 10.4 `up`

```bash
cdenv up <name> [--config <path>]
```

Parse and validate desired configuration, compute category fingerprints, and reconcile without implicit rebuild:

- create a missing environment from the effective plan;
- start an unchanged stopped image-based container through Bollard;
- start recorded existing Compose containers without Compose reconciliation when build/create drift exists;
- apply independently valid runtime drift, including declared forwarding, `remoteEnv`, and attach behavior;
- warn and continue using the active generation when build/create drift is detected;
- fail before mutation when desired configuration is invalid or unsupported;
- verify an existing background lifecycle runner instead of starting duplicate commands.

After the selected `waitFor` stage, reinstall/provision the expected agent, capture the effective environment, transactionally establish declared forwards, update state atomically, and regenerate SSH material. Later lifecycle stages may continue in the background. `up` is idempotent but always reverifies/reprovisions cdenv-owned assets. A prior failed or indeterminate one-time lifecycle stage requires rebuild.

### 10.5 `down`

```bash
cdenv down <name>
```

Stop the complete cdenv-managed environment without deleting the checkout or named volumes:

1. stop and remove the declared forwarding supervisor;
2. cancel active background lifecycle work with bounded graceful/forced cleanup;
3. for image/Dockerfile scenarios, stop the primary container through Bollard;
4. for Compose, stop the persisted managed service set—configured `runServices` or all services by default, plus dependencies cdenv started—through Compose V2.

Do not stop unrelated services manually started later in the isolated Compose project. `shutdownAction` does not disable an explicit `cdenv down`; V1 has no editor-close event that triggers automatic shutdown.

### 10.6 `rebuild`

```bash
cdenv rebuild <name> [--config <path>] [--no-cache]
```

Recompute the complete desired plan, require a valid/fresh Feature lockfile when one exists, and build replacement images before stopping the active environment. `--no-cache` maps to Docker/Compose no-cache build behavior; cached rebuild is the default.

For image/Dockerfile scenarios, rename the old container to a private temporary backup, create/start/verify the replacement, and restore the old container on failure where feasible. Remove the backup only after success. Compose rebuild uses build-first then Compose force-recreate/reconciliation; document that project-wide replacement is not atomic. Preserve named volumes, remove orphaned project containers only after desired services are healthy, and classify partial replacement as interrupted/drifted.

Before rebuilding, use `git status --porcelain` only to print whether uncommitted changes exist. Do not block or expose filenames. Require a replacement primary container ID when a prior one existed, increment the generation, and reprovision all agent/SSH/environment/forwarding assets.

After successful replacement, remove operation-owned backup/orphan containers and only unreferenced images explicitly labeled as cdenv-generated for this workspace, retaining a small bounded history window. Never prune base images, repository-tagged images, named volumes, or unrelated BuildKit cache. Cleanup failure is a warning after an otherwise successful rebuild.

### 10.7 `status`

```bash
cdenv status <name> [--json]
```

Show local source/path, selected config, pinned profile, current Git branch when available, all status dimensions, desired-versus-active build/create/runtime fingerprints, scenario and managed Compose services, lifecycle stage, requested/assigned ports, forwarding-supervisor health, container ID/architecture, remote user/folder, Feature digests, and provisioned agent build/protocol.

Exit nonzero when the requested workspace is missing, corrupt, ambiguous, lifecycle-failed/indeterminate, required-forwarding degraded, or cannot be queried as requested. Build/create drift alone is a warning. Under `--json`, still emit a valid error envelope.

### 10.8 `ssh`

```bash
cdenv ssh <name> [-- <remote command and arguments...>]
```

Delegate to system OpenSSH using the generated config explicitly:

```text
ssh -F <absolute-cdenv-ssh-config> <name>.cdenv ...
```

This command works even when the user declines to modify `~/.ssh/config`. Propagate the OpenSSH exit status.

### 10.9 `forward`

```bash
cdenv forward <name> <local-port:container-port>... [--bind <address>]
```

- require one or more nonzero `u16` port pairs;
- default bind address to `127.0.0.1`;
- default remote target host to container `localhost`;
- warn clearly for non-loopback binds;
- preflight every requested listener and start none if any conflicts with declared or ad-hoc mappings;
- fail with the existing requested/assigned declared mapping when a supervisor owns an endpoint;
- run foreground until interrupted;
- delegate to:

```text
ssh -F <config> -N -o ExitOnForwardFailure=yes \
    -L <bind>:<local-port>:localhost:<container-port> ... <name>.cdenv
```

Use process arguments, never a shell command. Propagate the OpenSSH exit status. A forwarding-only SSH transport executes `postAttachCommand` once, just like another new SSH transport.

### 10.10 `lock`

```bash
cdenv lock <name> [--config <repo-relative-devcontainer.json>]
```

Resolve public OCI, unauthenticated HTTPS, and local Feature dependencies; compute deterministic ordering; and atomically create or update the selected configuration’s adjacent `devcontainer-lock.json`. This explicit command is the sole cdenv-owned exception to post-clone checkout immutability. It never stages or commits the file.

`--config` selects only the lockfile target and does not alter desired workspace intent. Without it, use desired selection or normal discovery. Canonicalize the adjacent target inside the checkout, refuse an existing symlink/non-regular file, preserve a reasonable existing mode, and atomically replace it through a same-directory temporary file. Lifecycle commands never generate or rewrite lockfiles.

### 10.11 `proxy`

```bash
cdenv --root <absolute-root> proxy <workspace-name-or-host>
```

Internal requirements:

1. normalize and validate the workspace;
2. briefly coordinate with lifecycle locking;
3. resolve exactly one acceptable running labeled container;
4. require live container ID and provisioned build/protocol to match state;
5. verify the container is running;
6. start attached Docker Exec as the recorded remote user and workspace folder;
7. run the recorded agent path with SSH config/assets;
8. bridge stdin, decoded stdout, and stderr with bounded backpressure;
9. propagate EOF/cancellation and inspect final Exec status;
10. emit only SSH protocol bytes on stdout.

Before starting the SSH server, serialize and execute `postAttachCommand` once for this transport. Closed/background stdin rules apply, output goes only to restricted logs/stderr, and failure rejects this transport. A later transport may retry; successful execution clears attach-specific degraded status. After an external container replacement or host-agent upgrade, fail with a concise instruction to run `cdenv up <name>`. Proxy never provisions or repairs lifecycle/container state beyond the specification-required attach hook.

### 10.12 `doctor`

Read-only checks:

- cdenv root ownership/permissions and schema;
- installation ID and SSH consent;
- pinned Dev Container profile and vendored schema identity;
- Git, OpenSSH, Docker Engine/CLI, and Compose V2 versions;
- local Docker socket and Bollard connectivity;
- generated SSH config syntax and Include visibility;
- client/host key validity and permissions;
- embedded agent artifacts/build IDs for both architectures;
- duplicate/stale labeled containers and Compose projects;
- generated image/cache retention and Feature-lock integrity;
- lifecycle runner, forwarding supervisor, assigned listener, configuration drift, and provision drift;
- stale operation state.

`doctor`, `status`, `list`, and proxy report but never repair a missing supervisor. Only `up` repairs cdenv-managed runtime services.

Exit nonzero when a required invariant fails. V1 `doctor` diagnoses but does not repair.

---

### 10.13 `credentials`

```text
cdenv credentials enable NAME git-https [--host HTTPS_ORIGIN ...]
cdenv credentials enable NAME ssh-agent [--socket auto|ABSOLUTE_HOST_SOCKET]
cdenv credentials enable NAME git-identity
cdenv credentials allow NAME git-https HTTPS_ORIGIN ...
cdenv credentials deny NAME git-https HTTPS_ORIGIN ...
cdenv credentials disable NAME [CAPABILITY ...]
cdenv credentials status NAME [--json]
```

This host-only permission group is independent of repository profile parsing.
Permissions are off by default; unknown capabilities and schemas fail closed.
An initial staged HTTPS enable needs explicit origins. Explicit-name clone binds
staged permission to the installation/root and exact workspace receipt, without
checkout writes. Failed clones preserve staged permission, replacement identities
cannot inherit old grants, and disable discards revoked authority.

Permission schema 1 lives in `<root>/credentials/NAME.json`, with a private
workspace binding receipt outside the checkout. Existing unsafe permission
paths/modes are rejected rather than automatically tightened. Status/list/doctor
inspect the same staged/bound/stale facts without helper or login operations.

The permission/parser foundation, versioned broker transport, host Git and
selected-agent backends, reversible container integrations, supervisor ownership,
and managed lifecycle/SSH enrollment are implemented. Create/up establishes
credential readiness before the first hook; down retains grants, rebuild changes
generation authority, and active-generation mutations reconcile a monotonic
revision without rerunning hooks. Disable/deny persist revocation before requiring
an authenticated acknowledgement or proof that the exact supervisor stopped;
they never signal an unverified PID. Issue 78 owns the remaining controlled
HTTPS/SSH, cross-platform, lifecycle, secret-surface, and macOS release evidence.
See [ADR 0002](docs/adr/0002-opt-in-host-capabilities.md) for implemented limits,
DevPod comparison, and explicit evidence blockers.

---

## 11. Dev Container Compatibility Profile

### 11.1 Versioned contract and lifecycle authority

`cdenv-devcontainer-v1` is a documented compatibility profile pinned to the exact upstream specification commit recorded in the Chunk 0 ADR. The vendored schema, profile support matrix, merge rules, and cdenv tests—not an installed CLI or a dynamically downloaded schema—define runtime behavior.

cdenv is the sole Dev Container lifecycle authority. The official `@devcontainers/cli` is not a runtime, build, or release dependency. Captured fixtures and optional differential tests may identify incompatibilities, but undocumented reference-CLI behavior does not silently redefine the profile.

The profile implementation follows a deterministic pipeline:

```text
discover configuration
  → parse bounded JSONC
  → validate scenario/profile
  → perform host/workspace-stage substitutions
  → resolve/build base image
  → inspect and merge image metadata
  → resolve Features and lockfile
  → perform container/runtime-stage substitutions
  → produce immutable build/create/runtime/lifecycle plans
  → execute through Docker/Compose/Bollard adapters
```

Keep parsing, merging, validation, dependency ordering, and plan generation pure in `cdenv-devcontainer`. I/O adapters consume typed plans; command modules never interpret raw Docker/Compose output directly.

### 11.2 V1 support matrix

V1 supports:

- standard discovery and explicit config selection;
- comments in JSONC, with trailing commas rejected by the pinned schema;
- image, Dockerfile, and Docker Compose V2 scenarios;
- image `devcontainer.metadata` and Feature metadata merge rules;
- specified local/container/workspace/`${devcontainerId}` substitutions and defaults;
- workspace mounts/folders, additional mounts, container/remote users, UID/GID update, and user environment probing;
- `containerEnv`, `remoteEnv`, `overrideCommand`, `init`, `privileged`, capabilities, security options, `runArgs`, and supported build properties/options;
- `appPort`, `forwardPorts`, `portsAttributes`, and `otherPortsAttributes` as qualified below;
- public OCI, unauthenticated HTTPS, and contained local Features;
- Feature options, recursive `dependsOn`, `installsAfter`, overrides, lifecycle contributions, and lockfiles;
- all lifecycle command forms, ordering, `waitFor`, and attach behavior;
- Compose `service`, `runServices`, workspace folder, and managed environment stop/resume;
- measurable host CPU, memory, storage, and GPU requirements.

Profile interpretations and limits:

- arbitrary process-based listening-port discovery is deferred;
- `otherPortsAttributes` and regex/range attributes that require discovery are validated and reported but have no automatic V1 trigger;
- `openBrowser`/`openBrowserOnce` print the resolved URL; `openPreview` explains that cdenv has no embedded preview; all produce structured warnings rather than launching UI;
- `shutdownAction` is merged/displayed, but V1 has no editor-close event; explicit `down` always stops the managed environment;
- `customizations.<tool>` and advisory `secrets` metadata are accepted but not interpreted for tools cdenv does not implement;
- deprecated Feature distribution forms, private Feature sources, insecure HTTP registries, custom TLS bypasses, legacy Compose V1, and non-Docker orchestrators are rejected.

Reject unknown top-level behavioral properties and known unsupported behavioral values with exact property paths and profile revision. Accept `$schema` and tool-namespaced customization objects. Never silently ignore configuration that could change the resulting environment.

### 11.3 Parsing, paths, metadata, and substitutions

Use an audited JSONC parser with bounded file size, depth, strings, arrays, lifecycle command groups, and diagnostics that retain source spans. Follow specification discovery precedence from Section 10.2. Explicit and discovered paths must remain symlink-contained in the canonical checkout.

Implement the specification’s property-specific image-metadata merge table; do not use generic recursive JSON merging. Inspect the base/final image labels at the required planning stages. When order matters, repository configuration is last. Validate merged output before executing it.

Substitute values only in properties allowed by the pinned specification and at the stage when required inputs exist. `${containerEnv:...}` uses the actual active container environment for runtime-only reconciliation. Compute `${devcontainerId}` from stable cdenv identity labels using the specification’s canonical sorted-label hash so it is unique on the daemon and stable across rebuilds.

Configuration produces separate keyed build, create, runtime, and lifecycle plans. Never persist resolved `localEnv`, build-argument, container-environment, remote-environment, or secret values.

### 11.4 Docker and build options

The Docker CLI executes specification-facing pull/build/create plans so Docker owns registry image authentication, BuildKit, Dockerfile parsing, build context semantics, and supported Docker-shaped options. Invoke arguments directly without a shell and stream bounded/redacted logs.

Parse `runArgs` and `build.options` only enough to reject options that conflict with cdenv invariants, including:

- cdenv identity labels, required container names, and automatic removal;
- workspace mount targets and cdenv-owned asset locations;
- selected Dockerfile/context and cdenv-owned result image tags/outputs;
- attach/detach/stdin/TTY modes owned by orchestration;
- user overrides that would invalidate the effective container/remote user.

Pass other arguments unchanged. Report the exact conflicting argument; do not silently reorder or override it. Supplying a repository remains consent to valid privileged mode, capabilities, devices, host mounts, and other Docker access requested by its configuration.

Generate Feature, UID/GID, metadata, and temporary Compose build material only under cdenv cache/tmp paths, never in the checkout. Respect `.dockerignore`, path containment, symlink behavior, cancellation, and bounded build-context generation.

Implement `updateRemoteUserUID` only where required by the pinned Linux-container rules. Resolve the effective named user from image/Compose metadata, derive host UID/GID without persisting identity-sensitive environment, and create a derived image layer before container creation. Prefer the architecture-matched static helper and safe filesystem APIs over assumptions about `usermod`, `groupmod`, or distro utilities; preserve root and conflicting-account invariants and fail precisely when a safe update is impossible.

Evaluate `hostRequirements` before destructive mutation. Hard-fail CPU, memory, or required GPU constraints only when reliable evidence proves they are unmet; warn when local Docker/Desktop cannot measure a requirement reliably. Honor optional GPU without failure and never grant GPU access unless configuration requests it.

### 11.5 Features and lockfiles

Supported Feature sources are:

- public OCI registries over verified HTTPS, including anonymous bearer-token challenges;
- unauthenticated HTTPS tarballs;
- specification-contained local Feature directories.

V1 does not read Docker credentials for Features, accept credentials in URLs, or support private registries. Implement the minimum OCI Distribution pull surface in Rust: reference normalization, manifest/media-type negotiation, anonymous token exchange, blob download, size/digest verification, and content-addressed caching. Bound redirects, response sizes, decompression, file counts, and extracted sizes. Reject archive traversal, escaping symlinks, devices, and other unsafe entries.

Validate Feature metadata/options, apply defaults, normalize identifiers, recursively resolve `dependsOn`, and apply `installsAfter` and `overrideFeatureInstallOrder` using the specification’s deterministic round algorithm. Detect cycles and conflicting dependency options with an actionable graph error. Install each ordered Feature as root in its own generated image layer; merge its environment, mounts, capabilities, entrypoints, lifecycle hooks, and metadata exactly once.

Lockfile policy:

- create/rebuild/lock resolve Features; ordinary existing-container `up` does not contact registries merely to detect drift;
- a present lockfile is frozen during create/rebuild and must match configuration, resolved dependencies, versions, digests, and integrity;
- stale/inconsistent locks fail create/rebuild with `cdenv lock` guidance;
- existing-container `up` warns and continues on lock drift;
- a missing lockfile permits resolution without writing one and warns that the build is not fully reproducible;
- verified digest-addressed cache entries may satisfy a frozen lock offline; unlocked tags require online resolution;
- `cdenv lock` is the only command that creates/updates the lockfile.

Digest and TLS checks provide integrity, not publisher identity. V1 has no signature/Sigstore requirement; lock generation is trust on first use.

### 11.6 Lifecycle execution and recovery

Execute lifecycle stages in specification order, with Feature-contributed commands before repository commands:

- `initializeCommand` runs on the host checkout for create/up/rebuild before container mutation and may run more than once;
- `onCreateCommand`, `updateContentCommand`, and `postCreateCommand` run for a new container generation;
- `postStartCommand` runs after an actual successful start, not an idempotent `up` of an already-running environment;
- `postAttachCommand` runs once for each new SSH transport, including forwarding-only transports, but not once per multiplexed channel.

String commands run through the applicable `/bin/sh`; array commands execute directly; object entries execute concurrently and all must succeed. Synchronous string/array forms may inherit interactive stdin. Parallel object forms and background stages receive closed stdin. Prefix multiplexed logs by stable command key without changing the command’s own byte stream.

Honor `waitFor`. For issue 68's credential-enrolled flows, provision/verify the agent, establish the workspace credential bridge, and enroll the effective environment **before the first container lifecycle stage**, not after `waitFor` or at SSH attachment. Production credential-enabled reconciliation enforces this ordering. After the selected stage succeeds, cdenv may establish declared forwarding while later lifecycle stages continue under `cdenv-agent lifecycle-runner`. The runner stores the generation’s immutable effective lifecycle/runtime plan, restricted before/after checkpoints, and bounded logs inside the container and exits when work completes; an intentionally long-running later command remains healthy/running. Later stages never run after failure.

Successful `up` means the selected stage, cdenv provisioning, environment capture, and forwarding listener startup have completed. A later lifecycle failure:

- never terminates existing SSH sessions;
- skips subsequent stages;
- rejects new transports for failed one-time stages;
- appears in list/status/doctor;
- requires rebuild in V1.

A definite `postAttachCommand` failure rejects only that transport; a later transport invokes it again and can clear attach degradation. Serialize attach hooks per container. If cancellation/crash leaves a one-time command indeterminate, plain `up` refuses and rebuild is required.

`down` or rebuild requests graceful lifecycle-runner cancellation, waits a bounded period, then terminates it. A later `up` verifies an existing runner/checkpoints and never duplicates background commands. The command list is immutable for a generation even if `devcontainer.json` changes; files referenced from the live checkout remain live.

### 11.7 Effective remote environment

A plain Docker Exec does not automatically include profile-probed or `remoteEnv` values. After container start, upload the staging agent and:

1. inspect the actual container environment;
2. resolve the effective remote user;
3. run the configured `userEnvProbe` as that user;
4. apply supported `${containerEnv:...}` substitutions and merge `remoteEnv`;
5. use the result for lifecycle and cdenv-injected user processes;
6. recapture after the readiness lifecycle stage for SSH sessions.

Do not print captured values or persist them on the host. Store the snapshot inside the container in a restricted binary-safe format supporting non-UTF-8 Unix values. Remove transient `PWD`, `OLDPWD`, `SHLVL`, `_`, and SSH session entries before SSH reuse.

### 11.8 Desired and active plans

Persist desired selection separately from the active generation. Compare keyed category fingerprints:

- **build:** base image/Dockerfile, build args/options, Features, metadata build, UID/GID update;
- **create:** mounts, container environment/user, published ports, capabilities, security, command, and run arguments;
- **runtime:** remote environment/probe, forwarding, port attributes, and attach behavior.

`up` warns but does not rebuild for build/create drift. It applies independently valid runtime drift to the active container using its actual environment/user/folder. Changed create-time hooks do not run on the old generation. Invalid desired configuration blocks up/rebuild/lock but never blocks proxy, existing SSH, status/list/doctor, or down from persisted active data.

Forwarding-plan updates are transactional: prebind new/changed listeners, retain unchanged assignments, switch only when the new plan is viable, then release obsolete listeners. On required-listener failure, retain the previous complete plan and report desired runtime drift. Rebuild consumes the complete desired plan.

### 11.9 Declared ports and forwarding supervisor

`appPort` uses Docker publication at container creation:

- numeric values bind `127.0.0.1` on the same port;
- string values retain their explicit Docker publication syntax;
- non-loopback string bindings produce a security warning;
- changes are create drift and require user-chosen rebuild to take effect.

`forwardPorts` uses a detached host supervisor scoped to one workspace. It starts during `up`, owns loopback TCP listeners through explicit `down`, and forwards through cdenv agent/Bollard transport from the primary container to `localhost` or the declared Compose service host. Explicit `forwardPorts` wins over `onAutoForward: ignore` because the latter applies to deferred automatic discovery.

For each requested local port:

- `requireLocalPort=false` selects an available loopback port when the same port is occupied;
- `requireLocalPort=true` leaves the environment running but makes `up` nonzero and forwarding degraded;
- `label` and `protocol` affect human/JSON endpoint and URL rendering;
- privileged local ports fail with guidance; `elevateIfNeeded` never causes silent elevation in V1;
- requested and assigned endpoints are exposed separately and assignments are reused for the active generation where available.

The supervisor uses a private control socket/lifetime lock under the workspace runtime directory, validates installation/workspace/generation/build identity, avoids PID-reuse signaling, uses bounded logs/backoff, and retains listeners when the active container is temporarily unavailable. It exits on authenticated `down`; `up` replaces stale/incompatible supervisors. Read-only commands and proxy diagnose but never repair it. After host reboot, the user runs `up` to restore forwarding. There is no launchd/systemd integration or global supervisor.

An ad-hoc `cdenv forward` preflights all mappings and starts none on any conflict. It never silently reuses a declarative listener. No configuration-declared or ad-hoc forward publishes to non-loopback without explicit user input.

### 11.10 Compose V2 behavior

Use only `docker compose` V2, with a minimum version chosen in Chunk 0. Generate deterministic JSON-compatible Compose overrides under cdenv tmp/cache for labels, final Feature image, workspace settings, environment, entrypoints/commands, and other profile enrichment. Always pass explicit files, project name, and Docker endpoint without a shell.

For a Feature/UID-enriched primary service, first ask Compose to pull/build the declared base service image, inspect/tag that exact result, build the generated derived image, then override the primary service to that image and start with build disabled for the final step. Verify that Compose did not substitute a different image.

Derive a stable Docker-safe project name from installation ID and workspace name; truncate with a stable hash. V1 connects/provisions only the configured primary service but manages the environment service set:

- explicit `runServices`, or all configured services by default;
- dependencies Compose starts for those services;
- services cdenv records as part of environment creation.

Do not stop unrelated services manually started later. `down` stops the persisted managed set and preserves containers, project networks, and named volumes. Initial create/rebuild may use Compose reconciliation. With detected build/create drift, ordinary `up` starts recorded existing containers directly rather than allowing Compose to recreate or add services unexpectedly.

Compose rebuild resolves/builds first, then force-recreates/reconciles the isolated project. It is not transactionally atomic. Remove orphaned project containers only after desired services are healthy; never remove named volumes. Record partial replacement precisely.

### 11.11 Stable identity and discovery

Apply stable labels directly through Docker creation plans and Compose overrides:

```text
cdenv.installation=<installation-id>
cdenv.workspace=<workspace-name>
cdenv.generation=<generation>
cdenv.profile=cdenv-devcontainer-v1
```

Verify labels, primary service, project, image, and generation before provisioning. Discovery policy:

- connection/status may accept exactly the recorded running generation and report stale matches;
- unrecorded external replacement is provision drift and never silently adopted;
- multiple current-generation matches are ambiguous and fail;
- mutating flows clean up only operation-owned candidates/backups and otherwise fail with explicit manual Docker cleanup guidance.

### 11.12 Mutation and trust boundaries

Outside initial clone, cdenv never runs Git mutation commands and never writes its own material into the checkout except the exact lockfile targeted by explicit `cdenv lock`. Repository/Feature lifecycle commands and `initializeCommand` may modify the checkout as repository-defined code; this is outside cdenv’s preservation guarantee.

Supplying a repository source is sufficient consent. Do not add per-property prompts for host initialization, Features, privileged containers, host mounts, devices, capabilities, or Docker options. Document that these operations can execute arbitrary code with Docker-daemon-level access.

---

## 12. Docker CLI, Compose, and Bollard Adapters

Keep three explicit adapter boundaries:

- **Docker CLI:** specification-facing image pull, BuildKit build, context/options, and image/container creation;
- **Compose V2:** configuration resolution, build, create/reconcile, managed service start/stop, and primary-service lookup;
- **Bollard:** cdenv-owned daemon ping, bulk discovery, inspect/verify, direct restart/stop of recorded image containers, archive upload, and attached Exec.

Subprocess adapters use argument arrays, one resolved working directory/environment, bounded stdout/stderr capture, streamed restricted operation logs, process-group cancellation, and typed parsed output. They must pass the resolved Unix `DOCKER_HOST`, stable project/name/labels, and reject a CLI result that Bollard verification does not confirm. Never log an interpolated Compose model because it may contain secrets.

Bollard capabilities:

- local Unix-socket connection and ping;
- one-call list/filter by installation/workspace/generation labels;
- container and image inspect, architecture detection, rename, stop/start, and operation-owned cleanup;
- archive upload;
- attached and detached Exec create/start/inspect;
- binary-clean stdin/stdout/stderr streaming;
- cancellation and bounded control/API timeouts.

The Bollard adapter must decode Docker multiplexed frames internally:

```text
local stdin       → Docker Exec stdin
Docker stdout     → local stdout exactly
Docker stderr     → local stderr/log exactly
```

No framing bytes may escape. Do not allocate a Docker TTY; SSH handles channel PTYs.

Lifecycle and build commands have no guessed overall fixed timeout. Individual Docker control/discovery and network calls do. Ctrl+C terminates owned subprocess groups/runners where safe and records definite versus indeterminate interruption. SSH/proxy sessions and intentional background lifecycle commands have no cdenv idle timeout.

Docker/Compose output is advisory until cdenv verifies IDs, labels, image, project, service, state, mounts, users, and generation through Bollard. A Docker restart policy may revive an environment after explicit `down`; cdenv does not install a global policy-enforcement daemon and reports resulting live truth.

Container architecture—not host architecture—selects the agent:

```text
amd64/x86_64 → x86_64-unknown-linux-musl
arm64/aarch64 → aarch64-unknown-linux-musl
```

Reject unsupported architectures clearly.

---

## 13. Agent Build, Upload, and Provisioning

### 13.1 Build pipeline

The release host embeds two static Linux artifacts:

```text
x86_64-unknown-linux-musl
aarch64-unknown-linux-musl
```

Do not recursively invoke Cargo from a build script.

- ordinary `cargo build`/tests may use an injectable or staged agent provider;
- development container operations fail clearly when artifacts are not staged;
- `cargo xtask build`/`dist` is the canonical fully embedded build path;
- release CI rejects absent/empty artifacts and wrong target formats.

The spike determines the practical cross-build tool after proving Russh’s dependency graph. Do not choose one speculatively.

### 13.2 Build identity

`xtask` generates one build ID shared by host and both agents. Agent output:

```json
{
  "name": "cdenv-agent",
  "version": "0.1.0",
  "protocolVersion": 1,
  "buildId": "..."
}
```

### 13.3 Always reinstall on lifecycle success

Every `up` and rebuild atomically uploads and provisions the expected embedded agent, even when an installed agent reports the same ID. Verify `version` afterward. Build/protocol reporting is verification and diagnostics, not authority to skip upload.

### 13.4 Tool-free provisioning flow

Do not require `sh`, `cp`, `install`, `chmod`, `id`, or similar tools in the container.

1. upload a tar archive with a uniquely named staging agent and SSH assets;
2. execute staging `cdenv-agent identity` through Bollard as the Dev Container-selected user;
3. receive effective UID, GID, home, and shell as machine-readable output;
4. execute staging `cdenv-agent provision` as UID 0;
5. use Linux syscalls/Rust filesystem APIs to create directories, atomically install the agent, set ownership/modes, and place assets;
6. execute final `cdenv-agent version` as the remote user;
7. run the staging/final agent’s environment probe/capture path with the effective profile plan;
8. start or verify the scoped lifecycle runner/forwarding bridge when required;
9. persist the successfully provisioned record only after every readiness step succeeds.

Normal SSH server processes always run as the remote user. Root is used only for the short provisioning command.

### 13.5 Installation locations

Try a documented ordered list beginning with a conventional root-owned executable location and falling back to writable executable locations discovered during provisioning. Record the actual path.

Read-only root filesystems and writable locations mounted `noexec` are best-effort, not guaranteed. If no secure executable location exists, fail precisely. Never weaken private-key permissions or write into the checkout.

---

## 14. SSH Identity and Configuration

### 14.1 Keys

Generate with an actively maintained Ed25519 implementation compatible with the pinned Russh version:

- one installation-wide unencrypted client key;
- one persistent host key per workspace.

The client key is intentionally unencrypted for unattended editor/forward workflows and is dedicated to cdenv. The agent accepts only the exact client public key and synthetic SSH username `cdenv`.

Upload the workspace host private key and authorized public key with restricted ownership/modes during every provisioning pass. Stable host keys survive rebuilds.

### 14.2 Generated config uses explicit hosts

Do not use `Host *.cdenv` with `ProxyCommand ... %h`; OpenSSH executes ProxyCommand through a shell after token substitution, creating an injection surface.

Generate one explicit block per valid workspace:

```sshconfig
Host project.cdenv
    HostName project.cdenv
    Port 22
    User cdenv
    ProxyCommand "/stable/path/cdenv" --root "/absolute/cdenv/root" proxy project
    IdentityFile "/absolute/cdenv/root/ssh/id_ed25519"
    IdentitiesOnly yes
    UserKnownHostsFile "/absolute/cdenv/root/ssh/known_hosts"
    StrictHostKeyChecking yes
    ServerAliveInterval 30
    ServerAliveCountMax 3
```

Use tested OpenSSH escaping and reject paths that cannot be represented safely. The workspace argument is a validated literal, never substituted user input.

Resolve the executable path from `argv[0]` through `PATH` to an absolute path without dereferencing the final stable symlink. Use `current_exe()` only as fallback.

Regenerate the managed config atomically during successful setup/mutating flows. `doctor` remains read-only.

### 14.3 User SSH Include consent

Generated content remains under the cdenv root. For direct `ssh project.cdenv`, add one global line to the user’s normal SSH config:

```sshconfig
Include /absolute/cdenv/root/ssh/config
```

- prompt only while remembered consent is `unknown` and stdin is a TTY;
- insert before the first `Host` or `Match` block;
- show the exact change;
- preserve formatting and permissions as much as possible;
- make insertion idempotent;
- require `--modify-ssh-config` for noninteractive modification;
- honor remembered `declined` without prompting again;
- `--no-modify-ssh-config` records/uses decline but never removes an existing Include;
- show manual instructions when automatic editing is unsafe.

`cdenv ssh` and `cdenv forward` always use `-F` and therefore do not require this Include.

---

## 15. Agent SSH Behavior

### 15.1 Stdio transport and logging

Expose a generic async stream entry point so tests can use in-memory duplex streams and production can use Tokio stdin/stdout.

Agent `ssh-server --stdio` stdout is exclusively SSH protocol data. Tracing goes to stderr with no ANSI formatting. Production code must not call stdout-oriented logging/printing from the server path.

### 15.2 Authentication

- accept only username `cdenv`;
- accept only public-key authentication;
- match the exact installation public key;
- reject password and keyboard-interactive methods;
- use the persistent workspace Ed25519 host key.

### 15.3 Session state

Use a runtime state enum because SSH requests arrive dynamically:

```text
Pending → RunningProcess
Pending → Forwarding
```

Pending state accumulates approved environment values and an optional PTY request. Reject invalid transitions such as a second shell/exec request.

Do not impose guessed channel-count quotas in V1. Use protocol/OS limits, bounded buffers, and backpressure. Never collect unbounded channel or forwarding data.

### 15.4 Environment

Start with the captured effective Dev Container environment. Accept client SSH environment requests only for:

```text
LANG
LC_*
TERM
COLORTERM
NO_COLOR
```

Reject all others, especially dynamic-loader and shell-startup overrides.

Provide conventional variables:

- synthetic loopback `SSH_CONNECTION` and `SSH_CLIENT` values;
- actual PTY path in `SSH_TTY`;
- exact exec string in `SSH_ORIGINAL_COMMAND`.

Network endpoint values are synthetic because the SSH transport has no TCP socket.

### 15.5 Working directory and shell

Start sessions in the authoritative `remoteWorkspaceFolder` recorded from the active effective Dev Container plan.

Resolve shell in order:

1. current UID’s passwd entry;
2. captured `SHELL`;
3. `/bin/sh`.

For non-PTY exec, match OpenSSH command semantics:

```text
<shell> -c <exact SSH command string>
```

Pass the command as one process argument. Never construct an additional quoted shell command.

For interactive shells, use login-shell argv semantics, allocate a controlling PTY, and do not assume Bash exists.

### 15.6 Non-PTY exec

```text
SSH stdin          → child stdin
child stdout       → SSH channel data
child stderr       → SSH extended-data stderr
child status       → SSH exit-status / exit-signal
```

Support EOF, close, cancellation, and binary data.

### 15.7 PTY and signals

- allocate Linux PTYs through the isolated OS module;
- establish a session, controlling terminal, and child process group;
- apply requested dimensions and supported terminal modes;
- process `window-change` with `TIOCSWINSZ`;
- map at least `INT`, `TERM`, `HUP`, `QUIT`, `KILL`, `USR1`, and `USR2`;
- signal the child process group so foreground descendants receive signals.

On channel disconnect:

1. send `SIGHUP` to the channel process group;
2. wait a short bounded grace period;
3. send `SIGTERM`;
4. send `SIGKILL` if required.

A process that intentionally daemonizes into a new session/process group survives, enabling editor-like remote helper reconnection.

### 15.8 TCP forwarding

Implement unrestricted standard `direct-tcpip` for the authenticated local client:

1. validate host representation and nonzero port;
2. connect asynchronously from inside the container;
3. bridge bidirectionally with bounded backpressure;
4. support concurrent forwarded connections;
5. close both sides cleanly on EOF/error.

`localhost` refers to the container. Do not implement reverse forwarding in V1.

### 15.9 OpenSSH interoperability

Support protocol behavior required by:

- `ControlMaster`/multiplexed channels;
- long-lived no-session master connections;
- keepalive/global request behavior;
- multiple concurrent exec, PTY, and forwarding channels;
- clean reconnect after a detached helper starts.

---

## 16. Errors, Output, Logs, and Security

### 16.1 Layered errors

Do not define one global cross-crate catch-all error.

- core: domain/state validation errors;
- Dev Container profile: syntax, validation, substitution, metadata, Feature, lock, and plan `thiserror` types;
- host adapters: Git, Docker CLI, Compose, Bollard, public Feature transport, storage, supervisor, and SSH setup `thiserror` types;
- agent: protocol/process/provision/lifecycle errors;
- binary boundary: user context and exit-code mapping, optionally with `anyhow`.

Error values crossing async task boundaries must satisfy required `Send + Sync + 'static` bounds. Test meaningful error variants/messages.

### 16.2 JSON output

Use versioned envelopes rather than bare arrays:

```json
{
  "schemaVersion": 1,
  "workspaces": [],
  "warnings": []
}
```

Use the same pattern for status/doctor and machine-readable errors. Under `--json`, stdout contains one valid JSON document and incidental diagnostics remain off stdout.

Exit behavior:

- `list`: success when local enumeration succeeds, with per-item failures/warnings;
- `status`: nonzero for requested-state failure while retaining valid JSON;
- `doctor`: nonzero when a required check fails;
- `ssh`/`forward`: OpenSSH exit status;
- proxy: concise nonzero failure on stderr only.

### 16.3 Human output

- no animated progress when output is not a TTY;
- mutating commands show operation, workspace, final state, and next command;
- tables remain deterministic and concise;
- proxy errors fit editor diagnostics, for example:

```text
cdenv: workspace "project" is stopped; run `cdenv up project`
```

### 16.4 Logs

- use unique operation/connection IDs and per-operation files;
- fixed bounded retention in V1; no unbounded append-only proxy log;
- permissions `0600`;
- redact values cdenv explicitly supplies or recognizes, including Docker/Compose environment and registry headers;
- never store the raw clone source argument, interpolated Compose model, effective environment, or substituted build arguments;
- document that arbitrary repository/Feature command output cannot be guaranteed secret-free;
- keep detailed external errors in logs and concise sanitized summaries in state;
- never route host or agent tracing into proxy stdout.

### 16.5 Repository source sanitization

Pass the original source only to Git. Store/display a sanitized description:

- remove HTTP URL userinfo credentials;
- remove query and fragment data;
- retain ordinary recognizable SSH usernames such as `git@host`;
- canonicalize local source paths.

The cloned repository’s own Git config may retain the source as normal Git behavior; cdenv does not duplicate it elsewhere.

### 16.6 Trust model

This is a local development tool. Access to the cdenv root, Docker daemon, or cdenv executable is highly privileged. SSH keys and private supervisor control paths provide stable identity and prevent accidental cross-workspace connections; they are not a sandbox against repository, Feature, Dockerfile, Compose, lifecycle, or host-initialization code.

Providing a repository source is trust consent for configuration-requested host execution and Docker privileges. Public Feature digest/lock verification provides integrity and trust on first use, not publisher authentication. Non-loopback `appPort` or ad-hoc `forward` exposes the target service to selected host interfaces and does not add service-level authentication.

---

## 17. Testing Strategy

Tests are living documentation:

- descriptive behavior-focused names;
- one behavior and ideally one assertion per test;
- share fixture setup, not action/assertion logic;
- tolerate clear duplication instead of wrong abstractions;
- test error variants/messages;
- use small snapshots for structured/human output, never giant unstable objects;
- add doc tests for intentionally public APIs.

### 17.1 Unit tests

- workspace validation/derivation, including URL/path forms;
- source sanitization;
- newtype and status logic;
- state serialization, migration, and corruption;
- atomic writes and permission enforcement;
- config path containment/symlink rejection;
- SSH config escaping and explicit host generation;
- user Include insertion before `Host`/`Match` and consent behavior;
- architecture/build-ID mapping;
- JSONC comments/trailing-comma rejection, bounds, source-span diagnostics, and config discovery ambiguity;
- raw/effective profile validation and unknown/unsupported property errors;
- specification metadata merge rules and staged variable substitution;
- build/create/runtime/lifecycle keyed plan fingerprints without persisted secret values;
- mount/appPort/forwardPorts/port-attribute parsing and requested/assigned endpoint rendering;
- Feature reference normalization, option validation, dependency cycles/equality, and deterministic round ordering;
- lockfile parse/generate/staleness/integrity and atomic explicit mutation;
- reserved Docker argument detection and command generation;
- Compose project-name, override, and managed-service-set generation;
- lifecycle command forms/order/checkpoint transitions and drift classification;
- environment snapshot filtering/encoding;
- command argument generation for Git/OpenSSH/Docker/Compose.

### 17.2 Component tests

Use statically dispatched fakes for orchestration:

- Docker running/stopped/missing/unavailable/duplicate/drift;
- replacement container after rebuild;
- unsupported architecture;
- archive/provision/version/environment failures;
- Dev Container syntax/metadata/Feature/lock/build/create/runtime/lifecycle failures;
- public OCI anonymous-token and HTTP tarball success, digest/media-type/redirect/archive attacks, and cache behavior against mock servers;
- Docker/Compose output verification and reserved option failures;
- desired/active drift, stale lock, invalid desired config with healthy active state, and replacement rollback;
- complete Compose managed-service stop and non-atomic partial replacement;
- forwarding supervisor startup, alternate assignment, required-port degradation, transactional update, crash, build mismatch, and target loss;
- background lifecycle success/failure/indeterminate/cancellation and repeated-up behavior;
- cancellation and stale foreground-operation recovery;
- one Docker list call for many workspaces;
- Docker multiplexing and bounded stream behavior.

### 17.3 Agent protocol tests

Run the server over an in-memory duplex stream or socket harness:

- valid/invalid key and username;
- exec stdout/stderr/exit separation;
- binary-clean long-lived channels;
- PTY shell, resize, and signal behavior on Linux;
- multiple channels;
- direct TCP forwarding and cancellation;
- environment probe/snapshot allowlist and conventional SSH variables;
- lifecycle runner checkpoints, closed stdin, bounded logs, and long-running commands;
- forwarding bridge and cancellation;
- disconnect cleanup and detached-process survival.

### 17.4 Integration-test package

Root tests are a non-published workspace package. Ordinary `cargo test --workspace` remains Docker-free. Real tests run through:

```bash
cargo xtask test-integration
```

When CI declares integration support, missing Docker Engine/CLI, Compose V2, or OpenSSH dependencies are failures, never silent skips.

Fixtures include:

```text
basic-debian
basic-alpine
custom-remote-user
compose-primary-and-service
image-metadata-merge
public-oci-features
https-and-local-features
lockfile-stale
forwarding-server
failing-and-background-lifecycle
build-create-runtime-drift
read-only-best-effort
```

Use a small static test helper preinstalled in fixtures to simulate an editor remote server independently of any editor or container utility.

### 17.5 Automated Dev Container profile release gate

The in-repository black-box suite must verify:

1. pinned-schema discovery, parsing, validation, metadata merge, and substitutions;
2. image, Dockerfile, and Compose V2 creation/resume/down/rebuild;
3. public OCI, HTTPS, and local Features with dependencies/options/order and frozen/generated locks;
4. workspace mounts, users, UID/GID update, environment probe, Docker options, and host requirements;
5. lifecycle ordering, parallel groups, `waitFor`, background failure, cancellation, and immutable generation plans;
6. app-port publication, declared forwarding lifetime/attributes, alternate ports, and external stop/reconnect;
7. desired/active category drift without implicit rebuild;
8. Compose project isolation, managed sibling stop, volume preservation, and orphan cleanup;
9. unsupported/private/insecure inputs fail closed with profile-aware errors;
10. no lifecycle operation mutates the checkout except explicit `cdenv lock` and repository-defined code.

Optional differential tests may compare selected fixtures with the captured behavior of the official CLI version recorded in the ADR. They are diagnostic and must not make Node.js a normal CI/release dependency.

### 17.6 Automated OpenSSH release gate

The in-repository black-box suite must verify:

1. successful authentication and invalid-key/host-key rejection;
2. exec with exact stdout, stderr, and exit status;
3. long-lived binary-clean stdio;
4. `ControlMaster` with concurrent multiplexed sessions;
5. interactive PTY, resize, and Ctrl+C;
6. detached helper survival and reconnect;
7. local forwarding with multiple simultaneous connections;
8. declarative forwarding lifetime, alternate assignment, Compose service targets, and supervisor recovery;
9. foreground `cdenv forward`, including explicit non-loopback binding and declared-listener conflicts;
10. serialized `postAttachCommand` per new transport, including forwarding-only transports;
11. disconnect and process-group cleanup;
12. applicable behavior on Debian and Alpine.

### 17.7 Platform matrix

Release artifacts:

- macOS arm64;
- Linux arm64;
- Linux x86_64.

Automated complete Docker/OpenSSH suites:

- Linux arm64;
- Linux x86_64.

Documented pre-release Docker Desktop smoke tests:

- macOS Apple Silicon.

Named editor observations are compatibility notes, not release gates.

---

## 18. Implementation Sequence

Each chunk ends with focused tests and a coherent commit. Do not begin broad production implementation until Chunk 0 records feasibility and profile findings. Internal previews may exist, but no release may silently accept a configuration whose profile behavior is unfinished.

### Chunk 0: Disposable feasibility and profile spike

- select/pin the upstream specification commit and vendor candidate schemas/fixtures;
- make the repository’s own Dev Container configuration schema-valid, including removal of its current trailing comma;
- prove image, Dockerfile+Feature, metadata, lifecycle, declared-forward, and Compose V2 fixtures;
- select minimum Docker Engine/CLI/Compose versions and pin/test candidate Russh/Bollard/JSONC/HTTP APIs;
- prove public OCI anonymous pull, digest/lock generation, generated Feature/UID layers, and safe archive handling;
- prove Docker/Compose identity labels, project/service discovery, managed stop, and drift-safe resume;
- prove lifecycle checkpoints/`waitFor`, forwarding supervisor, Docker Exec stdio, OpenSSH handshake, ControlMaster, PTY, and forwarding;
- write the profile/spike ADR and V1 support matrix;
- discard or quarantine spike implementation code.

**Gate:** all Section 5 criteria pass and no unresolved profile behavior remains.

### Chunk 1: Cargo workspace, standards, and core domain

- create `cdenv-core`, `cdenv-devcontainer`, host, agent, and xtask targets;
- pin toolchain, dependencies, workspace lints, formatting, docs, tests, and cargo-deny;
- implement validated newtypes, naming, multidimensional status, profile/generation IDs, and layered core errors;
- define CLI parser/help including `lock`, `forward`, config selection, and rebuild `--no-cache`.

**Gate:** baseline quality commands pass and help lists every V1 command.

### Chunk 2: Root, installation/workspace state, fingerprints, and locks

- root precedence and UTF-8/path validation;
- managed layout/permissions, installation identity, and fingerprint key;
- desired/active schema, category fingerprints, lifecycle/forwarding dimensions, and migration framework;
- atomic writer, workspace enumeration, global reservation, and per-workspace standard-library locks;
- private runtime control paths and stale foreground-operation interpretation.

**Gate:** managed files remain under root except consented SSH Include and later explicit repository lockfile writes; no effective secret is persisted.

### Chunk 3: Git and durable create transaction

- Git dependency detection and safe subprocess invocation;
- URL/local path naming and source sanitization;
- clone transaction/cleanup/log behavior;
- selected config containment;
- create state retained after post-clone failure;
- local bare-repository tests.

At this stage create may finish as `missing`; environment startup is added after orchestration exists.

### Chunk 4: Dev Container syntax, metadata, and immutable planner

- deterministic discovery and strict bounded JSONC parsing;
- pinned raw/effective models and semantic validation;
- image metadata merge logic and staged substitutions;
- mount, port, user/environment, host requirement, and lifecycle command models;
- build/create/runtime/lifecycle plan generation and keyed fingerprints;
- explicit unknown/unsupported property diagnostics and profile capability output;
- upstream and security fixture unit tests.

**Gate:** pure fixture inputs produce stable reviewed plans with no Docker/network access.

### Chunk 5: Docker CLI and Bollard image-scenario adapters

- one resolved Unix Docker endpoint and external capability/version checks;
- typed Docker CLI pull/build/create adapter with reserved-option validation and bounded/redacted logs;
- deterministic context/generated-material handling outside checkout;
- stable labels/names/generations;
- Bollard list/inspect/verify/rename/start/stop/upload/attached+detached Exec;
- fake adapters and opt-in real component tests.

**Gate:** image and Dockerfile plans create verifiable labeled containers; attached byte round trip is exact.

### Chunk 6: Features, cache, lockfile, and generated images

- public OCI anonymous token/manifest/blob client and unauthenticated HTTPS/local sources;
- bounded content-address cache and hardened extraction;
- metadata/options/defaults, dependency graph/equality/order, and cycle errors;
- frozen lock validation and atomic explicit `cdenv lock` generation;
- generated Feature/UID/metadata Dockerfile layers and cleanup labels;
- offline frozen-cache and hostile-source tests.

**Gate:** public OCI, HTTPS, and local Feature fixtures build deterministically and lock operations are the only cdenv checkout writes.

### Chunk 7: Compose V2 orchestration

- exact Compose V2 version/capability checks;
- stable project naming and deterministic JSON override generation;
- secret-safe config parsing and primary/managed service discovery;
- create/start/stop/resume behavior with `runServices` and dependencies;
- volume preservation, project isolation, drift-safe direct resume, and partial-state reporting;
- two-workspace/multi-service integration tests.

**Gate:** complete managed environments stop/resume without deleting named volumes or touching unrelated services.

### Chunk 8: Agent build, provisioning, environment, and lifecycle runner

- agent `version`, `identity`, `provision`, environment probe/capture, lifecycle runner, and forwarding bridge commands;
- shared build ID/protocol and musl xtask staging/embedding;
- tool-free staging/root provisioning and always-reinstall behavior;
- binary-safe effective environment merge/storage;
- restricted lifecycle checkpoints, closed-stdin/background execution, bounded logs, cancellation, and long-running commands;
- Debian/Alpine, architecture, and custom-user tests.

**Gate:** cdenv securely provisions the agent and can resume/reconcile lifecycle state without container utilities.

### Chunk 9: Lifecycle orchestration and core CLI environment flows

- create/up/down through immutable plans with specification lifecycle ordering and `waitFor`;
- desired/active build/create/runtime drift and invalid-desired behavior;
- no duplicate background runners and rebuild-only one-time failure recovery;
- full Compose managed stop and image direct restart;
- live status correlation, duplicate/drift dimensions, and list/status human/versioned JSON;
- create calls shared up orchestration; cancellation and operation recovery.

**Gate:** fixture checkouts survive create/up/down/up, lifecycle gates are correct, and status uses one Docker list query where practical.

### Chunk 10: Declarative port publication and forwarding supervisor

- `appPort` Docker publication and non-loopback warnings;
- private per-workspace supervisor control/lifetime protocol;
- `forwardPorts` primary/Compose targets, requested/assigned endpoints, alternate/required behavior;
- transactional runtime plan updates, stable assignments, target-loss retry, crash/build mismatch, reboot/up recovery, and down cleanup;
- port attribute URL/warning rendering and deferred-discovery diagnostics.

**Gate:** declared ports remain reachable after `up` exits, survive transient target loss, and release on `down`.

### Chunk 11: SSH identity and generated configuration

- client and per-workspace host Ed25519 keys;
- strict permissions and stable known-host entries;
- explicit per-workspace SSH blocks and safe executable/root rendering;
- consented idempotent Include insertion;
- `cdenv ssh` explicit-config wrapper and `ssh -G` tests.

**Gate:** OpenSSH resolution is correct without wildcard ProxyCommand substitution.

### Chunk 12: Agent SSH handshake, authentication, and exec

- generic stdio async stream, Russh server, and host key loading;
- exact user/public-key authentication;
- effective environment/allowlist, session state, and non-PTY shell `-c` execution;
- stdout/stderr/exit reporting, bounded channel I/O, and strict stderr logging.

**Gate:** protocol harness observes exact output separation and exit status.

### Chunk 13: Host proxy and attach lifecycle through Bollard

- brief shared lifecycle coordination and strict active generation/provision checks;
- serialized `postAttachCommand` per transport with retryable attach degradation;
- attached agent Exec as active remote user/workspace;
- binary-clean forwarding/cancellation and concise bounded diagnostics.

**Gate:** system OpenSSH runs a remote command with one attach hook and no exposed SSH port or permanent SSH daemon.

### Chunk 14: PTY, multiplexing, signals, and cleanup

- isolated minimal-unsafe Linux PTY module;
- login shell/session/process groups, terminal modes/resizing, and signal mapping;
- HUP/TERM/KILL cleanup and detached helper behavior;
- ControlMaster concurrent channels/transports and attach serialization.

**Gate:** automated PTY/multiplexing suite passes on Debian and Alpine.

### Chunk 15: Direct forwarding and ad-hoc `cdenv forward`

- unrestricted `direct-tcpip` and concurrent bounded TCP bridges;
- system OpenSSH foreground wrapper;
- all-or-none mapping preflight, declared-listener conflict, loopback default, and explicit non-loopback warning;
- service/network bind and forwarding-only attach-hook tests.

**Gate:** ad-hoc host mappings reach container services without config changes or rebuild.

### Chunk 16: Rebuild, rollback, and cleanup

- cached/no-cache desired-plan rebuild;
- build-first image backup/rename/best-effort rollback;
- Compose force-recreate partial-state handling;
- uncommitted-change notice, replacement/generation validation, and complete reprovision;
- forwarding handoff, orphan cleanup, bounded generated-image retention, and stable host identity;
- interrupted rebuild/drift recovery tests.

**Gate:** tracked/untracked changes and named volumes survive; generation changes; SSH reconnects without host-key warnings.

### Chunk 17: Compatibility and profile hardening

- complete independent Dev Container and OpenSSH release-gate suites;
- long-lived lifecycle/SSH/binary/forwarding behavior;
- malformed config/OCI/archive/Docker/Compose and cancellation edge cases;
- concurrent exec/PTY/forward/attach channels;
- bounded buffering/throughput checks measured in release mode.

**Gate:** Sections 17.5 and 17.6 pass without Node.js or an editor dependency.

### Chunk 18: Doctor, packaging, CI, and release

- read-only profile-aware `doctor` and JSON envelope;
- all three host artifacts with both embedded agents;
- minimum/pinned Docker Engine/CLI/Compose jobs and Linux architecture integration matrix;
- macOS Docker Desktop smoke checklist;
- checksums, installation/profile/lock/upgrade/state-migration documentation;
- forwarding/lifecycle/trust troubleshooting and compatibility notes.

**Gate:** one installed host binary satisfies the complete V1 profile and all Section 19 workflows.

---

## 19. V1 Definition of Done

The following workflows work:

```bash
cdenv create https://github.com/example/project.git
cdenv create https://github.com/example/project.git --name project-second
cdenv create /path/to/local/repository.git --name local-project
cdenv list
cdenv list --json
cdenv status project
cdenv lock project
cdenv down project
cdenv up project
cdenv rebuild project
cdenv rebuild project --no-cache
cdenv ssh project
ssh project.cdenv
ssh -L 8080:localhost:3000 project.cdenv
cdenv forward project 8080:3000
cdenv forward project 8080:3000 --bind 0.0.0.0
```

And:

- the pinned `cdenv-devcontainer-v1` support matrix and profile release gate pass for image, Dockerfile, and Compose V2 scenarios;
- strict JSONC discovery/validation, image metadata, substitutions, users, mounts, Docker options, host requirements, and lifecycle semantics are deterministic;
- public OCI, unauthenticated HTTPS, and local Features honor options/dependencies/order, frozen locks, digest integrity, and hardened caching/extraction;
- `cdenv lock` is the only cdenv operation that writes into the checkout after clone; cdenv never mutates Git state;
- source changes survive stop/start/rebuild except changes intentionally made by repository/Feature lifecycle or initialization code;
- desired/active build/create drift warns without implicit rebuild, safe runtime drift applies, and invalid desired config never destroys a healthy active environment;
- app ports publish with documented binding rules and declared forwards persist from `up` to `down` with requested/assigned status;
- ad-hoc forwarding remains foreground, all-or-none, and requires explicit non-loopback binding;
- no `sshd` or SSH container port is required;
- no installation-wide daemon or permanent container SSH daemon exists; scoped forwarding supervisors and active lifecycle runners are owned, diagnosed, and cleaned up;
- the agent runs in Debian and Alpine on x86_64 and arm64;
- effective remote-user environment is available to lifecycle and SSH sessions without host persistence;
- live status derives from one Docker query where practical and retains independent config/lifecycle/forwarding dimensions;
- duplicate/external replacement/drift conditions fail safely instead of selecting arbitrary containers;
- Compose projects isolate workspaces, manage the configured service set and dependencies, stop them completely, and preserve named volumes;
- host, supervisor, runner, and agent upgrades cannot silently mix incompatible builds/protocols/generations;
- all cdenv-managed host state remains under the configured root except the consented SSH Include and explicit lockfile target;
- proxy stdout is proven binary-clean and attach hooks cannot contaminate it;
- automated Dev Container and editor-independent OpenSSH compatibility suites pass;
- release binaries contain both verified static agent artifacts;
- strict Rust lint, documentation, security, test, and unsafe-code policies pass.
