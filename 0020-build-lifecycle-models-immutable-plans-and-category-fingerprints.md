---
id: 20
created: 2026-08-20
---

# Build lifecycle models, immutable plans, and category fingerprints

_Converted from [`implementation-chunks/20-lifecycle-model-and-immutable-plans.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.6, §11.8, and Chunk 4](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#116-lifecycle-execution-and-recovery)
**Depends on:** 16, 17, 18, and 19

## Goal

Finish the pure planner so adapters consume typed plans rather than raw configuration.

## Work

- Model all lifecycle command forms: shell string, direct argv array, and concurrent keyed object. Merge Feature-contributed commands before repository commands.
- Encode stage order, `waitFor`, initialize/one-time/start/attach semantics, background eligibility, immutable generation command lists, and checkpoint transitions.
- Produce separate immutable build, create, runtime, and lifecycle plans with the exact category ownership in §11.8.
- Keep runtime-only substitutions explicit until actual container environment is supplied. Do not let changed create-time hooks leak into an active generation’s runtime plan.
- Canonically fingerprint each category with the installation keyed digest primitive from 11. Persist only opaque keyed fingerprints, never plan secrets or unkeyed hashes.
- Add pure desired-versus-active drift classification: runtime-applicable, build/create warning, invalid desired, and lifecycle change for next generation.

## Rust guidance

Load the `rust-best-practices` skill. Favor immutable owned plan outputs with borrowed planner inputs, typed errors, static dispatch, descriptive tests, and small reviewed snapshots. Do not invent generic adapter traits in this pure crate.

## Acceptance criteria

- Reviewed fixtures produce stable build/create/runtime/lifecycle snapshots for image, Dockerfile, and Compose scenarios with no I/O.
- Tests prove lifecycle order, parallel grouping, `waitFor`, Feature-before-repository contributions, checkpoint transitions, and immutable active-generation commands.
- Changing one property affects only its documented category fingerprint.
- Secret-marker tests prove serialized state/snapshots expose neither values nor unkeyed digests.
- The full pure-profile gate runs without Docker/network access and all workspace quality commands pass.