# 11 — Implement workspace state, fingerprints, and migrations

**Parent phase:** [Implementation plan §9.1–9.3 and Chunk 2](../implementation-plan.md#9-state-model-atomicity-and-locking)  
**Depends on:** [07](07-core-status-and-errors.md) and [10](10-atomic-storage-and-installation-identity.md)

## Goal

Persist intent and the last successfully provisioned generation without treating state as Docker live truth.

## Work

- Implement a versioned workspace state schema preserving the distinctions in §9.1: installation/workspace identity, sanitized repository source, desired profile/config/fingerprints, timestamps, foreground operation, last sanitized error, and optional active generation.
- Model active scenario/container/image/Compose managed set, build/create/runtime fingerprints, Feature digests, lifecycle checkpoints, forwarding requested/assigned state, and provisioned user/folder/architecture/agent/environment data.
- Do not persist derivable `repositoryDirectory`, free-form `statusHint`, effective environment, substituted build arguments, local environment, or secrets.
- Represent category fingerprints as keyed opaque values. Provide canonical keyed hashing support that can later consume planner inputs without exposing the installation key or an unkeyed digest.
- Reject newer schemas and corrupt state without modification. Add explicit, tested in-memory migration steps for older schemas and report whether a mutating caller should persist the migrated form.
- Ensure desired selection can change independently of `active`, which changes only after complete provisioning success.

## Rust guidance

Load the `rust-best-practices` skill. Use clear runtime enums rather than typestate, typed `thiserror` variants, borrowed inputs for hashing, and small focused serialization/error tests.

## Acceptance criteria

- Reviewed JSON fixtures round-trip every state distinction, including no active generation, Compose, background lifecycle, and degraded forwarding.
- Tests prove newer/corrupt files are not replaced by defaults and old schemas migrate only in memory until explicitly persisted by a mutating path.
- Secret-marker tests scan serialized state and logs and find neither source values nor unkeyed plan hashes.
- Desired config/fingerprints can change while the active record remains byte-for-byte unchanged.
- Atomic persistence uses [10](10-atomic-storage-and-installation-identity.md), and all workspace quality commands pass.