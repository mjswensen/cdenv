# cdenv Implementation Plan

**Project name:** `cdenv`  
**Project website:** `cdenv.sh`  
**Meaning:** Containerized Development Environment  
**Primary implementation language:** Rust  
**Primary platform target:** Local Docker on macOS and Linux  
**Workspace format:** Development Containers (`devcontainer.json`)  
**Docker integration:** Bollard  
**Editor integration:** Standard SSH only; editors are not launched automatically

---

## 1. Executive Summary

`cdenv` is a CLI-only tool for creating, running, rebuilding, listing, and connecting to local Docker-backed development containers.

The preferred workflow is:

```bash
cdenv create https://github.com/example/project.git
cdenv list
cdenv up project
ssh project.cdenv
```

A user may create multiple independent workspaces from the same repository by supplying a distinct name:

```bash
cdenv create https://github.com/example/project.git --name project-feature-a
cdenv create https://github.com/example/project.git --name project-feature-b
```

Each workspace owns:

- a Git checkout under `~/.cdenv/workspaces/<workspace-name>/repo/`;
- local metadata under `~/.cdenv/workspaces/<workspace-name>/state.json`;
- a Docker dev container created through the official Dev Container CLI;
- an injected, statically linked Rust SSH agent;
- a stable SSH hostname such as `<workspace-name>.cdenv`.

The container does **not** need to run OpenSSH `sshd` or expose port 22. Instead:

1. OpenSSH resolves `<workspace-name>.cdenv` using a generated `ProxyCommand` rule.
2. The `ProxyCommand` starts `cdenv proxy <workspace-name>` on the host.
3. The host uses Bollard to create and attach to a Docker Exec process.
4. Docker Exec runs `cdenv-agent ssh-server --stdio` inside the container.
5. SSH protocol bytes flow end-to-end over stdin/stdout.
6. The agent implements SSH sessions, PTYs, command execution, signals, and TCP forwarding.

The repository should be a Cargo workspace producing two binaries:

```text
cdenv          # host CLI
cdenv-agent    # Linux binary copied into containers
```

The host binary should embed Linux x86_64 and arm64 agent artifacts so installation requires only one host executable.

---

## 2. Goals

### 2.1 Primary goals

1. Provide a simple CLI for local Docker-based development containers.
2. Prefer a repository-first workflow through `cdenv create`.
3. Keep every project checkout, key, configuration file, log, and state file under `~/.cdenv/`.
4. Use the official Dev Container CLI as the reference implementation for `devcontainer.json` behavior.
5. Use Bollard for Docker Engine communication.
6. Expose each running workspace through a standard SSH interface.
7. Support any editor capable of opening an SSH remote.
8. Avoid requiring `sshd`, exposed ports, or a persistent daemon inside the container.
9. Preserve repository changes when stopping, starting, or rebuilding a workspace.
10. Keep the implementation modular enough that a coding agent can complete it in small, independent chunks.

### 2.2 Secondary goals

- Support macOS on Apple Silicon and Intel.
- Support Linux on x86_64 and arm64.
- Support glibc- and musl-based Linux containers through static agent binaries.
- Produce useful diagnostics without corrupting the SSH byte stream.
- Make workspace discovery resilient to container replacement during rebuilds.
- Allow multiple simultaneous SSH clients and editor sessions.

---

## 3. Non-goals for the Initial Release

The first release should deliberately exclude:

- remote Docker hosts;
- Kubernetes or cloud providers;
- automatic editor launching;
- reimplementation of the Dev Container specification;
- Windows host support;
- Windows containers;
- Git hosting authentication management beyond using the user's existing Git configuration and credentials;
- background workspace synchronization;
- workspace snapshots;
- SFTP unless an explicitly targeted editor requires it;
- SSH agent forwarding;
- reverse remote forwarding;
- Docker Compose support beyond what the Dev Container CLI already handles;
- a long-running `cdenv` daemon;
- arbitrary repository management commands beyond clone and preserving the checkout.

---

## 4. External Dependencies

The host system must provide:

- Docker Engine or Docker Desktop;
- the official `devcontainer` CLI;
- Git;
- an OpenSSH-compatible `ssh` client for direct terminal use and most editor integrations.

The `cdenv` binary should detect these dependencies and emit actionable errors.

Example:

```text
error: the Dev Container CLI was not found
install it, then retry: npm install -g @devcontainers/cli
```

Do not hard-code one installation method into the implementation. Error messages may mention common installation approaches, but dependency detection should only require that the executable is discoverable on `PATH`.

---

## 5. High-Level Architecture

```text
┌─────────────────────────────┐
│ User or SSH-capable editor  │
└──────────────┬──────────────┘
               │ ssh workspace.cdenv
               ▼
┌─────────────────────────────┐
│ Local OpenSSH client        │
│ ~/.cdenv/ssh/config           │
│ ProxyCommand cdenv proxy %h   │
└──────────────┬──────────────┘
               │ raw SSH bytes over stdio
               ▼
┌─────────────────────────────┐
│ cdenv host CLI                │
│ resolves workspace          │
│ connects to Docker/Bollard  │
└──────────────┬──────────────┘
               │ Docker Exec attach
               ▼
┌─────────────────────────────┐
│ cdenv-agent in container      │
│ ssh-server --stdio          │
├─────────────────────────────┤
│ SSH authentication          │
│ Session channels            │
│ PTYs and shell execution    │
│ Non-PTY exec requests       │
│ Window resize and signals   │
│ direct-tcpip forwarding     │
└─────────────────────────────┘
```

### 5.1 Process lifetime

There is no persistent SSH server process.

Each SSH connection creates:

- one local `cdenv proxy` process;
- one Docker Exec instance;
- one `cdenv-agent ssh-server --stdio` process inside the container.

When the SSH connection closes, all three terminate.

Multiple clients create independent agent processes.

---

## 6. Cargo Workspace Layout

