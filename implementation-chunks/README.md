# cdenv implementation chunks

These files divide the [main implementation plan](../implementation-plan.md) into focused agent-sized tasks. The main plan remains authoritative for product semantics and security constraints; each task links to the relevant detail.

## How to use these tasks

- Complete a task only after its listed dependencies. Tasks with satisfied dependencies may run in parallel when they do not edit the same area.
- Keep work inside the stated goal. Do not pull later functionality forward or preserve a spike abstraction merely because it exists.
- For Rust work, load and follow the project’s `rust-best-practices` skill. In particular: borrow where ownership is unnecessary, use layered `thiserror` errors, avoid production `unwrap`/`expect`, prefer static dispatch at test seams, document public APIs, and write focused behavior tests.
- Unless a task says otherwise, production changes must keep these commands green:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps
cargo deny check
```

- Docker/OpenSSH tests belong behind the explicit `cargo xtask test-integration` entry point. Declared integration CI must fail rather than skip when required dependencies are missing.
- Finish each task without untracked `TODO` comments. Link unavoidable follow-up notes to an issue.

## Task index

### Feasibility gate

- [00 — Establish the feasibility baseline](00-feasibility-baseline.md)
- [01 — Spike profile planning and Features](01-feasibility-profile-and-features.md)
- [02 — Spike Docker, Compose, lifecycle, and declared forwarding](02-feasibility-docker-compose-lifecycle.md)
- [03 — Spike the OpenSSH-to-agent packet flow](03-feasibility-ssh-packet-flow.md)
- [04 — Record feasibility decisions and close the spike gate](04-feasibility-adr-and-gate.md)

### Workspace, core, state, and Git

- [05 — Scaffold the Rust workspace and quality baseline](05-rust-workspace-and-quality-baseline.md)
- [06 — Implement core identities and workspace naming](06-core-identities-and-workspace-names.md)
- [07 — Model multidimensional status and core errors](07-core-status-and-errors.md)
- [08 — Define the CLI contract and output foundation](08-cli-contract-and-output-foundation.md)
- [09 — Resolve the cdenv root and validate managed paths](09-root-resolution-and-layout.md)
- [10 — Add secure atomic storage and installation identity](10-atomic-storage-and-installation-identity.md)
- [11 — Implement workspace state, fingerprints, and migrations](11-state-schema-fingerprints-and-migrations.md)
- [12 — Add workspace locking, reservation, and read-only enumeration](12-workspace-locking-and-reservation.md)
- [13 — Implement Git handling and the durable create transaction](13-git-and-create-transaction.md)

### Pure Dev Container profile

- [14 — Implement config discovery and bounded JSONC parsing](14-config-discovery-and-jsonc.md)
- [15 — Define and validate the V1 raw profile](15-profile-raw-model-and-validation.md)
- [16 — Implement image metadata merge and staged substitution](16-image-metadata-and-substitution.md)
- [17 — Plan users, mounts, environment, and host requirements](17-users-mounts-environment-and-host-requirements.md)
- [18 — Plan ports and Docker-shaped options](18-ports-and-docker-options-planning.md)
- [19 — Model Features and deterministic dependency ordering](19-feature-model-and-ordering.md)
- [20 — Build lifecycle models, immutable plans, and category fingerprints](20-lifecycle-model-and-immutable-plans.md)

### Docker, Features, and Compose adapters

- [21 — Resolve Docker and build the subprocess foundation](21-docker-endpoint-and-process-runner.md)
- [22 — Implement the specification-facing Docker CLI adapter](22-docker-cli-adapter.md)
- [23 — Implement Bollard discovery, verification, and control](23-bollard-discovery-and-control.md)
- [24 — Implement binary-clean Bollard Exec streaming](24-bollard-exec-streaming.md)
- [25 — Orchestrate image and Dockerfile container creation](25-image-scenario-orchestration.md)
- [26 — Implement secure Feature sources, cache, and extraction](26-feature-sources-cache-and-extraction.md)
- [27 — Implement frozen Feature locks and `cdenv lock`](27-feature-lockfile-and-lock-command.md)
- [28 — Build generated Feature, metadata, and UID/GID images](28-generated-feature-and-uid-images.md)
- [29 — Implement Compose project planning and the V2 adapter](29-compose-planning-and-adapter.md)
- [30 — Manage Compose service sets, isolation, and drift-safe resume](30-compose-managed-lifecycle.md)

### Agent and core lifecycle flows

- [31 — Build, identify, and embed both agent artifacts](31-agent-build-identity-and-embedding.md)
- [32 — Implement tool-free agent identity and provisioning](32-agent-tool-free-provisioning.md)
- [33 — Capture and apply the effective remote environment](33-effective-remote-environment.md)
- [34 — Implement lifecycle execution, checkpoints, and the background runner](34-agent-lifecycle-runner.md)
- [35 — Orchestrate lifecycle stages and readiness on the host](35-host-lifecycle-readiness.md)
- [36 — Implement shared `create`/`up` reconciliation](36-create-and-up-reconciliation.md)
- [37 — Implement `down` and interrupted-operation recovery](37-down-and-operation-recovery.md)
- [38 — Implement live `list`/`status` correlation and rendering](38-list-status-and-output.md)

### Ports and forwarding

- [39 — Integrate `appPort` publication and port attributes](39-app-port-publication.md)
- [40 — Implement the agent bridge and scoped forwarding supervisor](40-agent-bridge-and-forwarding-supervisor.md)
- [41 — Reconcile declared forwarding plans transactionally](41-declarative-forwarding-reconciliation.md)

### SSH and proxy behavior

- [42 — Implement SSH identities, generated config, and `cdenv ssh`](42-ssh-identity-config-and-wrapper.md)
- [43 — Implement agent SSH authentication and non-PTY exec](43-agent-ssh-auth-and-exec.md)
- [44 — Implement the host proxy and per-transport attach hook](44-host-proxy-and-post-attach.md)
- [45 — Add PTYs, signals, cleanup, and OpenSSH multiplexing](45-pty-signals-and-multiplexing.md)
- [46 — Implement `direct-tcpip` and foreground `cdenv forward`](46-direct-tcpip-and-ad-hoc-forward.md)

### Rebuild, compatibility, and release

- [47 — Implement image/Dockerfile rebuild, rollback, and cleanup](47-image-rebuild-rollback-and-cleanup.md)
- [48 — Implement Compose rebuild and partial-state recovery](48-compose-rebuild-and-partial-recovery.md)
- [49 — Build the Dev Container profile release gate](49-devcontainer-profile-release-gate.md)
- [50 — Build the OpenSSH interoperability release gate](50-openssh-release-gate.md)
- [51 — Implement read-only `doctor`](51-read-only-doctor.md)
- [52 — Finish packaging, CI, documentation, and the V1 release gate](52-packaging-ci-docs-and-release.md)
