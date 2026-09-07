---
id: 75
created: 2026-09-07
depends-on:
  - 71
  - 72
  - 73
---

# Implement separately enabled missing-field Git author identity defaults

**Split from:** 68, primarily section 5 and the non-destructive managed-process requirements.

## Goal

Implement `git-identity` as separately consented defaults for genuinely missing Git `user.name` and `user.email`, without overriding existing identity or enabling authentication/signing capabilities.

## Work

- Read only host `user.name` and `user.email` from issue 72's trusted neutral host Git configuration context during explicit reconciliation. Do not evaluate container-writable checkout configuration on the host or copy the host `.gitconfig`.
- Carry only the typed identity metadata through issue 71's verified transport. Missing host values are independently unavailable/advisory and must not block readiness.
- Extend issue 73's owned integration mechanism with reversible defaults of the correct precedence. Fill each genuinely absent field independently while preserving container/system/global/local Git identity, conditional configuration, command/command-environment overrides, and explicit author/committer environment values.
- Treat values as data, not shell fragments. Do not use a blanket high-precedence name/email override merely because it is easier to inject.
- Refresh on explicit reconciliation and remove only cdenv-owned defaults on disable for future invocations. Existing commits or already-running processes cannot have metadata recalled.
- Report configuration and availability through issue 76 without displaying identity values by default. Granting identity must not enable HTTPS, SSH-agent, GPG, signing, or Docker credentials.

## Acceptance criteria

- [ ] A matrix of missing name, missing email, both missing, and both present proves independent fill-only-missing behavior; explicitly configured values are not mistaken for missing values.
- [ ] Conditional/local/global/system identity, Git command overrides and `GIT_CONFIG_*`, and `GIT_AUTHOR_*`/`GIT_COMMITTER_*` overrides retain their effective meaning.
- [ ] Host values containing spaces, quotes, shell metacharacters, or invalid control input cannot become executable fragments or escape the supported data/config grammar.
- [ ] Enable/refresh/disable changes only cdenv-owned defaults; existing user/repository files remain byte-for-byte unchanged, and later invocations regain underlying behavior after disable.
- [ ] Missing host name/email remains advisory. Status/debug/log output does not dump identity metadata by default, and no environment/configuration wholesale copy is persisted.
- [ ] Tests prove `user.signingKey`, `commit.gpgsign`, signing helpers, GPG keys/trust, and all other credential capabilities are unaffected.
- [ ] Fake host configuration and controlled container Git fixtures cover the integration without live credentials/keychains; `cargo xtask check` passes. Issue 77 owns production lifecycle/SSH enrollment and issue 78 owns packaged end-to-end evidence.

## Implementation guidance

Load the Rust best-practices skill. Reuse the neutral host configuration boundary and non-destructive integration owner; helper replacement precedence and identity default precedence are different requirements and must not be conflated.
