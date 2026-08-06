# cdenv Implementation Plan

**Project:** `cdenv` — Containerized Development Environment
**Website:** `cdenv.sh`
**Implementation:** Rust 2024 edition
**Hosts:** macOS and Linux on x86_64 and arm64
**Containers:** Local Docker, managed through the official Dev Container CLI
**Workspace format:** Development Containers (`devcontainer.json`)
**Docker API:** Bollard
**Editor integration:** Standard OpenSSH; no editor-specific implementation

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
- a Dev Container CLI-created primary development container;
- an injected static Rust SSH agent;
- a stable SSH hostname, `<name>.cdenv`;
- a stable SSH host key.

The container does not run OpenSSH `sshd` and does not publish port 22. OpenSSH starts a local ProxyCommand, which creates a Docker Exec process through Bollard and runs:

```text
cdenv-agent ssh-server --stdio
```

SSH protocol bytes flow through stdin/stdout. Each SSH connection has one host proxy process and one container agent process. There is no persistent cdenv daemon.

---

## 2. Goals

### 2.1 Primary goals

1. Provide a simple repository-first CLI for local Docker development containers.
2. Keep cdenv-managed host files under one configurable root, defaulting to `~/.cdenv`.
3. Delegate Development Container behavior to the official Dev Container CLI.
4. Use Bollard for cdenv-owned Docker inspection, upload, status, stop, and exec operations.
5. Expose workspaces through standard OpenSSH behavior without `sshd` or published SSH ports.
6. Preserve the host checkout across stop, start, and rebuild.
7. Support concurrent SSH clients, multiplexed channels, PTYs, commands, signals, and local TCP forwarding.
8. Install one host binary containing Linux x86_64 and arm64 static agent artifacts.
9. Produce actionable diagnostics without ever corrupting the SSH byte stream.
10. Keep modules and implementation chunks small enough to test independently without pre-inventing abstractions.

### 2.2 Ad-hoc port forwarding

A user can expose a container service without changing `devcontainer.json`, rebuilding, or publishing a Docker port:

```bash
cdenv forward project 8080:3000
cdenv forward project 8080:3000 --bind 0.0.0.0
cdenv forward project 8080:3000 5432:5432
```

The command is foreground and session-scoped. It delegates to system OpenSSH local forwarding and ends on Ctrl+C. Binding to a non-loopback address is explicit and produces a security warning.

### 2.3 Compatibility promise

V1 promises the SSH behavior verified by the in-repository OpenSSH compatibility suite. Named editors are not release gates. Documentation may record editor versions observed to work, but cdenv contains no editor-specific behavior.

---

## 3. Non-goals for V1

- remote Docker daemons, repository synchronization, or remote workspace placement;
- Kubernetes, cloud providers, Windows hosts, or Windows containers;
- automatic editor launching;
- reimplementation of the Dev Container specification;
- a persistent host or container cdenv daemon;
- automatic self-update;
- background or persistent port-forward management;
- SFTP, SSH agent forwarding, reverse forwarding, or Unix-socket forwarding;
- destructive workspace/repository deletion commands;
- Git pull, branch, reset, clean, stash, or credential management;
- snapshots or background synchronization;
- managing Docker Compose sibling services directly;
- guaranteed editor-server support in every image, especially minimal/musl images;
- shell completions unless added after core V1 work is complete.

Remote Docker is not being pre-designed. Local transport assumptions must remain inside the local Docker adapter, but remote support is not expected to be “just another adapter”: repository placement and Dev Container execution would also need design.

---

## 4. Required Host Dependencies

The host must provide:

- a reachable local Docker Engine or Docker Desktop daemon;
- the Docker CLI required transitively by the official Dev Container CLI;
- the official `devcontainer` CLI;
- Git;
- an OpenSSH-compatible `ssh` client.

`cdenv` never directly invokes `docker` for its own Docker operations. The Dev Container CLI is permitted to invoke Docker internally.

Commands must perform only the dependency checks they need. For example, `list` remains useful when Docker is unavailable, while `create` performs a complete preflight before starting expensive work.

### 4.1 Docker endpoint consistency

Resolve one local Unix-domain Docker socket at startup:

1. accept a standard `DOCKER_HOST` only when it is a Unix socket;
2. support known Docker Desktop, rootless, and `/var/run/docker.sock` local paths;
3. reject TCP, SSH, and remote context endpoints in V1;
4. construct Bollard from the resolved socket;
5. pass the same socket to Dev Container CLI subprocesses as `DOCKER_HOST=unix://...`.

This prevents the Dev Container CLI and Bollard from accidentally targeting different daemons.

### 4.2 External version support

The feasibility spike determines the minimum supported Dev Container CLI version from verified flags and output schemas. V1 must:

- reject older versions with an actionable message;
- accept newer versions unless a real compatibility check fails;
- record detected versions in operation logs and `doctor` output.

Required CI uses the declared minimum and the repository’s pinned/tested version. Testing `latest` on a schedule is a future improvement, not a V1 requirement.

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

Prove both:

1. a single-container Dev Container fixture;
2. a two-service Compose fixture where only the primary development service is accessed.

For Compose, prove that two cdenv workspace identities result in isolated Compose projects.

### 5.3 Spike success criteria

- `--id-label` applies stable custom labels to image- and Compose-backed primary containers;
- Dev Container JSON output reliably provides the primary container ID, remote user, and remote workspace folder;
- Bollard Exec supports binary-clean, bidirectional, cancellation-aware stdio;
- Docker stdout/stderr framing never reaches SSH stdout;
- Russh can serve a connection over a generic stdio stream;
- system OpenSSH completes authentication and command execution;
- an OpenSSH `ControlMaster` can multiplex concurrent sessions;
- a PTY shell, resize, and Ctrl+C work sufficiently to validate the approach;
- `direct-tcpip` supports local forwarding;
- disconnect cleanup is feasible without leaving a cdenv agent daemon.

### 5.4 Spike outputs

Spike code is disposable. Retain:

- an ADR recording exact Bollard/Russh/Dev Container versions and APIs;
- verified command lines and JSON fixtures;
- Compose-label and isolation findings;
- packet-flow, cancellation, and multiplexing findings;
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

#### `cdenv-cli`

Host application library and `cdenv` binary:

- CLI parsing and exit rendering;
- local state, locking, Git, and paths;
- Dev Container and Docker adapters;
- lifecycle orchestration;
- agent installation;
- SSH identity/configuration;
- proxy transport and system-SSH wrappers;
- human and JSON output.

`src/lib.rs` contains testable application behavior. `src/main.rs` is limited to parsing, runtime setup, invoking the library, and rendering an exit status.

#### `cdenv-agent`

Agent library and Linux-targeted binary:

