# cdenv

`cdenv` is a host CLI for creating and operating isolated, Docker-backed development environments with system OpenSSH access. The supported Dev Container behavior is defined by the [`cdenv-devcontainer-v1` support matrix](docs/cdenv-devcontainer-v1-support.md).

## Workspace

The Rust workspace contains platform-neutral core types, pure Dev Container profile logic, the host CLI, the Linux-targeted agent, `xtask`, and a non-published integration-test package. The disposable feasibility spike under `spikes/feasibility/` is intentionally excluded. End-user installation, getting started, trust, recovery, forwarding, and macOS smoke-test guidance is in the [operations guide](docs/operations.md).

## Install and get started

On a supported host with Docker, Compose V2, OpenSSH, and Git installed:

```bash
curl -fsSL https://github.com/mjswensen/cdenv/releases/latest/download/install.sh | sh
cdenv doctor
cdenv create https://github.com/example/project.git
cdenv up project
cdenv ssh project
```

Use `cdenv list`, `cdenv status project`, and `cdenv down project` to inspect,
monitor, and stop the environment. See the [operations guide](docs/operations.md)
for dependency versions, installation overrides, SSH consent, and recovery.

## Host credential permissions

`cdenv credentials` manages independently opted-in HTTPS, SSH-agent, and Git
author-identity permissions, including staging before an explicitly named clone.
**This is currently a permission/parser foundation, not working live credential
forwarding.** Production lifecycle command wiring and the credential broker are
still required for issue 68. See the [implemented boundary and command guide](docs/operations.md#host-credential-permissions-issue-68).

## Development

The development container uses the shared [`ghcr.io/mjswensen/devcontainer`](https://ghcr.io/mjswensen/devcontainer) image and runs as its `mjs` user. On creation it installs the tools pinned in `mise.toml` (Python 3.13.15 and Rust 1.97.1) and bootstraps [`cargo-deny`](https://github.com/EmbarkStudios/cargo-deny) 0.20.2 and [Pi](https://pi.dev/). To prepare a local checkout with the same tooling, install [mise](https://mise.jdx.dev/) and run:

```bash
mise install
mise bootstrap
```

Then run the complete local gate:

```bash
cargo xtask check
```

Build a locally verifiable release archive from a clean checkout (Docker Buildx builds and verifies both static Linux agents):

```bash
cargo xtask dist
```

See [release packaging](docs/release-packaging.md) for the three-host matrix,
reproducibility contract, package-only smoke, and checksum-verified installed
Linux workflow. The versioned [macOS Docker Desktop checklist](docs/smoke/macos-docker-desktop-v1.md)
records the Apple Silicon runtime smoke separately.

`cargo xtask check` runs the same baseline commands used by CI:

```bash
cargo fmt --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --locked
cargo doc --workspace --no-deps --locked
cargo deny check
```

Ordinary workspace tests do not require Docker or OpenSSH. Environment-backed tests use an explicit named suite:

```bash
cargo xtask test-integration --suite devcontainer-v1
cargo xtask test-integration --suite openssh
```

A suite fails when it is unknown, unavailable, discovers zero tests, skips tests,
or executes fewer tests than it discovered; it never turns an empty package into
a successful gate. Declared CI architecture and required fixture mismatches also
fail. Missing or below-baseline Docker Engine/CLI, Compose V2, or OpenSSH
prerequisites fail rather than skip.
