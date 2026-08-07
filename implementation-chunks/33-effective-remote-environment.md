# 33 — Capture and apply the effective remote environment

**Parent phase:** [Implementation plan §11.7 and Chunk 8](../implementation-plan.md#117-effective-remote-environment)  
**Depends on:** [16](16-image-metadata-and-substitution.md), [17](17-users-mounts-environment-and-host-requirements.md), and [32](32-agent-tool-free-provisioning.md)

## Goal

Give lifecycle and SSH processes the Dev Container user environment without persisting or leaking it on the host.

## Work

- Have the staging/final agent inspect the actual container environment, resolve the effective remote user, and run the configured `userEnvProbe` as that user.
- Apply supported runtime `${containerEnv:...}` substitutions and merge `remoteEnv` according to the effective plan.
- Encode the environment in a restricted, binary-safe container-side format that supports non-UTF-8 Unix names/values where valid.
- Remove transient/session values before reuse: `PWD`, `OLDPWD`, `SHLVL`, `_`, and SSH session entries.
- Expose safe agent APIs to load the snapshot for lifecycle and SSH child processes.
- Capture once for readiness work and recapture after the selected lifecycle readiness stage for SSH sessions.
- Never print values or persist snapshots/resolved environment on the host; diagnostics may name invalid keys but not values.

## Rust guidance

Load the `rust-best-practices` skill. Work with `OsStr`/byte-oriented values where required, borrow environment entries, avoid lossy conversion/cloning, and use typed probe/encoding errors.

## Acceptance criteria

- Tests cover every supported probe mode, user selection, precedence, runtime substitution, non-UTF-8 values, filtering, and malformed snapshots.
- Lifecycle child tests observe the expected effective values while excluded transient values are absent.
- A secret-marker scan of host state, stdout/stderr, and logs finds no environment values.
- Snapshot files are container-only, restricted, atomically replaced, and generation-scoped.
- Debian/Alpine fixtures and all standard workspace quality commands pass.