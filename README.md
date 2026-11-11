# cdenv

`cdenv` creates Docker-backed development environments from Git repositories and connects to them through your system OpenSSH client. It keeps project tools and dependencies in containers while letting you use an SSH-based terminal or editor workflow.

Environments use the repository's Dev Container configuration, including supported image, Dockerfile, Compose, Feature, and lifecycle settings. See the [support matrix](docs/cdenv-devcontainer-v1-support.md) for the exact supported subset.

## Install

Supported hosts are Linux x86_64, Linux arm64, and Apple Silicon macOS. Install Docker, Docker Compose V2, OpenSSH, and Git first; the [operations guide](docs/operations.md#supported-hosts-and-exact-dependency-baseline) lists verified dependency versions.

```bash
curl -fsSL https://github.com/mjswensen/cdenv/releases/latest/download/install.sh | sh
cdenv doctor
```

The installer verifies the release archive's SHA-256 checksum and places `cdenv` in a writable directory on `PATH`. See [installation options](docs/operations.md#install-root-selection-and-uninstall) for a custom destination or pinned version.

## Use

Clone a repository into a managed checkout and start its environment:

```bash
cdenv create https://github.com/example/project.git --name project
cdenv ssh project
```

Replace the example URL with your repository. `create` clones and starts the environment; use `up` to start it again after stopping it.

```bash
cdenv list
cdenv status project
cdenv forward project 3000:3000  # Forward localhost:3000; Ctrl+C stops forwarding
cdenv down project             # Stop the environment, retaining checkout and volumes
cdenv up project
cdenv rebuild project          # Apply changes to the environment configuration
```

`cdenv` asks before adding an Include to your SSH config. You can decline and still use `cdenv ssh`. Host Git credentials, SSH-agent access, and Git author identity are independently opt-in through [`cdenv credentials`](docs/operations.md#host-credential-permissions-issue-68).

Repository configuration and lifecycle commands run trusted code with Docker-user authority. Review repositories before starting their environments. The [operations guide](docs/operations.md) covers configuration, forwarding, credentials, and recovery.

## Develop

Install [mise](https://mise.jdx.dev/), then prepare the pinned toolchain and run the local quality gate:

```bash
mise install
mise bootstrap
cargo xtask check
```

The gate runs formatting, Clippy, workspace tests, documentation builds, and dependency checks. Workspace tests do not require Docker or OpenSSH. Run the environment-backed suites explicitly:

```bash
cargo xtask test-integration --suite devcontainer-v1
cargo xtask test-integration --suite openssh
```

See [integration tests](tests/integration/README.md) for prerequisites and suites, and [release packaging](docs/release-packaging.md) for building and verifying release archives.
