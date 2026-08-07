# 42 — Implement SSH identities, generated config, and `cdenv ssh`

**Parent phase:** [Implementation plan §14 and Chunk 11](../implementation-plan.md#14-ssh-identity-and-configuration)  
**Depends on:** [10](10-atomic-storage-and-installation-identity.md), [13](13-git-and-create-transaction.md), [32](32-agent-tool-free-provisioning.md), and [36](36-create-and-up-reconciliation.md)

## Goal

Create stable OpenSSH-compatible identity/configuration without wildcard ProxyCommand substitution or editor-specific behavior.

## Work

- Generate one installation-wide unencrypted Ed25519 client key and one persistent host key per workspace using a maintained implementation compatible with pinned Russh.
- Maintain strict modes and stable `known_hosts` entries. Provision the workspace host private key and exact authorized client public key on every successful `up`.
- Generate one explicit `Host <name>.cdenv` block per workspace with the exact options in §14.2. Safely render absolute executable/root/key paths; reject unrepresentable inputs.
- Resolve the executable from `argv[0]` via `PATH` without dereferencing the final stable symlink; use `current_exe()` only as fallback.
- Regenerate managed config atomically during successful mutating flows.
- Implement remembered Include consent: prompt only for unknown+TTY, insert before first `Host`/`Match`, preserve formatting/mode, refuse unsafe/symlinked user config, and honor explicit global flags.
- Implement `cdenv ssh` as system `ssh -F <managed-config> <host> ...`, preserving argv and exit status.

## Rust guidance

Load the `rust-best-practices` skill. Use typed key/config/escaping errors, avoid copying private key bytes, document security invariants, and snapshot only small generated blocks.

## Acceptance criteria

- Key tests verify format, exact client/host roles, stable host key across rebuild, strict permissions, and rejection of wrong keys/modes.
- `ssh -G -F <config> <name>.cdenv` resolves the expected explicit ProxyCommand/options for paths containing supported spaces/quotes.
- Tests prove no `Host *.cdenv` block or `%h` workspace substitution exists.
- Include tests cover insertion position, idempotence, accepted/declined/noninteractive/TTY states, symlinks, and manual instructions.
- `cdenv ssh` works without user Include and propagates fake/system SSH status; standard quality commands pass.