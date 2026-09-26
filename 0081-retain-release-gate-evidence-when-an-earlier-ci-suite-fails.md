---
id: 81
created: 2026-09-26
depends-on: []
---

# Retain release gate evidence when an earlier CI suite fails

## Category

CI evidence retention / cascading coverage failures. Blocks issue 78; related to issue 80, but failure-handling improvements can be implemented independently.

## Evidence

CI run [36219272407](https://github.com/mjswensen/cdenv/actions/runs/36219272407), 2026-09-26, source revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`.

In every Linux release matrix leg (x86_64/arm64 × minimum/pinned), the uninstrumented Dev Container suite passes eight tests, then the credential suite fails with the socket-path errors tracked in issue 80. The subsequent ordinary OpenSSH suite, coverage-tool installation, and all three instrumented suites are skipped. Unconditional artifact-retention steps then fail because their output directories were never created:

```text
No files were found with the provided path: target/coverage/devcontainer-v1-linux-x86_64-minimum/. No artifacts will be uploaded.
No files were found with the provided path: target/coverage/credentials-linux-x86_64-minimum/. No artifacts will be uploaded.
No files were found with the provided path: target/coverage/openssh-linux-x86_64-minimum/. No artifacts will be uploaded.
```

The same three errors occur for each of the other matrix legs (12 missing uploads total). See `ci.log:35-139` for step outcomes; upload errors at `11125/11144/11163`, `13966/13985/14004`, `16801/16820/16839`, and `19654/19673/19692`.

The coverage inventory downloads only the three workspace artifacts (`ci.log:19901-19921`) and exits 1 (`19930`). The macOS workspace report also records a failed test. This is downstream incomplete/failed evidence, not proof of an independent coverage-index defect or an OpenSSH test failure.

## Work

- Review sequencing and conditions in `.github/workflows/ci.yml` and outcome/log handling in `.github/scripts/coverage.sh` and `coverage-index.py`.
- Retain original release gate logs and explicit failed/not-run outcomes even when a prerequisite or earlier suite fails before coverage initialization.
- Run independent evidence-producing steps when safe, or record why they could not run. Avoid cascading empty-upload errors that obscure the primary failure.
- Keep the inventory fail-closed: missing, failed, or skipped required suites must not become successful coverage evidence. Do not fix this by simply ignoring missing artifacts or enabling blanket success.
- Add workflow/plumbing regression coverage for an early ordinary-suite failure and failure before coverage tools are installed.

## Acceptance criteria

- [ ] An injected early release-suite failure retains its diagnostics and explicit outcomes for every required matrix/suite entry without misleading empty-upload failures.
- [ ] The originating gate and aggregate inventory remain failed when any required evidence is failed, absent, or not run.
- [ ] A clean successful rerun retains all 12 release-suite reports plus the three workspace reports and passes the existing complete-success inventory contract.
