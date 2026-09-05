---
id: 50
created: 2026-08-20
depends-on:
  - 41
  - 44
  - 45
  - 46
  - 59
  - 48
---

# Build the OpenSSH interoperability release gate

_Converted from [`implementation-chunks/50-openssh-release-gate.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §17.3, §17.6, and Chunk 17](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#176-automated-openssh-release-gate)

## Goal

Prove standard OpenSSH behavior without `sshd`, editor code, or protocol-stream contamination.

## Work

Automate every gate in §17.6 on Debian and Alpine fixtures:

- valid authentication plus invalid client/host-key rejection;
- exact exec stdout/stderr/status and long-lived binary-clean streams;
- ControlMaster concurrent channels, idle masters, and reconnect;
- PTY shell, resize, Ctrl+C/signals, process cleanup, and detached helper survival;
- direct/local forwarding with concurrent connections;
- declarative supervisor lifetime, alternate assignment, Compose targets, target loss, and recovery;
- foreground ad-hoc forwarding, non-loopback warning, and declared-listener conflicts;
- serialized per-transport attach hooks, including forwarding-only transports;
- disconnect/cancellation and process-group cleanup.

Add malformed protocol/control cases and release-mode bounded-buffer/throughput checks. Measure rather than assert arbitrary performance targets; record regressions against a reviewed baseline.

## Rust guidance

Load the `rust-best-practices` skill, especially testing, performance, async safety, and unsafe documentation. Profile/benchmark only release builds and do not optimize without evidence.

## Acceptance criteria

- `cargo xtask test-integration --suite openssh` passes every §17.6 item on Linux x86_64 and arm64 CI and records a positive discovered/executed test count; zero-test or all-skipped runs fail.
- Debian and Alpine both pass applicable exec, PTY, cleanup, and forwarding cases.
- Protocol stdout remains byte-exact under tracing, hook failure, Docker errors, cancellation, and high concurrency.
- Tests prove there is no published port 22, `sshd`, installation-wide daemon, or permanent agent SSH process.
- Missing OpenSSH/Docker dependencies fail declared jobs; standard workspace quality checks pass.