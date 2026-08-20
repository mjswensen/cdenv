---
id: 36
created: 2026-08-20
---

# Implement shared `create`/`up` reconciliation

_Converted from [`implementation-chunks/36-create-and-up-reconciliation.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §10.1, §10.4, §11.8, and Chunk 9](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#104-up)
**Depends on:** 13, 20, 25, 27, 30, and 35

## Goal

Make `create` and `up` converge a workspace safely without implicit rebuild.

## Work

- Under the exclusive workspace lock, discover/validate desired config, persist explicit desired selection before lifecycle execution, inspect live Docker truth, and compute category drift.
- Run command-specific dependency/profile preflight before image work. Feature source access must follow lock policy; ordinary existing-container `up` must not contact registries merely to detect drift.
- Create a missing image/Compose environment, directly start unchanged stopped image containers, and drift-safely resume recorded Compose containers.
- Warn and continue with the active generation on build/create drift; apply independently valid runtime drift through an injected forwarding/environment seam. Invalid desired config fails before mutation but leaves healthy active data usable by non-mutating commands.
- For already-running environments, remain idempotent while always reverifying/reprovisioning cdenv-owned assets and verifying an existing lifecycle runner.
- Commit the new `active` record atomically only after complete readiness; `create` calls this same internal flow after clone.
- Reject retry after failed/indeterminate one-time lifecycle work with rebuild guidance.

## Rust guidance

Load the `rust-best-practices` skill. Use ownership to make desired/active commit boundaries clear, static fakes for adapters, typed reconciliation outcomes, and no catch-all orchestration error.

## Acceptance criteria

- Component tests cover missing/running/stopped/duplicate/external-replaced/Docker-unavailable environments for image and Compose.
- Drift tests prove no implicit rebuild, category-specific warnings, valid runtime application, and unchanged active generation on invalid desired config/failure.
- Repeated `up` reuploads/provisions assets but does not repeat one-time lifecycle commands.
- `create` post-clone failures retain checkout/state and plain safe retry behaves exactly as documented.
- Docker integration fixtures survive `create/up/down/up`; standard workspace quality commands pass.