```text
cdenv/
├── Cargo.toml
├── Cargo.lock
├── README.md
├── LICENSE
├── crates/
│   ├── cdenv-cli/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── commands/
│   │       │   ├── create.rs
│   │       │   ├── up.rs
│   │       │   ├── down.rs
│   │       │   ├── rebuild.rs
│   │       │   ├── list.rs
│   │       │   ├── status.rs
│   │       │   ├── ssh.rs
│   │       │   ├── proxy.rs
│   │       │   └── doctor.rs
│   │       ├── app.rs
│   │       ├── paths.rs
│   │       ├── git.rs
│   │       ├── devcontainer.rs
│   │       ├── docker.rs
│   │       ├── workspace_store.rs
│   │       ├── workspace_status.rs
│   │       ├── agent_install.rs
│   │       ├── ssh_config.rs
│   │       ├── keys.rs
│   │       └── output.rs
│   │
│   ├── cdenv-agent/
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs
│   │       ├── config.rs
│   │       ├── stdio_stream.rs
│   │       ├── server.rs
│   │       ├── auth.rs
│   │       ├── channel_state.rs
│   │       ├── session.rs
│   │       ├── process.rs
│   │       ├── pty.rs
│   │       ├── signals.rs
│   │       └── forwarding.rs
│   │
│   └── cdenv-core/
│       ├── Cargo.toml
│       └── src/
│           ├── lib.rs
│           ├── workspace.rs
│           ├── status.rs
│           ├── platform.rs
│           ├── naming.rs
│           └── error.rs
│
├── xtask/
│   ├── Cargo.toml
│   └── src/main.rs
├── tests/
│   ├── fixtures/
│   └── integration/
└── .github/
    └── workflows/
        ├── ci.yml
        └── release.yml
```

### 6.1 Recommended crate responsibilities

#### `cdenv-core`

Contains shared, platform-neutral data structures and validation logic:

- workspace metadata;
- workspace name validation;
- status enums;
- architecture enums;
- error types that are useful across crates;
- serialization formats.

It must not depend on Bollard, Russh, or CLI presentation code.

#### `cdenv-cli`

Contains all host-side operations:

- command parsing;
- paths and storage;
- Git clone;
- Dev Container CLI invocation;
- Docker access through Bollard;
- workspace lifecycle;
- agent installation;
- SSH configuration;
- proxy transport;
- user-facing output.

#### `cdenv-agent`

Contains Linux-only SSH server behavior:

- public-key authentication;
- shell and exec requests;
- PTY creation;
- signal propagation;
- resizing;
- TCP forwarding;
- strict stdout discipline.

#### `xtask`

Contains repository build and distribution automation:

- build static Linux agents;
- build host binaries;
- embed or stage agent artifacts;
- create release archives;
- run compatibility test matrices.

---

## 7. Filesystem and State Layout

All persistent `cdenv` data must live under:

```text
~/.cdenv/
```

No state should be written to `~/.config`, `~/.local/share`, system directories, or project directories outside `~/.cdenv`.

Recommended layout:

```text
~/.cdenv/
├── config.toml
├── workspaces/
│   ├── project/
│   │   ├── repo/
│   │   ├── state.json
│   │   └── logs/
│   │       ├── create.log
│   │       ├── up.log
│   │       ├── rebuild.log
│   │       └── proxy.log
│   └── project-feature-a/
│       ├── repo/
│       ├── state.json
│       └── logs/
├── ssh/
│   ├── config
│   ├── known_hosts
│   ├── id_ed25519
│   ├── id_ed25519.pub
│   └── host_keys/
│       ├── project_ed25519
│       └── project-feature-a_ed25519
├── agents/
│   ├── cdenv-agent-linux-x86_64
│   └── cdenv-agent-linux-aarch64
├── cache/
│   └── devcontainer/
├── logs/
│   └── cdenv.log
└── tmp/
```

### 7.1 Workspace directory

Each workspace has a stable directory:

```text
~/.cdenv/workspaces/<workspace-name>/
```

The repository is cloned into:

```text
~/.cdenv/workspaces/<workspace-name>/repo/
```

The workspace name is the stable user-facing identifier used by:

```bash
cdenv up <workspace-name>
cdenv rebuild <workspace-name>
cdenv down <workspace-name>
ssh <workspace-name>.cdenv
```

### 7.2 State file

Suggested initial schema:

```json
{
  "schemaVersion": 1,
  "name": "project-feature-a",
  "repositoryUrl": "https://github.com/example/project.git",
  "repositoryDirectory": "/Users/example/.cdenv/workspaces/project-feature-a/repo",
  "createdAt": "2026-08-01T00:00:00Z",
  "lastUpAt": "2026-08-01T00:02:00Z",
  "containerId": "optional-last-known-container-id",
  "containerLabels": {
    "cdenv.workspace": "project-feature-a"
  },
  "remoteUser": "vscode",
  "remoteWorkspaceFolder": "/workspaces/project",
  "containerArchitecture": "aarch64",
  "agentPath": "/usr/local/libexec/cdenv-agent",
  "statusHint": "running"
}
```

The state file is a cache and user-facing record, not the sole source of truth for container status. `cdenv` must re-check Docker whenever live status matters.

### 7.3 Atomic writes

All state and configuration updates should use atomic replacement:

1. write to a temporary file in the same directory;
2. flush and sync when practical;
3. rename over the destination.

This prevents partially written state after interruption.

---

## 8. Workspace Naming Rules

A workspace name must:

- be unique within `~/.cdenv/workspaces/`;
- contain lowercase ASCII letters, digits, and hyphens only;
- begin and end with a letter or digit;
- be safe for use as a directory name, Docker label value, and SSH hostname prefix;
- have a documented maximum length, recommended at 63 characters.

Examples:

```text
valid:
project
project-feature-a
api2

invalid:
Project
project_feature
../project
project.cdenv
```

### 8.1 Default repository-derived name

When `--name` is omitted, derive the name from the Git URL:

```text
https://github.com/example/project.git  → project
git@github.com:example/project.git       → project
ssh://git@example.com/example/project    → project
```

Then:

1. remove a trailing `.git`;
2. normalize to lowercase;
3. replace invalid runs with `-`;
4. trim leading/trailing hyphens;
5. validate the result.

If the derived name already exists, fail with an actionable error rather than silently adding a suffix:

```text
error: workspace "project" already exists
use --name to create another checkout of the same repository
```

---

## 9. CLI Contract

## 9.1 `cdenv create`

```bash
cdenv create <git-repository-url> [--name <workspace-name>]
```

Preferred workflow. It must:

1. validate dependencies;
2. derive or validate the workspace name;
3. ensure the target workspace does not already exist;
4. create the workspace directory;
5. clone the repository into `repo/`;
6. create initial `state.json`;
7. call the same internal orchestration used by `cdenv up`;
8. install the agent;
9. ensure SSH configuration and keys exist;
10. print the workspace name and SSH destination.

Example output:

```text
Cloning https://github.com/example/project.git
Creating dev container
Installing cdenv agent

Workspace: project
Repository: ~/.cdenv/workspaces/project/repo
Status: running
SSH host: project.cdenv

Connect with:
  ssh project.cdenv
```