- `version`, `identity`, `provision`, environment capture, and `ssh-server` commands;
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
├── workspaces/
│   └── project/
│       ├── checkout/
│       │   └── project/       # Git checkout / --workspace-folder
│       ├── state.json
│       ├── .lock
│       └── logs/
├── ssh/
│   ├── config
│   ├── known_hosts
│   ├── id_ed25519
│   ├── id_ed25519.pub
│   └── host_keys/
├── cache/devcontainer/
├── logs/
└── tmp/
```

The checkout basename intentionally equals the workspace name. This soft-guides the Dev Container CLI toward `/workspaces/<name>`, but the CLI-returned `remoteWorkspaceFolder` is always authoritative.

Do not create `config.toml` until a real global setting exists.

### 7.3 Installation record

`installation.json` contains operational installation metadata, including:

- schema version;
- stable random installation ID;
- SSH Include consent: `unknown`, `accepted`, or `declined`.

The installation ID namespaces Docker resources shared by multiple users or roots.

### 7.4 Permissions and symlinks

For exclusively cdenv-managed paths:

```text
root and SSH directories      0700
private keys                  0600
state and logs                0600
public keys                   0644
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
  "desiredDevcontainerConfig": ".devcontainer/devcontainer.json",
  "createdAt": "...",
  "lastUpAt": "...",
  "operation": {
    "kind": "idle",
    "id": null,
    "startedAt": null
  },
  "lastError": null,
  "provisioned": {
    "containerId": "...",
    "remoteUser": "vscode",
    "remoteWorkspaceFolder": "/workspaces/project",
    "containerArchitecture": "aarch64",
    "agentPath": "/usr/local/libexec/cdenv/cdenv-agent",
    "agentBuildId": "...",
    "protocolVersion": 1,
    "environmentPath": "..."
  }
}
```

The exact serde representation may change during Chunk 1, but it must preserve these distinctions.

### 9.2 Status dimensions

Model independently:

- **container:** running, stopped, missing, ambiguous, Docker unavailable;
- **operation:** idle, creating, starting, rebuilding, stopping;
- **local health:** valid, interrupted, last operation failed, corrupt, provision drift.

Human output derives a concise display; JSON retains all dimensions. A running container and failed rebuild can both be true.

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
- use `workspaces/<name>/.lock` for create/up/down/rebuild;
- mutating commands hold the exclusive lock for the full operation;
- read-only commands do not rewrite stale state;
- proxy takes a shared lock only through resolve/inspect/Exec attach, then releases it;
- proxy fails quickly when a lifecycle operation holds the exclusive lock;
- existing SSH sessions never hold lifecycle locks.

A persisted operation is active only while the lock is unavailable. If state records an operation but the lock is available, report an interrupted previous operation. The next mutating command may recover it.

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
proxy
doctor
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
- failure after clone: retain checkout and state, record the error, and allow plain `up` to retry;
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

Without `--config`, use the official CLI’s normal default lookup.

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

Always delegate lifecycle decisions to `devcontainer up`; never directly start a stopped container through Bollard. Then verify labels/identity, reinstall and provision the agent, recapture the effective remote environment, update state atomically, and regenerate SSH material.

`up` is idempotent but always reprovisions the expected agent and assets.

### 10.5 `down`

```bash
cdenv down <name>
```

Stop only the labeled primary development container through Bollard. Do not delete the checkout, container, or volumes. In Compose workspaces, sibling services may remain running; document this explicitly.

### 10.6 `rebuild`

```bash
cdenv rebuild <name> [--config <path>] [--no-cache]
```

The current Dev Container CLI has no separate rebuild command. Implement:

```text
devcontainer up --remove-existing-container
```

Map `--no-cache` to `--build-no-cache`. Cached rebuild is the default.

Before rebuilding, use `git status --porcelain` only to print whether uncommitted changes exist. Do not block or expose filenames. After lifecycle success, require a replacement container ID when a prior container existed, then reprovision all agent/SSH/environment assets.

### 10.7 `status`

```bash
cdenv status <name> [--json]
```

Show local source/path, selected config, current Git branch when available, all status dimensions, container ID/architecture, remote user/folder, and provisioned agent build/protocol.

Exit nonzero when the requested workspace is missing, corrupt, ambiguous, or cannot be queried as requested. Under `--json`, still emit a valid error envelope.

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
- run foreground until interrupted;
- delegate to:

```text
ssh -F <config> -N -o ExitOnForwardFailure=yes \
    -L <bind>:<local-port>:localhost:<container-port> ... <name>.cdenv
