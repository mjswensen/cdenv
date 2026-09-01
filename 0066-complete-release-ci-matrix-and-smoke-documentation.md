---
id: 66
created: 2026-09-01
---

# Complete release CI matrix and smoke documentation

**Split from:** 52

## Goal

Enforce the supported runtime matrix and document the operational V1 release contract.

## Work

- Configure CI for strict Rust quality checks, `cargo-deny`, and the pinned/minimum Docker and Compose jobs.
- Enforce complete nonempty Linux x86_64/arm64 Dev Container and OpenSSH release-suite matrices; missing declared dependencies must fail.
- Add a recorded macOS Docker Desktop smoke checklist for Apple Silicon and Intel when hardware is available. Editor observations remain non-gating notes.
- Complete documentation for installation/uninstallation, root override, dependency baselines, profile revision/support matrix, lock policy, state migrations/upgrades, trust and source sanitization, lifecycle/recovery, forwarding exposure, SSH Include consent, troubleshooting, and retention cleanup.
- Document and run a clean-install smoke workflow covering the applicable §19 commands and V1 non-goals.

## Acceptance criteria

- CI enforces the required quality and Linux architecture/version matrix from a clean locked checkout.
- Both release suites remain nonempty and fail for missing Docker, Compose, or OpenSSH dependencies.
- macOS smoke results record hardware, Docker Desktop version, date, and outcome before release.
- Documentation covers the listed operational/security topics and promises no behavior beyond `cdenv-devcontainer-v1`.
- The clean-install smoke workflow requires neither Node.js nor an editor and verifies no `sshd` or published port 22.
