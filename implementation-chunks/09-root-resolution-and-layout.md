# 09 — Resolve the cdenv root and validate managed paths

**Parent phase:** [Implementation plan §7.1–7.2 and Chunk 2](../implementation-plan.md#7-paths-installation-identity-and-permissions)  
**Depends on:** [06](06-core-identities-and-workspace-names.md) and [08](08-cli-contract-and-output-foundation.md)

## Goal

Resolve one validated root and define all managed paths without scattering environment or home-directory logic.

## Work

- Implement precedence `--root` > `CDENV_HOME` > `~/.cdenv`, requiring the explicit forms to meet the main plan’s absolute/UTF-8 rules.
- Represent the result as `CdenvRoot` and derive typed installation, workspace, checkout, state, lock, runtime, log, SSH, cache, blob, generated, and temp paths.
- Enforce the exact host layout, including checkout basename equal to workspace name. Do not create a speculative `config.toml`.
- Validate required cdenv/executable/checkout/config/local-source paths for UTF-8 and reject control/token cases that cannot later be represented safely in OpenSSH configuration.
- Add secure inspection helpers that identify symlinks, non-directories, and ownership mismatches without following or repairing them. Actual atomic file creation is the next chunk.
- Keep root resolution one-time and injectable in tests; command modules must receive it rather than reread globals.

## Rust guidance

Load the `rust-best-practices` skill. Accept borrowed `&Path` inputs, preserve OS errors in typed storage/path errors, and avoid cloning full path trees unnecessarily.

## Acceptance criteria

- Isolated-home tests prove all precedence cases and exact derived paths.
- Tests reject relative explicit roots, non-UTF-8 required paths on Unix, control characters, symlinked managed components, wrong file kinds, and simulated ownership mismatch.
- No test touches the real user home, and no managed path escapes the selected root.
- Root resolution occurs once in application wiring (verified by a fake environment/resolver test).
- Standard workspace quality commands pass.