### 9.1.1 Clone behavior

Use the system `git` executable instead of implementing the Git protocol in Rust.

Equivalent operation:

```bash
git clone <url> ~/.cdenv/workspaces/<name>/repo
```

Reasons:

- inherits the user's credential helpers;
- supports SSH agents and enterprise Git configurations;
- supports all common Git URL formats;
- avoids embedding credential-management behavior in `cdenv`.

If clone fails:

- preserve logs;
- remove the incomplete workspace directory unless a debug flag says otherwise;
- never leave a workspace that appears valid in `cdenv list`.

### 9.1.2 Repository without devcontainer configuration

If the cloned repository has no detectable `.devcontainer/devcontainer.json`, root `.devcontainer.json`, or other form supported by the reference CLI, return a clear error.

Do not generate a default devcontainer configuration in the initial release.

## 9.2 `cdenv list`

```bash
cdenv list
```

Displays every workspace under `~/.cdenv/workspaces/` and its current status.

Suggested columns:

```text
NAME                 STATUS       REPOSITORY                              SSH HOST
project              running      github.com/example/project             project.cdenv
project-feature-a    stopped      github.com/example/project             project-feature-a.cdenv
broken-demo          unavailable  github.com/example/broken-demo         broken-demo.cdenv
```

Possible statuses:

- `running`: container exists and is running;
- `stopped`: container exists but is stopped;
- `creating`: local state indicates an unfinished creation operation;
- `rebuilding`: local state indicates a rebuild operation in progress;
- `missing`: workspace exists locally but no matching container exists;
- `unavailable`: Docker cannot currently be queried;
- `error`: state is invalid or the last lifecycle operation failed.

The command should:

1. enumerate workspace directories;
2. read state files;
3. query Docker once and correlate containers using stable labels;
4. avoid one Docker round trip per workspace where possible;
5. support `--json` for automation.

Example JSON:

```json
[
  {
    "name": "project",
    "status": "running",
    "repositoryUrl": "https://github.com/example/project.git",
    "sshHost": "project.cdenv",
    "containerId": "abc123"
  }
]
```

## 9.3 `cdenv up`

```bash
cdenv up <workspace-name>
```

Starts or creates the workspace container from the existing repository checkout.

Behavior:

1. validate the workspace exists;
2. verify the repository directory still exists;
3. invoke the Dev Container CLI against that repository directory;
4. discover the resulting container;
5. inspect its architecture, user, and workspace path;
6. install or update the embedded agent;
7. update state atomically;
8. print connection instructions.

The repository checkout must not be recloned or reset.

If the container already exists and is running, the command should be idempotent and still verify that the agent is installed and current.

## 9.4 `cdenv down`

```bash
cdenv down <workspace-name>
```

Stops the current container without deleting the repository checkout.

Default semantics should be **stop**, not remove.

Optional future flags may include:

```text
--remove-container
--remove-volumes
```

Do not implement destructive repository deletion as part of `down`.

## 9.5 `cdenv rebuild`

```bash
cdenv rebuild <workspace-name>
```

Rebuilds the development container using the current repository checkout.

Requirements:

- preserve all tracked and untracked repository changes;
- do not reclone;
- do not reset, clean, stash, or switch branches;
- invoke the Dev Container CLI rebuild flow against the existing `repo/` path;
- expect the container ID to change;
- rediscover the replacement container through labels or Dev Container CLI output;
- reinstall the agent;
- update host-key and known-host handling without causing unnecessary SSH warnings;
- atomically update state.

The repository is host-mounted into the dev container by the Dev Container workflow, so source changes remain in `~/.cdenv/workspaces/<name>/repo/` across rebuilds.

Before rebuilding, print a concise notice if the repository has uncommitted changes, but do not block:

```text
note: repository contains uncommitted changes; they will be preserved
```

This may be detected with:

```bash
git -C <repo> status --porcelain
```

## 9.6 `cdenv status`

```bash
cdenv status <workspace-name>
```

Shows detailed workspace information:

- repository URL;
- local repository path;
- current branch when available;
- container status;
- container ID;
- container architecture;
- remote user;
- remote workspace folder;
- agent version;
- SSH hostname.

Support `--json`.

## 9.7 `cdenv ssh`

```bash
cdenv ssh <workspace-name> [-- <ssh arguments...>]
```

Convenience wrapper around the system SSH client:

```bash
ssh <workspace-name>.cdenv
```

Examples:

```bash
cdenv ssh project
cdenv ssh project -- uname -a
```

This is not the core transport implementation. Editors should connect directly to `<workspace-name>.cdenv` through the generated SSH configuration.

## 9.8 `cdenv proxy`

```bash
cdenv proxy <workspace-host-or-name>
```

Internal command used by OpenSSH `ProxyCommand`.

It must:

1. normalize `project.cdenv` to `project`;
2. resolve workspace metadata;
3. resolve the current Docker container by stable label;
4. verify the container is running;
5. create a Docker Exec instance using Bollard;
6. execute the agent as the configured remote user and workspace directory;
7. attach stdin/stdout/stderr;
8. forward bytes until EOF;
9. exit with a meaningful status.

Absolute requirement:

```text
stdout contains only SSH protocol bytes
```

All errors and logs must go to stderr or a file.

## 9.9 `cdenv doctor`

```bash
cdenv doctor
```

Recommended diagnostic command. It should check:

- `~/.cdenv/` permissions;
- Git availability;
- Dev Container CLI availability and version;
- Docker connectivity through Bollard;
- Docker architecture information;
- OpenSSH availability;
- generated SSH config validity;
- client key presence;
- embedded agent availability for supported architectures.

---

## 10. Dev Container CLI Integration

`cdenv` should call the official Dev Container CLI as a subprocess rather than reimplementing its behavior.

### 10.1 Workspace folder

Every lifecycle invocation should use:

```text
~/.cdenv/workspaces/<workspace-name>/repo/
```

as the workspace folder.

### 10.2 Stable container labels

`cdenv` needs a stable label that survives container recreation conceptually, even though the container itself is replaced.

Recommended label:

```text
cdenv.workspace=<workspace-name>
```

Inject it using a Dev Container mechanism that results in the label being applied to the container, such as appropriate run arguments or supported metadata. Confirm the exact CLI/config mechanism during implementation and encapsulate it behind one module.

Also consume existing labels generated by the Dev Container CLI, but do not make them the only identifier unless their stability has been verified.

### 10.3 Output parsing

Prefer JSON output from the Dev Container CLI where supported.

Capture:

- resulting container ID;
- remote user;
- remote workspace folder;
- lifecycle result and errors.

Treat CLI output parsing as an adapter with integration tests. Do not scatter parsing logic throughout commands.

### 10.4 Invocation wrapper

Create one abstraction:

```rust
trait DevcontainerRunner {
    async fn up(&self, request: UpRequest) -> Result<UpResult>;
    async fn rebuild(&self, request: RebuildRequest) -> Result<UpResult>;
}
```

The production implementation invokes the executable. Tests should be able to supply a fake implementation.

---

## 11. Bollard Docker Integration

Bollard is the only Docker Engine integration layer for the application.

Do not invoke the `docker` CLI for container inspection, status, file upload, or exec operations.

### 11.1 Connection

Support local Docker transports:

- Unix socket on Linux and macOS environments where available;
- Docker Desktop's supported local socket behavior.

Create one `DockerClient` wrapper around Bollard so transport and API details do not leak into command code.

Suggested interface:

```rust
#[async_trait]
pub trait DockerClient {
    async fn ping(&self) -> Result<()>;
    async fn list_cdenv_containers(&self) -> Result<Vec<ContainerSummary>>;
    async fn find_workspace_container(&self, name: &str) -> Result<Option<ContainerSummary>>;
    async fn inspect_container(&self, id: &str) -> Result<ContainerDetails>;
    async fn start_container(&self, id: &str) -> Result<()>;
    async fn stop_container(&self, id: &str) -> Result<()>;
    async fn upload_file(&self, id: &str, destination: &str, file: UploadFile) -> Result<()>;
    async fn exec_attached(&self, request: ExecRequest) -> Result<AttachedExec>;
}
```

### 11.2 Container correlation

Use one Docker list call filtered by the `cdenv.workspace` label when possible.

For `cdenv list`, build a map:

```text
workspace name → current container summary
```

The last-known container ID in state is only a hint.

### 11.3 Agent upload

Use Docker's archive upload API through Bollard rather than relying on commands inside the container.

Upload a tar archive containing:

```text
/usr/local/libexec/cdenv-agent
```

with executable mode `0755`.

This avoids assuming that the container has:

- `install`;
- `cp`;
- `chmod`;
- a shell.

If `/usr/local/libexec` is not writable through archive upload because of container configuration, fall back to a configurable location such as:

```text
/tmp/cdenv/bin/cdenv-agent
```

The state file must record the actual path.

### 11.4 Docker Exec options

The proxy must create an exec request equivalent to:

```text
attach_stdin  = true
attach_stdout = true
attach_stderr = true
tty           = false
user          = <remote user>
working_dir   = <remote workspace folder>
command       = [<agent path>, "ssh-server", "--stdio", ...]
```

Do not allocate a Docker TTY. SSH itself handles per-channel PTYs.

### 11.5 Multiplexed stream handling

Docker may frame stdout and stderr separately for non-TTY exec sessions. The Bollard adapter must:

- decode Docker stream frames;
- write stdout payload bytes exactly to local stdout;
- write stderr payload bytes to local stderr;
- forward local stdin to the exec input;
- propagate EOF and cancellation correctly.

No Docker framing bytes may reach the SSH client.

---

## 12. Embedded Agent Distribution

The host binary should include two static Linux agent builds:

```text
x86_64-unknown-linux-musl
aarch64-unknown-linux-musl
```

Suggested approach:

```rust
static AGENT_X86_64: &[u8] = include_bytes!(env!("CDENV_AGENT_X86_64"));
static AGENT_AARCH64: &[u8] = include_bytes!(env!("CDENV_AGENT_AARCH64"));
```

The build pipeline should stage the agent artifacts before compiling the host binary.

At runtime:

1. inspect container architecture;
2. select matching bytes;
3. create a tar archive in memory;
4. upload with Bollard;
5. verify installation by executing:

```bash
cdenv-agent version
```

The agent must expose a machine-readable version:

```json
{"name":"cdenv-agent","version":"0.1.0","protocolVersion":1}
```

`cdenv up` should reinstall the agent whenever the embedded version differs from the installed version.

---

## 13. SSH Configuration

All SSH material lives under:

```text
~/.cdenv/ssh/
```

Generated config:

```sshconfig
Host *.cdenv
    User cdenv
    ProxyCommand /absolute/path/to/cdenv proxy %h
    IdentityFile ~/.cdenv/ssh/id_ed25519
    IdentitiesOnly yes
    UserKnownHostsFile ~/.cdenv/ssh/known_hosts
    StrictHostKeyChecking yes
    ServerAliveInterval 30
    ServerAliveCountMax 3
```

### 13.1 Main SSH config integration

Because all `cdenv` state must stay under `~/.cdenv`, do not copy configuration fragments elsewhere.

The user still needs OpenSSH to read `~/.cdenv/ssh/config`. Two approaches are acceptable:

1. On first setup, add this one line to the user's regular SSH config:

   ```sshconfig
   Include ~/.cdenv/ssh/config
   ```

2. Alternatively, document editor-specific configuration allowing an explicit SSH config file.

For the initial product, prefer automatically adding the single `Include` line after showing what will be changed. Make the operation idempotent and preserve the user's existing file formatting as much as possible.

The `cdenv`-generated content itself remains entirely under `~/.cdenv/`.

### 13.2 SSH user field

The SSH username in the local config need not match the container user because the agent process is already launched by Docker as the correct remote user.

Use a synthetic stable username such as:

```text
cdenv
```

The SSH server may accept this username and ignore it for OS user selection.

### 13.3 Client key

Generate one installation-wide Ed25519 client key:

```text
~/.cdenv/ssh/id_ed25519
~/.cdenv/ssh/id_ed25519.pub
```

Permissions:

```text
private key: 0600
public key: 0644
```

### 13.4 Host keys

Generate one persistent Ed25519 host key per workspace:

```text
~/.cdenv/ssh/host_keys/<workspace-name>_ed25519
```

This key should remain stable across container rebuilds so editors do not see host-key changes.

The agent needs access to the host private key. For the initial implementation, upload it into a restricted container path during `up` and `rebuild`, for example:

```text
/run/cdenv/ssh_host_ed25519_key
```

If `/run` is unsuitable, use a private directory under the installed agent location.

The corresponding known-host entry should be written to:

```text
~/.cdenv/ssh/known_hosts
```

for:

```text
<workspace-name>.cdenv
```

### 13.5 Authorized key

The agent should accept only the installation-wide `cdenv` client public key.

Upload it alongside agent configuration or pass it as an environment variable to the agent process.