```

Use process arguments, never a shell command. Propagate the OpenSSH exit status.

### 10.10 `proxy`

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

After an external container replacement or host-agent upgrade, fail with a concise instruction to run `cdenv up <name>`. Proxy never provisions or mutates lifecycle state.

### 10.11 `doctor`

Read-only checks:

- cdenv root ownership/permissions and schema;
- installation ID and SSH consent;
- Git, OpenSSH, Docker CLI, and Dev Container CLI versions;
- local Docker socket and Bollard connectivity;
- generated SSH config syntax and Include visibility;
- client/host key validity and permissions;
- embedded agent artifacts/build IDs for both architectures;
- duplicate/stale labeled containers;
- provision drift and stale operation state.

Exit nonzero when a required invariant fails. V1 `doctor` diagnoses but does not repair.

---

## 11. Dev Container CLI Adapter

### 11.1 One lifecycle authority

Use the official CLI for every `up` and rebuild decision. Encapsulate invocation and JSON parsing in one adapter. Command modules must never parse raw output.

The adapter returns at least:

- primary container ID;
- remote user specification;
- authoritative remote workspace folder;
- Compose project information when present;
- structured failure details.

Use typed serde response structures that tolerate unknown fields but require fields cdenv needs. Bound captured JSON size and stream human logs to bounded operation files.

### 11.2 Stable identity labels

Generate a stable installation ID and pass both labels on every applicable Dev Container invocation:

```text
--id-label cdenv.installation=<installation-id>
--id-label cdenv.workspace=<workspace-name>
```

Verify both labels on the returned container before provisioning.

Discovery policy:

- connection/status may accept exactly one running match and report stopped stale matches;
- multiple running matches are ambiguous and fail;
- every mutating lifecycle command fails if more than one total matching container exists, including stopped containers, because the Dev Container CLI could select an arbitrary match;
- diagnostics list all conflicting IDs and provide explicit manual Docker cleanup instructions.

### 11.3 Compose behavior

V1 supports only the primary development service selected by the repository’s Dev Container configuration. Sibling services are neither inspected nor stopped directly by cdenv.

Set a stable Docker-safe `COMPOSE_PROJECT_NAME` derived from installation ID and workspace name for Dev Container CLI subprocesses. Truncate with a stable hash when necessary. This intentionally overrides repository-default Compose project naming to isolate multiple cdenv checkouts.

### 11.4 Lockfile preservation

The Dev Container CLI may generate `devcontainer-lock.json`, which would modify the checkout. Preserve repository files by using:

- `--frozen-lockfile` when the applicable lockfile already exists;
- `--no-lockfile` when no lockfile exists.

If a checked-in lockfile is stale, fail rather than rewriting it.

### 11.5 Repository mutation boundary

`cdenv` itself never runs Git mutation commands and never writes its own files into the checkout after clone. Repository-defined Dev Container lifecycle commands may modify the mounted checkout exactly as they would when run through the official CLI; this is outside cdenv’s preservation guarantee.

### 11.6 Effective remote environment

A plain Docker Exec can miss `remoteEnv` and user-environment probe results. After agent installation, run the agent once through `devcontainer exec` as the selected remote user so it captures its effective environment into a restricted file inside the container.

- do not print captured values;
- do not persist them in host state or logs;
- store them in a binary-safe format supporting non-UTF-8 Unix environment values;
- remove transient entries such as stale `PWD`, `OLDPWD`, `SHLVL`, `_`, and SSH session variables;
- load the snapshot for Bollard-launched SSH sessions;
- recapture on every `up`/rebuild.

---

## 12. Docker/Bollard Adapter

Bollard is the only direct Docker Engine integration used by cdenv.

Required capabilities:

- local Unix-socket connection and ping;
- one-call list/filter by installation and workspace labels;
- container inspect and architecture detection;
- stop primary container;
- archive upload;
- attached Exec creation/start/inspect;
- binary-clean stdin/stdout/stderr streaming;
- cancellation and bounded API timeouts.

The adapter must decode Docker multiplexed frames internally:

```text
local stdin       → Docker Exec stdin
Docker stdout     → local stdout exactly
Docker stderr     → local stderr/log exactly
```

No framing bytes may escape. Do not allocate a Docker TTY; SSH handles channel PTYs.

Lifecycle commands have no overall fixed timeout. Individual Docker control/discovery calls do. Ctrl+C terminates owned subprocess groups and records interruption. SSH/proxy sessions have no cdenv idle timeout.

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
7. capture effective environment through `devcontainer exec`;
8. persist the successfully provisioned record only after every step succeeds.

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

Start sessions in the authoritative `remoteWorkspaceFolder` returned by the Dev Container CLI.

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
- host adapters: Git, Docker, Dev Container, storage, and SSH setup `thiserror` types;
- agent: protocol/process/provision errors;
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
- redact known sensitive inputs;
- never store the raw clone source argument;
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

This is a local development tool. Access to the cdenv root, Docker daemon, or cdenv executable is highly privileged. SSH keys provide stable identity and prevent accidental cross-workspace connections; they are not a sandbox against a repository’s Dev Container code.

Non-loopback `forward` exposes the target service to interfaces selected by the user and does not add service-level authentication.

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
- Dev Container JSON parsing and error schemas;
- Compose project-name generation;
- lockfile argument selection;
- environment snapshot filtering/encoding;
- command argument generation for Git/OpenSSH/Dev Container.

### 17.2 Component tests

Use statically dispatched fakes for orchestration:

- Docker running/stopped/missing/unavailable/duplicate/drift;
- replacement container after rebuild;
- unsupported architecture;
- archive/provision/version/environment failures;
- Dev Container lifecycle/config/lockfile failures;
- cancellation and stale operation recovery;
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
- environment allowlist and conventional SSH variables;
- disconnect cleanup and detached-process survival.

### 17.4 Integration-test package

Root tests are a non-published workspace package. Ordinary `cargo test --workspace` remains Docker-free. Real tests run through:

```bash
cargo xtask test-integration
```

When CI declares integration support, missing Docker/OpenSSH/Dev Container dependencies are failures, never silent skips.

Fixtures include:

```text
basic-debian
basic-alpine
custom-remote-user
compose-primary-and-service
forwarding-server
failing-lifecycle
read-only-best-effort
```

Use a small static test helper preinstalled in fixtures to simulate an editor remote server independently of any editor or container utility.

### 17.5 Automated OpenSSH release gate

The in-repository black-box suite must verify:

1. successful authentication and invalid-key/host-key rejection;
2. exec with exact stdout, stderr, and exit status;
3. long-lived binary-clean stdio;
4. `ControlMaster` with concurrent multiplexed sessions;
5. interactive PTY, resize, and Ctrl+C;
6. detached helper survival and reconnect;
7. local forwarding with multiple simultaneous connections;
8. foreground `cdenv forward`, including explicit non-loopback binding;
9. disconnect and process-group cleanup;
10. applicable behavior on Debian and Alpine.

### 17.6 Platform matrix

Release artifacts:

- macOS arm64;
- macOS x86_64;
- Linux arm64;
- Linux x86_64.

Automated complete Docker/OpenSSH suites:

- Linux arm64;
- Linux x86_64.

Documented pre-release Docker Desktop smoke tests:

- macOS Apple Silicon;
- macOS Intel when hardware is available.

Named editor observations are compatibility notes, not release gates.

---

## 18. Implementation Sequence

Each chunk ends with focused tests and a coherent commit. Do not begin broad production implementation until Chunk 0 records feasibility findings.

### Chunk 0: Disposable end-to-end feasibility spike

- prove image and Compose fixtures;
- pin/test candidate Russh and Bollard APIs;
- prove custom id labels and JSON output;
- prove Docker Exec stdio, OpenSSH handshake, ControlMaster, PTY, and forwarding;
- write the spike ADR;
- discard or quarantine spike implementation code.

**Gate:** all Section 5 spike criteria pass.

### Chunk 1: Cargo workspace, standards, and core domain

- create crates and thin binary/library targets;
- pin toolchain and workspace lints;
- configure formatting, Clippy, docs, tests, and cargo-deny;
- implement validated newtypes, naming, status dimensions, schema types, and layered core errors;
- define CLI parser/help including `forward`, config selection, and rebuild `--no-cache`.

**Gate:** all baseline quality commands pass and help lists every V1 command.

### Chunk 2: Root, installation state, workspace store, and locks

- root precedence and UTF-8/path validation;
- managed layout and permissions;
- installation ID and SSH consent state;
- atomic state/config writer;
- schema migration framework;
- workspace enumeration;
- global reservation and per-workspace standard-library locks;
- stale-operation interpretation.

**Gate:** no managed host file escapes the configured root except an explicitly consented SSH Include edit.

### Chunk 3: Git and durable create transaction

- Git dependency detection and safe subprocess invocation;
- URL/local path naming and source sanitization;
- clone transaction/cleanup/log behavior;
- selected config containment;
- create state retained after post-clone failure;
- local bare-repository tests.

At this stage create may finish as `missing`; container startup is added after lifecycle orchestration exists.

### Chunk 4: Dev Container and local Docker adapters

- exact versions chosen from spike findings;
- one resolved Unix Docker endpoint;
- typed Dev Container invocation/JSON parser;
- stable dual labels;
- stable Compose project naming;
- lockfile preservation flags;
- Bollard list/inspect/stop/upload/attached-exec;
- bounded logging and API timeouts;
- fake adapters and opt-in real component tests.

**Gate:** attached `cat`-style round trip is byte-exact and Compose primary discovery is unambiguous.

### Chunk 5: Lifecycle without SSH agent

- `up` always through Dev Container CLI;
- `down` primary-only through Bollard;
- duplicate-container safety;
- live status correlation and drift dimensions;
- `list` and `status` human/versioned JSON output;
- create calls shared up orchestration;
- cancellation and operation recovery.

**Gate:** fixture checkout survives create/up/down/up and status requires one Docker list query.

### Chunk 6: Agent build pipeline and secure provisioning

- agent `version`, `identity`, `provision`, and environment-capture commands;
- shared build ID/protocol;
- musl artifacts and xtask staging/embedding;
- staging archive upload;
- root ownership/mode provisioning with no container tools;
- always-reinstall behavior;
- effective Dev Container environment capture;
- Debian/Alpine and custom-user tests.

**Gate:** up securely provisions and verifies the agent on supported architectures/images.

### Chunk 7: SSH identity and generated configuration

- client and per-workspace host Ed25519 keys;
- strict permissions and stable known-host entries;
- explicit per-workspace SSH blocks;
- safe executable/root path rendering;
- consented, idempotent global Include insertion;
- `cdenv ssh` explicit-config wrapper;
- `ssh -G` tests.

**Gate:** generated OpenSSH resolution is correct without wildcard ProxyCommand substitution.

### Chunk 8: Agent SSH handshake, authentication, and exec

- generic stdio async stream;
- Russh server and host key loading;
- exact user/public-key authentication;
- environment snapshot/allowlist;
- session state and non-PTY shell `-c` execution;
- stdout/stderr/exit reporting;
- bounded channel I/O and strict stderr logging.

**Gate:** protocol harness observes exact output separation and exit status.

### Chunk 9: Host proxy through Bollard

- brief shared lifecycle coordination;
- strict container/provision/build drift checks;
- attached agent Exec as remote user/workspace;
- binary-clean forwarding and cancellation;
- concise proxy diagnostics and bounded connection logs.

**Gate:** system OpenSSH runs a remote command with no exposed port or persistent cdenv process.

### Chunk 10: PTY, multiplexing, signals, and cleanup

- isolated minimal-unsafe Linux PTY module;
- login shell/session/process groups;
- terminal modes and resizing;
- signal mapping;
- HUP/TERM/KILL cleanup;
- detached helper behavior;
- ControlMaster concurrent channels.

**Gate:** automated PTY/multiplexing suite passes on Debian and Alpine.

### Chunk 11: Direct forwarding and `cdenv forward`

- unrestricted `direct-tcpip`;
- concurrent bidirectional TCP bridges;
- system OpenSSH foreground wrapper;
- multiple mappings, loopback default, non-loopback warnings;
- service and network-bind integration tests.

**Gate:** host and explicitly selected host-network interfaces reach container services without Docker port publication or rebuild.

### Chunk 12: Rebuild and replacement recovery

- cached rebuild through `up --remove-existing-container`;
- V1 `--no-cache` plumbing;
- desired config behavior;
- uncommitted-change notice;
- replacement ID validation;
- complete reprovision and stable host identity;
- interrupted rebuild/drift recovery tests.

**Gate:** tracked/untracked changes survive, container ID changes, and SSH reconnects without host-key warnings.

### Chunk 13: OpenSSH compatibility hardening

- complete independent release-gate helper/suite;
- long-lived binary channels;
- keepalive/global request behavior;
- concurrent exec/PTY/forward channels;
- reconnect and cancellation edge cases;
- bounded buffering/throughput checks measured in release mode.

**Gate:** Section 17.5 passes without an editor dependency.

### Chunk 14: Doctor, packaging, CI, and release

- read-only `doctor` and JSON envelope;
- all four host artifacts with both embedded agents;
- Linux architecture integration matrix;
- macOS Docker Desktop smoke checklist;
- minimum and pinned Dev Container CLI jobs;
- checksums, installation docs, upgrade/state migration docs;
- troubleshooting and compatibility notes.

**Gate:** one installed host binary can create, provision, connect, forward, stop, restart, and rebuild a workspace.

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

- source changes survive stop/start/rebuild except changes intentionally made by repository lifecycle code;
- cdenv itself never mutates Git state after clone;
- no `sshd` or SSH container port is required;
- no cdenv daemon persists after connection/forward commands end;
- the agent runs in Debian and Alpine on x86_64 and arm64;
- effective remote-user environment is available to SSH sessions;
- live status derives from one Docker query where practical;
- duplicate/drift conditions fail safely instead of selecting arbitrary containers;
- Compose workspaces isolate projects and access only their primary development service;
- host and agent upgrades cannot silently mix incompatible protocols;
- all cdenv-managed host state remains under the configured root except the explicitly consented SSH Include line;
- proxy stdout is proven binary-clean;
- the automated editor-independent OpenSSH compatibility suite passes;
- release binaries contain both verified static agent artifacts;
- strict Rust lint, documentation, security, test, and unsafe-code policies pass.
