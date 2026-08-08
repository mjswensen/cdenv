# ADR 0001: Accept the `cdenv-devcontainer-v1` feasibility gate

- **Status:** Accepted
- **Date:** 2026-08-08
- **Decision owners:** cdenv maintainers
- **Scope:** Implementation-plan feasibility Chunks 00–04

## Decision

The feasibility gate passes on the declared environment. The packet flow,
profile risks, Docker/Compose ownership boundaries, lifecycle recovery,
forwarding supervisor, and two-architecture agent build are feasible.
Production Chunk 1 (implementation chunk 05) is authorized to start, subject to
the pins and compatibility contract in this ADR and the
[`cdenv-devcontainer-v1` support matrix](../cdenv-devcontainer-v1-support.md).

The code under [`spikes/feasibility/`](../../spikes/feasibility/) remains
disposable and quarantined. Production crates may migrate fixtures, snapshots,
and black-box behavior but must not depend on spike modules or preserve their
abstractions merely because they worked here.

## Pinned specification inputs

The profile is named `cdenv-devcontainer-v1` and is pinned to:

- Dev Container specification repository commit
  [`c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421`](https://github.com/devcontainers/spec/commit/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421)
  (2026-03-20);
- unmodified `devContainer.base.schema.json`, SHA-256
  `a0883c0405ff433db188849d458fb20b9c0d73e0ba1a6e44c1d83f3b485408dd`;
- the aggregate schema and Feature schema from that same commit;
- the aggregate schema's mutable VS Code `main` references closed for audit at
  VS Code commit
  [`eb55ea151447d7607654d7046a18b37d3dca4704`](https://github.com/microsoft/vscode/commit/eb55ea151447d7607654d7046a18b37d3dca4704).

Runtime validation uses the pinned base schema and never follows network schema
references. Exact provenance is in
[`vendor/PROVENANCE.md`](../../spikes/feasibility/vendor/PROVENANCE.md), and all
13 vendored inputs are covered by
[`SHA256SUMS`](../../spikes/feasibility/SHA256SUMS).

The upstream repository did not contain a reusable test-fixture directory at
this revision. The four local fixture families therefore have cdenv provenance
and explicit behavior statements; they are not represented as upstream
fixtures.

## Verified dependency and environment baseline

The following is the **minimum verified baseline for initial production CI**.
It is deliberately conservative: lower releases may work, but none were tested
by this gate and must not be declared supported without compatibility evidence.

| Component | Minimum verified / selected version | Decision |
|---|---:|---|
| Docker Engine | 29.6.2, API 1.55 | Initial minimum verified daemon. Local Unix socket only. |
| Docker CLI | 29.7.1 | Initial minimum verified CLI for pull/build/create and direct start/stop command evidence. |
| Docker Compose V2 | 5.3.1 | Initial minimum verified Compose. Compose V1 is unsupported. |
| Docker Buildx / BuildKit | 0.36.0 / 0.31.2 | Selected cross-build/build matrix tool and builder. Legacy classic builder is unsupported. |
| Bollard | **0.21.0 exact** | Volatile production candidate pin; generated stubs are 1.53.1-rc.29.3.1 and default client API is 1.53. |
| Russh | **0.62.5 exact** | Volatile production candidate pin with `default-features = false`, `ring`. |
| JSONC parser | jsonc-parser 0.33.1 exact in spike | Audited parser candidate; comments enabled and trailing commas disabled. |
| JSON Schema | jsonschema 0.49.7 exact in spike | Offline base-schema validation candidate. |
| HTTP/TLS | reqwest 0.13.4 / rustls 0.23.43 exact in spike | Default certificate and hostname verification; no custom bypass. |
| PTY | pty-process 0.5.3 exact in spike | Safe PTY/open/resize/process API; the spike contains no unsafe code. |
| Rust | 1.97.1, edition 2024 | Verified spike toolchain, not yet the production workspace toolchain ADR. |
| OpenSSH client | 10.0p2 | Verified interoperability baseline. |
| Host | Docker Desktop aarch64 | Native arm64 plus BuildKit/QEMU amd64 execution. |

Production may choose compatible requirements for stable crates after Chunk 05
policy work, but Bollard and Russh remain exact pins until their adapter APIs and
compatibility jobs authorize an upgrade.

### Selected volatile APIs

**Bollard 0.21.0**

- `Docker::connect_with_socket`, `ping`, `version`, label-filtered
  `list_containers`, and `inspect_container` establish one Unix endpoint and
  verify CLI/Compose results.
- `create_exec` with `models::ExecConfig` and `start_exec` with
  `StartExecOptions` return `StartExecResults::Attached { output, input }`.
- `input` is an async writer. Writes and protocol output must be flushed
  incrementally; waiting for process completion before flush deadlocks an SSH
  key exchange.
- `output` is decoded as `container::LogOutput::StdOut`/`StdErr`; `StdIn` or
  `Console` on a non-TTY attach is a protocol error. Docker multiplex headers
  must never be copied as bytes.
- Attached-stream cancellation must race input EOF against output completion so
  dropping a killed ProxyCommand closes the Docker attach rather than waiting
  forever.

**Russh 0.62.5**

- `russh::server::run_stream` successfully serves `tokio::io::join(stdin,
  stdout)`; no TCP listener or `sshd` is required.
- `server::Config` fixes public-key-only methods, exact host key, bounded channel
  queues, authentication attempts, keepalive, and no server inactivity timeout.
- `server::Handler` callbacks proven are public-key offer/signature auth,
  session channel open, environment allowlist, PTY, shell/exec,
  `channel_open_direct_tcpip`, and window/signal channel messages.
- `Channel::split`, regular and extended-data writers, `exit_status`, `eof`, and
  `close` preserve exact stdout/stderr/status. PTY input uses `pty-process`; all
  non-PTY commands run in a new process group for disconnect cleanup.
- A client channel EOF closes command stdin but is not itself a disconnect;
  channel close/transport loss triggers process-group termination. This
  distinction is mandatory for commands that continue after stdin closes.

These APIs are volatile seams. Production adapters must have direct integration
tests before either exact version changes.

## Profile and orchestration decisions

1. The official `@devcontainers/cli` is not a runtime or CI dependency.
2. Parsing, bounds, property rejection, staged substitutions, `${devcontainerId}`
   generation, and property-specific metadata merge are fixed by the support
   matrix and reviewed snapshots.
3. Docker owns Dockerfile parsing, `.dockerignore`, context, BuildKit, and
   non-reserved Docker-shaped options. cdenv rejects options that compete for
   files/tags/outputs/labels it owns.
4. Feature sources are limited to public anonymous OCI, unauthenticated verified
   HTTPS, and contained local directories. Digests and bounded safe extraction
   are mandatory. Private sources, URL credentials, insecure HTTP, and TLS
   bypass are out of V1.
5. A Feature receives one generated root layer. A later generated UID/GID layer
   works on Debian and Alpine for amd64 and arm64.
6. Docker Buildx is the selected practical dual-agent build mechanism from this
   environment. It produced runnable, interpreter-free static musl ELF
   artifacts for both architectures while compiling the Russh dependency graph.
   Production `xtask` may wrap this proven containerized build; adopting
   `cross` or Zig later requires equivalent artifact and runtime checks.
7. Compose always receives explicit ordered files, project identity, and a
   canonical JSON override. The primary service is discovered exactly; the full
   managed set is persisted. Ordinary resume/start and stop use persisted
   container IDs and do not invoke broad Compose reconciliation or `down`.
8. Lifecycle checkpoints distinguish pending, running/indeterminate, and
   succeeded. Definite pending work can retry; interrupted one-time running work
   requires rebuild. A successful checkpoint is never duplicated.
9. Declared forwarding belongs to a detached, workspace/generation-scoped host
   supervisor with a private control socket and 0600 token. It retains the
   listener across temporary target loss and exits only after authenticated
   teardown. There is no host reboot service in V1.
10. SSH remains system OpenSSH over ProxyCommand and Docker Exec. There is no
    container listener, `sshd`, port 22 publication, or permanent SSH agent.
    A ControlMaster owns one stdio server and multiplexes exec, PTY, and
    `direct-tcpip` channels.
11. An explicitly detached remote helper may survive only after creating its
    own session and redirecting all inherited descriptors. Its PID/ownership
    must be recorded and teardown must terminate it; ordinary command children
    are reaped as a process group.

## Feasibility criterion resolution

Every implementation-plan §5.3 criterion is resolved as passed:

| Criterion | Result and reproducible evidence |
|---|---|
| Pinned schema and deterministic merge/plans | Pass — baseline checksum/schema check, byte-stable [`expected/`](../../spikes/feasibility/expected/) snapshots, and [`profile-integration.sh`](../../spikes/feasibility/scripts/profile-integration.sh). |
| Stable labels on CLI/Compose primary containers | Pass — Bollard verification records for image and two Compose identities in [`docker-compose-integration.sh`](../../spikes/feasibility/scripts/docker-compose-integration.sh). |
| Dockerfile context, BuildKit, options, reserved rejection | Pass — repository fixture plus Debian/Alpine matrix in [`docker-build-matrix.sh`](../../spikes/feasibility/scripts/docker-build-matrix.sh); focused reserved-option test. |
| Public OCI bearer/digest/lock without Node | Pass — GHCR challenge, manifest/blob verification, corrupt-cache rejection, canonical lock snapshot, and quarantine scan in [`profile-integration.sh`](../../spikes/feasibility/scripts/profile-integration.sh). |
| Feature/UID images on supported architectures/distros | Pass — Debian 13 and Alpine 3.22 on amd64 and arm64, one layer per Feature, UID/GID 1234 in [`docker-build-matrix.sh`](../../spikes/feasibility/scripts/docker-build-matrix.sh). |
| Compose override/discovery/isolation | Pass — two project/network/volume/container identities and exact workspace/dependency discovery in [`docker-compose-integration.sh`](../../spikes/feasibility/scripts/docker-compose-integration.sh). |
| Lifecycle checkpoint/wait/cancellation/background recovery | Pass — parallel timing, deterministic coordinator order, wait-stage forwarding, no-op repeat, indeterminate cancellation, and safe pending retry in the Compose gate. |
| Declared forwarding survives `up` and is removed by `down` | Pass — short-lived launcher, target stop/restart recovery, wrong-token rejection, authenticated cleanup in the Compose gate. |
| Binary-clean, bidirectional, cancellation-aware Bollard Exec | Pass — 4 MiB stdout and extended-data payload hashes, long stream, input/output flush, killed-client process cleanup in [`ssh-integration.sh`](../../spikes/feasibility/scripts/ssh-integration.sh). |
| Docker framing never reaches SSH stdout | Pass — exact stdout/stderr/status and binary hashes through the full route. |
| Russh generic stdio stream | Pass — `server::run_stream` over joined process stdio in every OpenSSH case. |
| OpenSSH auth and command | Pass — strict test host key, exact Ed25519 user/key, negative host/client keys, command/cwd/status checks. |
| ControlMaster concurrent sessions | Pass — four concurrent exec channels on one observed `ssh-server` Exec plus a no-session master. |
| PTY, resize, Ctrl+C, direct TCP | Pass — tmux-backed 80×24 to 101×43 resize, interrupt marker/exit, and OpenSSH local direct forwarding. |
| Cleanup without permanent daemon | Pass — killed command descendants are reaped, master exit removes `ssh-server`, no `sshd` executable/process or published port exists. |
| Dual static agent artifacts | Pass — [`agent-cross-build.sh`](../../spikes/feasibility/scripts/agent-cross-build.sh) builds, inspects, and executes static musl amd64/arm64 artifacts with Buildx. |

The complete command is
[`scripts/gate.sh`](../../spikes/feasibility/scripts/gate.sh). Declared
integration dependencies fail rather than skip.

## Important findings and encountered issues

The spike exposed the following implementation constraints. Resolved items are
requirements for production, not incidental workarounds.

1. **Mutable schema closure.** The selected aggregate schema references VS Code
   `main`. Vendoring only the Dev Container commit would not be immutable. The
   editor schemas were separately pinned, while behavioral validation uses the
   upstream base schema.
2. **OCI authorization scope.** GHCR requires the anonymous Bearer token on blob
   requests as well as the manifest retry. Omitting it yields HTTP 401 after a
   successful manifest fetch.
3. **Actionable TLS errors.** A top-level reqwest display string can hide the
   certificate cause. Error rendering must retain and print the source chain.
4. **Hostile archive fixtures.** Modern `tar` APIs refuse construction of parent
   traversal paths, so the negative fixture must form a raw valid header. The
   production extractor still validates before extraction and extracts regular
   entries before validated symlinks.
5. **Docker Desktop sibling mounts.** A path visible inside a development
   container is not necessarily a Docker-host path. Integration detected the
   current container's bind source before mounting repository/agent files into
   sibling containers. Production normally runs on the host, but development
   tooling must account for this boundary.
6. **Shared-volume Unix sockets.** Docker Desktop shared mounts rejected Unix
   control sockets/ControlMaster links (`EINVAL`/bad file descriptor). Private
   runtime sockets must live on a native local filesystem with 0700 parent
   permissions, never in a cross-OS shared checkout.
7. **Bollard packet flushing.** Buffered Docker stdout and unflushed attached
   stdin caused OpenSSH to stall at `SSH2_MSG_KEX_ECDH_REPLY` and Russh to hit a
   keepalive timeout. Every protocol frame requires incremental write and flush;
   Docker multiplex frames must be decoded first.
8. **SSH EOF versus disconnect.** OpenSSH sends channel EOF as soon as command
   stdin closes. Treating that as disconnect raced short commands, produced
   status 129, lost output, and could orphan a background `sleep`. Continue
   reading for channel close while shutting and dropping only the command-stdin
   pipe; retaining the closed handle prevented EOF and deadlocked streamed input.
9. **Process-group cleanup.** Non-PTY commands need a new session/process group.
   On close, terminate the group with bounded HUP/TERM/KILL escalation. Test the
   descendant, not only the shell leader.
10. **Build tooling availability.** `cross`, Zig, standalone QEMU tools, and
    `cargo-deny` were not preinstalled. Docker Buildx already exposed amd64 and
    arm64 and successfully built the static agents, so it is selected for the
    initial agent build. `cargo-deny` remains mandatory from production Chunk
    05; its absence here is an environment/tooling issue, not a waived
    production check.
11. **Classic Docker builder.** The verified Docker release is BuildKit-first and
    does not provide a supported classic-builder gate. V1 explicitly requires
    BuildKit rather than pretending untested classic behavior is portable.
12. **Mutable public tags.** The public Feature fixture resolves a tag into a
    reviewed manifest/blob lock. If the upstream tag moves, the gate fails
    closed until maintainers review and update the lock evidence; it never
    silently blesses the new artifact.

## Security and retained evidence

The gate uses generated, test-only SSH/TLS keys under ignored runtime paths and
contains no retained credentials. Feature tests are anonymous. Token files are
0600, control directories are 0700, listener binds are loopback, and negative
TLS/key/token cases fail closed. The quarantine check scans retained files for
credential-like material and confirms that normal scripts do not invoke Node.

Retained inputs are fixture definitions, provenance/checksums, canonical plans,
Feature lock/evidence snapshots, deterministic command templates, this ADR,
the support matrix, and executable black-box gates. Runtime logs, certificates,
keys, binaries, cache entries, container IDs, assigned ports, and Docker-host
paths remain below ignored `target/` and are not evidence committed to Git.

## Consequences

- Production starts from behavior contracts and adapter seams proven here, not
  from copied spike architecture.
- Initial CI must provide the conservative verified Docker/Compose/OpenSSH and
  Buildx environment. Lower-version support requires a compatibility matrix and
  ADR update.
- BuildKit, Compose V2, public/unauthenticated Features, and system OpenSSH are
  deliberate V1 boundaries.
- Runtime Unix sockets must use private native host paths.
- Any semantic profile change follows the support matrix's versioning policy.
- If a future run fails a §5.3 criterion, production work stops; the criterion
  cannot be waived merely by editing this ADR.