A file is simpler and avoids environment-size and quoting concerns:

```text
/run/cdenv/authorized_key
```

---

## 14. Container Agent Design

Recommended dependencies:

```toml
russh = "<pin-current-compatible-version>"
tokio = { version = "1", features = ["full"] }
nix = { version = "<pin>", features = ["process", "signal", "term", "user", "fs"] }
bytes = "1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
tracing = "0.1"
tracing-subscriber = "0.3"
thiserror = "2"
anyhow = "1"
```

Prefer direct Linux PTY handling through `nix` over a cross-platform PTY abstraction because the agent only targets Linux and must correctly handle:

- process groups;
- session leadership;
- controlling terminals;
- UID/GID inheritance;
- signals;
- window resizing;
- terminal modes.

### 14.1 Agent commands

```bash
cdenv-agent version
cdenv-agent ssh-server --stdio \
  --host-key /run/cdenv/ssh_host_ed25519_key \
  --authorized-key /run/cdenv/authorized_key \
  --workspace /workspaces/project
```

The host launches the process as the Dev Container remote user through Docker Exec.

### 14.2 Stdio transport

Wrap Tokio stdin and stdout in one type implementing `AsyncRead + AsyncWrite`.

Conceptually:

```rust
struct StdioStream {
    stdin: tokio::io::Stdin,
    stdout: tokio::io::Stdout,
}
```

Russh should run one SSH server connection over this stream rather than opening a TCP listener.

### 14.3 Logging discipline

The agent must never log to stdout.

Configure tracing to stderr or to a file:

```rust
tracing_subscriber::fmt()
    .with_writer(std::io::stderr)
    .init();
```

### 14.4 Authentication

Accept only:

- the synthetic SSH username expected by `cdenv`;
- public-key authentication;
- the exact `cdenv` installation public key.

Reject passwords and keyboard-interactive authentication.

### 14.5 Required SSH functionality

Initial editor-compatible release:

#### Session channels

- `channel_open_session`;
- environment requests;
- PTY requests;
- shell requests;
- exec requests;
- window-change requests;
- signal requests;
- EOF and close;
- exit status reporting.

#### Forwarding

- `direct-tcpip`.

Later, only if needed:

- SFTP subsystem;
- SSH agent forwarding;
- remote forwarding;
- Unix socket forwarding.

### 14.6 Session state

Each session channel accumulates configuration before process launch:

```rust
struct PendingSession {
    environment: HashMap<String, String>,
    pty: Option<PtyRequest>,
    working_directory: PathBuf,
}
```

Once the client requests `shell` or `exec`, transition to:

```rust
enum ChannelState {
    Pending(PendingSession),
    Running(RunningProcess),
    Forwarding(ForwardedConnection),
}
```

### 14.7 Interactive shell

On PTY plus shell request:

1. determine shell from `/etc/passwd` for the current process UID;
2. fall back to `$SHELL`;
3. fall back to `/bin/sh`;
4. open a PTY pair;
5. set dimensions and terminal modes;
6. create a new session/process group;
7. attach the slave side as controlling terminal;
8. start the login shell;
9. bridge PTY master bytes to the SSH channel;
10. propagate resize and signal requests;
11. report exit status.

Do not assume Bash exists.

### 14.8 Non-PTY command execution

For `exec` without a PTY:

```text
SSH stdin          → child stdin
child stdout       → SSH stdout channel data
child stderr       → SSH extended-data stderr
child exit status  → SSH exit-status request
```

Use the user's shell with `-lc` where appropriate, while taking care not to interpolate the command through an additional unsafe quoting layer.

### 14.9 Signals

Map supported SSH signal names to Unix signals, including at least:

```text
INT
TERM
HUP
QUIT
KILL
USR1
USR2
```

Send signals to the child process group so foreground subprocesses receive them.

### 14.10 Window resizing

Map SSH `window-change` requests to `TIOCSWINSZ` on the PTY master or slave as appropriate.

### 14.11 TCP forwarding

For each `direct-tcpip` request:

1. validate destination host and port;
2. create a Tokio TCP connection from inside the container;
3. bridge it bidirectionally with the SSH channel;
4. close both sides cleanly on EOF or error.

The expected behavior is that forwarded `localhost` refers to the container.

---

## 15. Repository Preservation and Rebuild Semantics

The Git checkout is the durable source of workspace state:

```text
~/.cdenv/workspaces/<name>/repo/
```

Container lifecycle commands must never modify Git state except during initial clone.

Specifically, `up`, `down`, and `rebuild` must not run:

```text
git reset
git clean
git checkout
git switch
git stash
git pull
```

Users retain full control over branches, commits, untracked files, submodules, and remotes.

### 15.1 Rebuild transaction

Suggested rebuild sequence:

1. acquire workspace operation lock;
2. set state operation to `rebuilding`;
3. inspect Git status for informational output;
4. invoke Dev Container CLI rebuild;
5. discover replacement container;
6. inspect remote user, folder, and architecture;
7. upload current agent and SSH assets;
8. verify the agent version;
9. update state with new container ID;
10. mark status hint `running`;
11. release lock.

If rebuild fails, preserve the repository and record an error message in state and logs.

---

## 16. Locking and Concurrent Operations

Prevent conflicting lifecycle operations on the same workspace.

Use a lock file under:

```text
~/.cdenv/workspaces/<name>/.lock
```

Commands requiring exclusive workspace mutation:

- `create`;
- `up`;
- `down`;
- `rebuild`.

Read-only commands such as `list` and `status` may continue but should report an in-progress operation when visible.

The SSH proxy should not hold the lifecycle lock for the entire SSH session. It should only resolve and inspect the current running container.

---

## 17. Error Handling

Define structured errors with `thiserror` and add context at command boundaries.

Suggested categories:

```rust
pub enum CdeError {
    DependencyMissing { name: String },
    InvalidWorkspaceName { name: String, reason: String },
    WorkspaceAlreadyExists { name: String },
    WorkspaceNotFound { name: String },
    RepositoryCloneFailed { url: String },
    RepositoryMissing { path: PathBuf },
    DevcontainerFailed { operation: String, details: String },
    DockerUnavailable { details: String },
    ContainerNotFound { workspace: String },
    ContainerNotRunning { workspace: String },
    UnsupportedArchitecture { architecture: String },
    AgentInstallFailed { details: String },
    SshConfigurationFailed { details: String },
    ProxyFailed { details: String },
    StateCorrupt { path: PathBuf },
}
```

Proxy errors must be concise because editors often display only stderr from the ProxyCommand.

Example:

```text
cdenv: workspace "project" is stopped; run `cdenv up project`
```

---

## 18. Output and UX

Use human-readable tables by default and JSON for automation.

Recommended crates:

- `clap` for CLI parsing;
- `comfy-table` or a similarly lightweight crate for tables;
- `indicatif` only if progress bars do not interfere with logs or automation;
- `serde_json` for `--json` output.

Every mutating command should provide:

- current operation;
- workspace name;
- final status;
- next useful command.

Avoid animated output when stdout is not a TTY.

---

## 19. Security Model

This is a local-development tool. Its primary trust boundary is access to:

- the user's `~/.cdenv/` directory;
- the Docker daemon;
- the `cdenv` executable.

Anyone with Docker daemon access generally has extensive control over local containers and potentially the host.

Still implement SSH authentication and host-key verification to:

- prevent accidental connection to the wrong workspace;
- satisfy normal editor SSH expectations;
- provide stable host identity;
- avoid silently accepting arbitrary local keys.

### 19.1 File permissions

Recommended permissions:

```text
~/.cdenv/                              0700
~/.cdenv/ssh/                          0700
client private key                   0600
workspace host private keys          0600
known_hosts                          0600 or 0644
state files                          0600
logs                                 0600
```

### 19.2 Untrusted repositories

A repository's devcontainer configuration can execute arbitrary code with access permitted by Docker and mounts. `cdenv create` should display a one-time warning or document clearly that running a repository's devcontainer configuration is equivalent to executing its setup code.

Do not attempt to claim sandboxing beyond Docker's configured behavior.

---

## 20. Testing Strategy

## 20.1 Unit tests

Test without Docker where possible:

- workspace name derivation;
- URL parsing;
- state serialization and migration;
- atomic file writes;
- SSH config generation;
- key path generation;
- status correlation logic;
- architecture mapping;
- Dev Container CLI output parsing;
- command validation.

## 20.2 Component tests

### Fake Docker adapter

Test CLI commands using a fake implementation of the Docker abstraction:

- container running;
- container stopped;
- missing container;
- replacement container after rebuild;
- unsupported architecture;
- upload failure;
- exec failure.

### Fake Dev Container runner

Test lifecycle orchestration without invoking the real CLI.

### Agent protocol tests

Start the agent over an in-memory duplex stream or local socket harness and use an SSH client library to test:

- authentication success/failure;
- exec stdout/stderr separation;
- exit codes;
- PTY shell;
- resize;
- Ctrl+C;
- multiple channels;
- direct TCP forwarding.

## 20.3 Integration tests

Create small fixture repositories under `tests/fixtures/`:

```text
basic-debian/
basic-alpine/
custom-remote-user/
forwarding-server/
failing-post-create/
```

Test against real Docker:

1. `cdenv create` from a local bare Git repository URL;
2. `cdenv list` reports running;
3. `ssh workspace.cdenv uname -a` succeeds;
4. interactive shell works;
5. uncommitted file survives rebuild;
6. container ID changes after rebuild;
7. host key remains stable;
8. forwarded port reaches a service inside the container;
9. `down` reports stopped;
10. `up` restarts or recreates successfully.

## 20.4 Compatibility matrix

Host matrix:

- macOS arm64;
- macOS x86_64 when available;
- Linux x86_64;
- Linux arm64.

Container matrix:

- Debian/Ubuntu x86_64;
- Debian/Ubuntu arm64;
- Alpine x86_64;
- Alpine arm64;
- a container using a non-root `remoteUser`;
- a minimal image with `/bin/sh` but without Bash.

Editor smoke tests:

- plain OpenSSH;
- VS Code Remote SSH;
- Zed remote SSH.

---

## 21. Recommended Implementation Sequence

The work is intentionally split into small, self-contained chunks. Each chunk should end with tests and a commit before moving on.

# Chunk 1: Cargo Workspace and Core Models

### Objective

Create the repository skeleton and stable shared data model.

### Build

- root Cargo workspace;
- `cdenv-core`, `cdenv-cli`, `cdenv-agent`, and `xtask` crates;
- shared error and status types;
- workspace name validation and normalization;
- repository URL-to-name derivation;
- state schema version 1;
- basic CLI command enum without implementations.

### Deliverables

- `cargo test --workspace` passes;
- `cdenv --help` lists planned commands;
- unit tests cover valid and invalid workspace names;
- unit tests cover HTTPS, SSH, and SCP-like Git URLs.

### Acceptance criteria

```bash
cargo run -p cdenv-cli -- --help
```

shows:

```text
create
list
up
down
rebuild
status
ssh
proxy
doctor
```

---

# Chunk 2: `~/.cdenv` Paths, State Store, and Locking

### Objective

Implement all local state management before introducing Docker.

### Build

- resolve the user's home directory;
- create the `~/.cdenv/` hierarchy;
- enforce directory and key-file permissions where supported;
- atomic JSON state writes;
- workspace enumeration;
- workspace operation lock;
- corruption-safe state loading;
- minimal global config.

### Deliverables

- `cdenv list` can enumerate mock/local workspace state;
- tests use temporary home directories;
- state writes are atomic;
- concurrent lock acquisition fails cleanly.

### Acceptance criteria

No application-owned state is written outside the configured `cdenv` root.

Allow a hidden test-only override such as `CDENV_HOME`, while production defaults to `~/.cdenv`.

---

# Chunk 3: Git Clone and `cdenv create` Local Workflow

### Objective

Implement repository creation through the system Git executable, without starting a container yet.

### Build

- dependency detection for Git;
- `cdenv create <url> [--name]`;
- clone into `~/.cdenv/workspaces/<name>/repo/`;
- default name derivation;
- duplicate-name error;
- cleanup after failed clone;
- initial state creation;
- repository validation.

### Deliverables

- clone from local and remote URLs;
- tests use a temporary local bare repository;
- repository content appears in the expected directory;
- `cdenv list` shows the newly created workspace as `missing` or `stopped` until container integration exists.

### Acceptance criteria

Creating the same repository twice without `--name` fails and explains how to use `--name`.

---

# Chunk 4: Dev Container CLI Adapter

### Objective

Wrap the official Dev Container CLI behind a testable Rust interface.

### Build

- dependency detection;
- version query;
- `DevcontainerRunner` trait;
- subprocess implementation;
- JSON result parsing;
- log capture;
- `up` and `rebuild` request/result models;
- support adding the `cdenv.workspace` label.

### Deliverables

- fixture integration test that runs `devcontainer up` when the CLI is installed;
- fake runner for unit tests;
- useful errors for missing configuration and failed lifecycle commands.

