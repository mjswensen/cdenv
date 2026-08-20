---
id: 27
created: 2026-08-20
---

# Implement frozen Feature locks and `cdenv lock`

_Converted from [`implementation-chunks/27-feature-lockfile-and-lock-command.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §10.10 and §11.5](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#1010-lock)
**Depends on:** 12, 14, 19, and 26

## Goal

Make Feature resolution reproducible while preserving the rule that only explicit `cdenv lock` writes into a checkout.

## Work

- Parse/generate deterministic adjacent `devcontainer-lock.json` content covering configured Features, recursive dependencies, normalized versions, digests, and integrity.
- Resolve tags/dependencies through allowed sources, apply deterministic ordering, and validate a present lock as frozen for create/rebuild.
- Implement policy results: stale/inconsistent lock fails create/rebuild with guidance; existing-container `up` warns without registry access; missing lock allows build with reproducibility warning; verified digest cache can satisfy frozen locks offline.
- Implement `cdenv lock [--config]` under the exclusive workspace lock. `--config` chooses only the target and must not change desired workspace intent.
- Canonicalize the adjacent target inside checkout, refuse symlink/non-regular targets, preserve a reasonable existing mode, and atomically replace via same-directory `create_new` temp file.
- Never stage/commit the lock or mutate any other checkout/Git path.

## Rust guidance

Load the `rust-best-practices` skill. Keep lock validation pure, isolate the one mutation boundary, use typed staleness/integrity errors, and favor small structural assertions over giant snapshots.

## Acceptance criteria

- Repeated lock generation is byte-identical and round-trips all dependency/options/digest data.
- Tests cover stale config, changed dependency/options, corrupt integrity, missing lock, offline frozen cache, and unlocked offline failure.
- Filesystem tests prove only the exact lockfile changes, modes are preserved, symlinks/escapes fail, and desired state remains unchanged.
- A guard test snapshots the checkout before/after every non-`lock` cdenv test and detects no cdenv-owned write.
- Standard workspace quality commands pass.