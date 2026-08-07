# 21 — Resolve Docker and build the subprocess foundation

**Parent phase:** [Implementation plan §4 and §12](../implementation-plan.md#12-docker-cli-compose-and-bollard-adapters)  
**Depends on:** [04](04-feasibility-adr-and-gate.md), [09](09-root-resolution-and-layout.md), and [10](10-atomic-storage-and-installation-identity.md)

## Goal

Guarantee every Docker-facing adapter targets one supported local daemon and every owned subprocess has safe, observable behavior.

## Work

- Resolve one Unix-domain Docker socket at application startup: accept Unix `DOCKER_HOST`, probe documented Docker Desktop/rootless/default paths, and reject TCP, SSH, remote contexts, and unsupported schemes with actionable errors.
- Expose the resolved endpoint to Bollard and inject exactly `DOCKER_HOST=unix://...` into Docker/Compose subprocess environments.
- Implement command-scoped version/capability probes for Docker Engine/CLI and Compose V2 using minimums fixed in the ADR. Accept newer versions unless a capability check fails.
- Build a reusable subprocess runner using direct argv, an explicit cwd/environment, unique operation IDs, bounded stdout/stderr capture, restricted streamed logs, and recognized-value redaction.
- Support process-group cancellation and bounded control/probe timeouts. Do not apply a guessed overall timeout to builds, lifecycle commands, SSH, or intentional background work.
- Return typed parsed process results/errors; never make command modules parse arbitrary output.

## Rust guidance

Load the `rust-best-practices` skill. Use native async APIs/static dispatch at the test seam, layered `thiserror` errors, borrowed argv/path inputs, and no `async_trait`/boxed dispatch without demonstrated need.

## Acceptance criteria

- Resolver tests cover Unix `DOCKER_HOST`, each known path, precedence, absent sockets, and rejection of every remote form.
- Fake executable tests prove exact argv/cwd/environment, no shell, version handling, redaction, bounded capture, and process-group cancellation.
- Docker and Compose probes receive the same endpoint that the Bollard connector exposes.
- Logs are `0600` and secret-marker tests find no supplied environment/header values.
- Standard workspace quality commands pass; opt-in local-daemon tests are clearly separated from ordinary `cargo test`.