### Acceptance criteria

The adapter returns container ID, remote user, and remote workspace folder without command modules parsing raw output themselves.

---

# Chunk 5: Bollard Docker Adapter

### Objective

Implement structured Docker access.

### Build

- local daemon connection;
- ping;
- list/filter containers by label;
- inspect container;
- start and stop;
- architecture detection;
- archive upload;
- create and inspect exec instances;
- attached stdin/stdout/stderr streaming abstraction.

### Deliverables

- Docker adapter trait and Bollard implementation;
- fake Docker adapter;
- real integration tests behind an opt-in feature or environment flag;
- test that Docker multiplexing is decoded correctly.

### Acceptance criteria

A test exec such as `cat` can receive bytes on stdin and return identical bytes on stdout through the adapter.

---

# Chunk 6: Workspace Lifecycle: `up`, `down`, `list`, and `status`

### Objective

Connect state, Dev Container CLI, and Bollard into usable lifecycle commands.

### Build

- `cdenv up <name>`;
- `cdenv down <name>`;
- live status correlation;
- `cdenv list` human table and JSON;
- `cdenv status` human and JSON;
- state updates after lifecycle operations;
- stable container rediscovery by label;
- idempotent behavior.

Do not install the SSH agent yet.

### Deliverables

- end-to-end fixture workspace can be created and started;
- list reports running/stopped/missing accurately;
- status reports container details;
- repository changes survive down/up.

### Acceptance criteria

`cdenv create` now automatically invokes the same internal `up` orchestration after cloning.

---

# Chunk 7: Agent Build Pipeline and Installation

### Objective

Produce and install static Linux agent binaries, initially with only `version` and a placeholder stdio command.

### Build

- musl builds for x86_64 and arm64;
- `xtask dist-agent`;
- embedding agent bytes into the host binary;
- container architecture selection;
- tar archive creation in memory;
- Bollard upload;
- executable permissions;
- installed-version check;
- reinstall on mismatch.

### Deliverables

- `cdenv-agent version` returns JSON;
- `cdenv up` installs and verifies the agent;
- tests cover architecture mapping and tar contents.

### Acceptance criteria

Agent installation works in Debian and Alpine fixture containers without invoking `cp`, `chmod`, `install`, or a shell inside the container.

---

# Chunk 8: SSH Keys and Generated Configuration

### Objective

Create stable local SSH identity and workspace host identity.

### Build

- installation-wide Ed25519 client key generation;
- per-workspace Ed25519 host keys;
- dedicated known-hosts entries;
- generated `~/.cdenv/ssh/config`;
- idempotent `Include ~/.cdenv/ssh/config` management;
- upload host key and authorized key to the container;
- strict file permissions.

### Deliverables

- unit tests for config generation;
- repeated setup does not duplicate Include lines;
- host key remains unchanged after repeated `up`;
- known-host entry is stable.

### Acceptance criteria

`ssh -G <workspace>.cdenv` shows the expected ProxyCommand, identity file, and known-hosts file.

---

# Chunk 9: Agent SSH Handshake and Exec Requests

### Objective

Prove the end-to-end SSH transport before implementing PTYs.

### Build

- stdio `AsyncRead + AsyncWrite` wrapper;
- Russh server over stdio;
- server host key loading;
- public-key authentication;
- session channel creation;
- non-PTY `exec` requests;
- stdout/stderr separation;
- exit-status reporting;
- strict stderr logging.

### Deliverables

- in-memory or socket-based protocol tests;
- agent can execute `uname -a` and return its exit code;
- invalid keys are rejected.

### Acceptance criteria

When connected through a test harness, an SSH client can run:

```bash
ssh workspace.cdenv 'printf out; printf err >&2; exit 7'
```

and observe distinct stdout/stderr plus exit code 7.

---

# Chunk 10: Host `proxy` Transport Through Bollard

### Objective

Connect the local OpenSSH client to the agent through Docker Exec.

### Build

- `cdenv proxy` command;
- workspace hostname normalization;
- live container resolution;
- attached exec of `cdenv-agent ssh-server --stdio`;
- stdin forwarding;
- decoded stdout to local stdout;
- decoded stderr to local stderr;
- EOF and cancellation handling;
- useful stopped/missing errors.

### Deliverables

- generated ProxyCommand works with system OpenSSH;
- no framing or log bytes corrupt the SSH handshake;
- proxy exits when SSH disconnects.

### Acceptance criteria

This command succeeds end-to-end:

```bash
ssh project.cdenv uname -a
```

No container port is exposed and no daemon remains running afterward.

---

# Chunk 11: PTY, Interactive Shell, Resize, and Signals

### Objective

Make ordinary terminal SSH sessions behave correctly.

### Build

- Linux PTY allocation with `nix`;
- controlling terminal and process group setup;
- shell detection;
- PTY request parsing;
- data bridging;
- terminal resize;
- signal mapping;
- child exit reporting;
- cleanup on disconnect.

### Deliverables

- `ssh project.cdenv` opens a usable shell;
- `vim`, `top`, or another full-screen program works;
- terminal resizing updates the remote PTY;
- Ctrl+C interrupts the foreground process;
- shell exits cleanly.

### Acceptance criteria

Interactive shell tests pass in both Debian and Alpine containers.

---

# Chunk 12: Direct TCP Forwarding

### Objective

Support editor remote servers and normal SSH local forwarding.

### Build

- Russh `direct-tcpip` handling;
- Tokio TCP connection from inside the container;
- bidirectional bridge;
- cancellation and error handling;
- concurrency tests.

### Deliverables

- fixture starts a TCP or HTTP server inside the container;
- local SSH forwarding reaches it;
- multiple simultaneous forwarded connections work.

### Acceptance criteria

Equivalent functionality works:

```bash
ssh -L 8080:localhost:3000 project.cdenv
```

and local port 8080 reaches port 3000 inside the container.

---

# Chunk 13: `rebuild` Preservation and Replacement-Container Handling

### Objective

Complete safe rebuild semantics.

### Build

- uncommitted-change informational check;
- operation state and logs;
- Dev Container CLI rebuild call;
- replacement container discovery;
- agent and key reinstallation;
- stable SSH host key;
- state update transaction;
- failure recovery.

### Deliverables

- integration test creates tracked and untracked changes;
- rebuild changes container ID;
- all repository changes remain;
- SSH reconnects without host-key warning.

### Acceptance criteria

```bash
cdenv rebuild project
```

preserves the entire repository checkout and leaves the workspace connectable.

