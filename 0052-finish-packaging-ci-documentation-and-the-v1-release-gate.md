---
id: 52
created: 2026-08-20
---

# Finish packaging, CI, documentation, and the V1 release gate

_Converted from [`implementation-chunks/52-packaging-ci-docs-and-release.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §17.7, Chunk 18, and §19](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-18-doctor-packaging-ci-and-release)
**Depends on:** 49, 50, and 51

## Goal

Produce verifiable host releases for all supported platforms and document the complete V1 contract.

## Work

- Extend `xtask dist` to build macOS arm64/x86_64 and Linux arm64/x86_64 host artifacts, each embedding both verified Linux musl agents with one shared build ID.
- Verify archive contents, executable formats/architectures, embedded artifact IDs/protocols, version output, checksums, and absence of empty/unexpected files.
- Configure CI for all strict Rust checks, `cargo-deny`, minimum/repository-pinned Docker/Compose jobs, and complete Linux x86_64/arm64 profile/OpenSSH matrices. Missing declared dependencies and empty integration suites must fail.
- Add `cargo-llvm-cov` coverage reporting for workspace and release suites. Begin with reviewed informational function/branch reports rather than an arbitrary percentage threshold, and retain reports as CI artifacts so risk concentration is visible.
- Add targeted mutation testing for security-critical Feature reference/authentication/digest/lock and archive-validation logic. Maintain a reviewed target list/baseline and require surviving mutants to be killed by focused tests or explicitly triaged with rationale.
- Add the documented macOS Docker Desktop smoke checklist for Apple Silicon and Intel when available; named editor observations remain non-gating notes.
- Document installation/uninstallation, root override, required/minimum dependencies, profile revision/support matrix, lock policy, state migrations/upgrades, trust model, source sanitization, lifecycle/recovery, forwarding exposure, SSH Include consent, troubleshooting, and cleanup retention.
- Verify every workflow and invariant in [§19](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#19-v1-definition-of-done). Do not add self-update, editor launching, remote Docker, or other V1 non-goals.

## Rust guidance

Load the `rust-best-practices` skill for final review: fix rather than suppress Clippy findings, document public APIs, retain typed errors, benchmark only release builds, and require safety comments for the isolated PTY unsafe code.

## Acceptance criteria

- `cargo xtask dist` produces four checksum-verified host archives, and each installed host binary verifies both embedded agents.
- All five baseline quality commands from 05, both nonempty release suites, package-content checks, coverage collection, and the reviewed targeted mutation gate pass from a clean locked checkout.
- CI enforces the required architecture/version matrix; macOS smoke results are recorded before release.
- Documentation covers every listed operational/security topic and contains no promise beyond `cdenv-devcontainer-v1`.
- A clean-install smoke run completes all §19 command workflows without Node.js, an editor dependency, `sshd`, or published port 22.