---
id: 64
created: 2026-09-01
---

# Add coverage reporting to release CI

**Split from:** 52

## Goal

Make tested-code concentration visible without imposing an arbitrary coverage percentage threshold.

## Work

- Install and run `cargo-llvm-cov` in CI against the locked workspace.
- Collect informational function and branch reports for ordinary workspace tests and both named release suites.
- Keep release-suite collection subject to the existing required-dependency and nonempty-suite rules.
- Upload machine-readable and human-readable reports as CI artifacts with deterministic names and bounded retention.
- Document the reviewed baseline/report interpretation and how maintainers investigate coverage regressions.

## Acceptance criteria

- CI produces workspace and release-suite function/branch coverage reports from a clean locked checkout.
- Reports are retained as CI artifacts and do not hide a failed, skipped, or zero-test release suite.
- Coverage is informational; no unreviewed percentage threshold is introduced.
- The workflow and documentation identify report locations and the reviewed baseline.
