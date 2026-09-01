---
id: 64
created: 2026-09-01
---

# Add coverage reporting to release CI

**Split from:** 52
**Depends on:** 66

## Goal

Make tested-code concentration visible without imposing an arbitrary coverage percentage threshold.

## Work

- Pin/install and run `cargo-llvm-cov` in CI against the locked workspace.
- Collect informational function and branch reports for ordinary workspace tests and both named release suites after issue 66 establishes their required matrix jobs.
- Keep release-suite collection subject to the same required-dependency, architecture, and nonempty-suite rules as the release gates; coverage must not replace or weaken those jobs.
- Upload mergeable machine-readable data plus human-readable reports as CI artifacts with deterministic matrix-qualified names and bounded retention.
- Document the reviewed baseline/report interpretation, exclusions, and how maintainers investigate coverage regressions.

Issue 66 owns which release jobs are required and whether they pass. This issue owns only coverage instrumentation, report merging/presentation, and artifact retention.

## Acceptance criteria

- CI produces workspace and per-release-suite function/branch coverage reports from clean locked checkouts on the supported release matrix.
- Reports are retained with deterministic names and bounded retention and do not hide a failed, skipped, missing-dependency, or zero-test release suite.
- Coverage is informational; no unreviewed percentage threshold is introduced.
- The workflow and documentation identify report locations, merge/exclusion behavior, tool version, and the reviewed baseline.