---

# Chunk 14: Editor Compatibility and Hardening

### Objective

Validate the intended editor-neutral SSH interface.

### Build

- multiple concurrent SSH channels;
- environment request handling;
- shell startup edge cases;
- long-running connections;
- reconnect behavior;
- proxy stderr logging;
- timeouts and cancellation;
- VS Code Remote SSH smoke test;
- Zed SSH smoke test.

### Deliverables

- compatibility notes in README;
- automated smoke tests where practical;
- fixes for commands and forwarding assumptions used by editors.

### Acceptance criteria

VS Code Remote SSH and Zed can both open the same workspace by hostname without editor-specific code in `cdenv`.

---

# Chunk 15: Doctor, Packaging, CI, and Release

### Objective

Prepare the project for normal installation and troubleshooting.

### Build

- `cdenv doctor`;
- CI formatting, linting, and tests;
- musl agent cross-builds;
- host release builds;
- release archives and checksums;
- installation documentation;
- upgrade behavior;
- state schema migration framework;
- shell completions if desired.

### Deliverables

- release artifacts for supported hosts;
- embedded matching agents;
- CI compatibility matrix;
- troubleshooting guide.

### Acceptance criteria

A user can install one `cdenv` host binary, run `cdenv create`, and connect through SSH without installing any `cdenv` runtime inside the container.

---

## 22. Definition of the Initial Complete Release

The initial release is complete when all of the following work:

```bash
cdenv create https://github.com/example/project.git
cdenv create https://github.com/example/project.git --name project-second
cdenv list
cdenv status project
cdenv down project
cdenv up project
cdenv rebuild project
cdenv ssh project
ssh project.cdenv
ssh -L 8080:localhost:3000 project.cdenv
```

And when:

- source changes survive stop, start, and rebuild;
- containers require no `sshd` and expose no SSH port;
- the agent runs in Debian and Alpine on x86_64 and arm64;
- `cdenv list` accurately reports current status;
- VS Code Remote SSH and Zed can connect through standard SSH;
- all application state remains under `~/.cdenv/`, except the one optional `Include ~/.cdenv/ssh/config` line in the user's standard SSH config;
- one host binary contains the appropriate container agents;
- the implementation does not require a persistent host daemon.

---

## 23. Suggested Initial Dependency Set

Versions should be selected and pinned when implementation begins.

```toml
[workspace.dependencies]
anyhow = "1"
async-trait = "0.1"
bollard = "<pin>"
bytes = "1"
clap = { version = "4", features = ["derive"] }
comfy-table = "<pin>"
fs2 = "<pin>"
futures-util = "<pin>"
nix = { version = "<pin>", features = ["process", "signal", "term", "user", "fs"] }
russh = "<pin>"
russh-keys = "<pin-if-required-by-selected-russh-version>"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
sha2 = "<pin>"
tar = "<pin>"
tempfile = "<pin>"
thiserror = "2"
tokio = { version = "1", features = ["full"] }
tracing = "0.1"
tracing-appender = "<pin>"
tracing-subscriber = "0.3"
uuid = { version = "1", features = ["v4", "serde"] }
which = "<pin>"
zeroize = "<pin>"
```

Use an actively maintained Ed25519/key crate compatible with the selected Russh version for key generation and serialization.

---

## 24. Design Principles for the Implementing Agent

1. **Keep command orchestration separate from adapters.** Commands should coordinate Git, Dev Container, Docker, storage, and SSH modules through interfaces.
2. **Do not parse human-oriented output when JSON is available.**
3. **Treat state as a cache, not live truth.** Docker determines container status.
4. **Never write logs to the SSH stdout stream.**
5. **Never alter the user's Git checkout after clone.**
6. **Prefer one Docker query over N per-workspace queries.**
7. **Use stable labels, not container names or stale IDs.**
8. **Make every mutating operation idempotent or safely retryable.**
9. **Do not require tools inside the container for agent installation.** Use Docker archive APIs.
10. **Pin crate versions and test exact APIs before expanding features.** Russh and Bollard APIs may differ between releases.
11. **Add integration tests at every architecture boundary.** Especially Dev Container CLI parsing, Docker stream framing, PTY behavior, and SSH forwarding.
12. **Do not add editor-specific behavior unless a standard SSH feature is insufficient.**

---

## 25. Open Implementation Decisions to Resolve Early

These should be decided during the relevant implementation chunks and documented in Architecture Decision Records.

1. The exact mechanism used to inject the stable `cdenv.workspace` Docker label through the Dev Container CLI.
2. The exact Russh version and corresponding handler API.
3. Whether host-key and authorized-key files are uploaded under `/run/cdenv`, `/tmp/cdenv`, or beside the agent when filesystems are read-only.
4. Whether the `cdenv` binary edits `~/.ssh/config` automatically or requires the user to add one Include line manually.
5. Whether stopped containers are restarted directly with Bollard or whether every `up` call delegates entirely to the Dev Container CLI.
6. How to handle repositories whose Dev Container configuration uses Docker Compose and produces multiple containers.
7. How much environment filtering is needed for SSH `env` requests.
8. Whether the first release should restrict `direct-tcpip` destinations for security or preserve normal SSH semantics.

Recommended defaults:

- use the Dev Container CLI for every `up` and `rebuild` lifecycle decision;
- support the primary development container only in Compose configurations;
- upload keys beside the agent if `/run` is not writable;
- preserve normal unrestricted `direct-tcpip` behavior for local development;
- make SSH config inclusion automatic but idempotent, with a `--no-modify-ssh-config` escape hatch if needed.

---

## 26. Final End-to-End Example

```bash
$ cdenv create https://github.com/example/project.git
Cloning repository
Creating dev container
Installing cdenv agent
Configuring SSH

Workspace: project
Status: running
Repository: /Users/matt/.cdenv/workspaces/project/repo
SSH host: project.cdenv

$ cdenv list
NAME      STATUS   REPOSITORY                          SSH HOST
project   running  github.com/example/project          project.cdenv

$ ssh project.cdenv
vscode@container:/workspaces/project$ 

$ printf 'local change\n' > ~/.cdenv/workspaces/project/repo/notes.txt

$ cdenv rebuild project
note: repository contains uncommitted changes; they will be preserved
Rebuilding dev container
Installing cdenv agent
Workspace project is running

$ test -f ~/.cdenv/workspaces/project/repo/notes.txt && echo preserved
preserved

$ cdenv down project
Workspace project is stopped

$ cdenv up project
Workspace project is running
Connect with: ssh project.cdenv
```

