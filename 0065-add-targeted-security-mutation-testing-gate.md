---
id: 65
created: 2026-09-01
---

# Add targeted security mutation testing gate

**Split from:** 52

## Goal

Ensure focused tests kill meaningful mutations in the security-critical Feature source pipeline.

## Work

- Define a reviewed target list covering Feature-reference validation, OCI authentication/challenge handling, digest and lock validation, and archive extraction/path validation.
- Select and configure a Rust mutation-testing runner that works from a clean locked checkout.
- Record a reviewed baseline of targeted mutants.
- Require each surviving targeted mutant to be killed by a focused test or explicitly triaged with an expiry/reason.
- Run the gate in CI and retain the report as an artifact.

## Acceptance criteria

- The target list and baseline are version-controlled and explain why each module is security-critical.
- CI runs the targeted mutation gate without mutating source state.
- Untreated surviving targeted mutants fail the gate; reviewed triage is explicit and auditable.
- Reports identify the mutant, affected source, focused test or triage rationale, and baseline revision.
