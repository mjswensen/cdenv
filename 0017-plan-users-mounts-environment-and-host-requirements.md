---
id: 17
created: 2026-08-20
---

# Plan users, mounts, environment, and host requirements

_Converted from [`implementation-chunks/17-users-mounts-environment-and-host-requirements.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.2, §11.4, and §11.7](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#114-docker-and-build-options)
**Depends on:** 16

## Goal

Model the effective workspace/runtime inputs that do not require executing Docker.

## Work

In `cdenv-devcontainer` add pure validated models and defaulting for:

- authoritative container workspace folder/mount and additional mounts, including source/target/options and conflicts with cdenv-owned targets;
- `containerUser`, `remoteUser`, Compose/image metadata defaults, and an explicit UID/GID-update intent for later image generation;
- `containerEnv`, `remoteEnv`, `userEnvProbe`, and runtime-only `${containerEnv:...}` requirements without persisting effective values;
- `overrideCommand`, `init`, privileged mode, capabilities, security options, devices, and other supported create/runtime settings not owned by Docker argument parsing;
- CPU, memory, storage, and GPU host requirements evaluated against injected measured/unknown capabilities.

Hard-fail host requirements only when reliable evidence proves they are unmet; warn for unmeasurable requirements and optional GPU, and never add GPU access unless requested.

## Rust guidance

Load the `rust-best-practices` skill. Use validated newtypes at security boundaries, borrow collections in validation, avoid redundant clones, and model measured/unknown outcomes with enums rather than booleans.

## Acceptance criteria

- Tests cover workspace defaults for all scenarios, mount conflicts/containment, user precedence, UID-update decisions, environment stage boundaries, and probe variants.
- Host-requirement tests distinguish met, provably unmet, unknown, required GPU, and optional GPU outcomes.
- No effective environment or identity-sensitive host value appears in serializable plan summaries or error snapshots.
- Unsupported/conflicting settings produce exact property-path errors.
- Pure crate tests remain Docker-free and standard workspace quality commands pass.