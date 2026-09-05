---
id: 65
created: 2026-09-01
depends-on:
  - 62
  - 66
---

# Add targeted security mutation testing gate

**Split from:** 52

## Goal

Ensure focused tests kill meaningful mutations in the security-critical Feature source pipeline.

## Work

- After issue 62 stabilizes the pure Feature boundaries, define a reviewed target list covering Feature-reference validation, OCI authentication/challenge and redirect handling, manifest/blob digest and frozen-lock validation, and archive extraction/path validation.
- Pin and configure a Rust mutation-testing runner that operates in an isolated copy from a clean locked checkout and leaves the source worktree byte-identical.
- Record a reviewed baseline of targeted mutants and a deterministic command/sharding policy suitable for the CI established by issue 66.
- Require each surviving targeted mutant to be killed by a focused test or explicitly triaged with an owner, expiry, and reason.
- Run the gate in CI and retain the report as a bounded artifact.

This is an automated source-mutation gate. It complements rather than duplicates the hand-written malformed-lock and mutation-boundary cases completed by issue 57, and it is independent of the informational coverage reports in issue 64.

## Acceptance criteria

- The pinned runner configuration, target list, command/sharding policy, and baseline are version-controlled and explain why each target is security-critical.
- CI runs the targeted mutation gate in isolation and verifies the checked-out source state is unchanged.
- Untreated surviving targeted mutants, runner errors, empty target selection, and expired triage fail the gate.
- Reports identify the mutant, affected source, focused test or owned/expiring triage rationale, runner version, and baseline revision.
