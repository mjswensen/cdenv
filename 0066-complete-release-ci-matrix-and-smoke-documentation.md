---
id: 66
created: 2026-09-01
depends-on:
  - 52
---

# Complete release CI matrix and smoke documentation

**Split from:** 52

**Blocks:** 64 and 65

## Goal

Enforce the supported runtime matrix and document the operational V1 release contract.

## Work

- Configure CI for strict Rust quality checks, `cargo-deny`, and explicit pinned/minimum Docker Engine/CLI and Compose V2 jobs; do not rely only on whatever versions a hosted runner happens to provide.
- Enforce nonempty Linux x86_64/arm64 Dev Container and OpenSSH release-suite matrices; declared Docker, Compose, OpenSSH, architecture, or fixture prerequisites must fail rather than skip.
- Add a versioned, recordable macOS Docker Desktop smoke checklist for Apple Silicon. Editor observations remain non-gating notes.
- Audit and complete the existing operations/profile documentation for installation/uninstallation, root override, exact dependency baselines, profile revision/support matrix, lock policy, state migrations/upgrades, trust and source sanitization, lifecycle/recovery, forwarding exposure, SSH Include consent, troubleshooting, and retention cleanup.
- Using the archives produced by issue 52, document and run an installed operational smoke workflow covering the applicable §19 commands and V1 non-goals.

Issue 52 owns archive construction, checksums, executable/artifact validation, and package-only smoke tests. This issue owns CI dependency/runtime matrices and installed Docker/OpenSSH workflows. Issues 64 and 65 add observability/security gates after this required workflow is stable.

## Acceptance criteria

- CI enforces the required quality gate plus explicit pinned/minimum dependency jobs and both Linux architecture release-suite jobs from clean locked checkouts.
- Both release suites fail on zero discovered/executed tests and on missing or unsupported Docker Engine/CLI, Compose V2, OpenSSH, architecture, or required fixtures.
- macOS smoke records use the versioned checklist and record host hardware/architecture, OS, Docker Desktop version, cdenv archive/checksum, date, and outcome before release.
- Documentation covers the listed operational/security topics, distinguishes automated guarantees from recorded smoke observations, and promises no behavior beyond `cdenv-devcontainer-v1`.
- The installed smoke consumes a checksum-verified issue-52 archive, covers the applicable §19 workflow, requires neither Node.js nor an editor, and verifies no `sshd` or published port 22.
