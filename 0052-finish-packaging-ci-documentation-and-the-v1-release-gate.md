---
id: 52
created: 2026-08-20
---

# Finish cross-platform release packaging

_Originally converted from [`implementation-chunks/52-packaging-ci-docs-and-release.md`] on 2026-08-20. Coverage, mutation testing, and CI/documentation work are tracked separately in 64, 65, and 66._

**Parent phase:** [Implementation plan §17.7 and Chunk 18](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-18-doctor-packaging-ci-and-release)
**Depends on:** 49, 50, and 51

## Goal

Produce verifiable host release archives for every supported host platform.

## Work

- Extend `cargo xtask dist` to build macOS arm64/x86_64 and Linux arm64/x86_64 host artifacts, each embedding both verified Linux musl agents with one shared build ID.
- Verify archive contents, host executable formats/architectures, embedded artifact IDs/protocols, version output, checksums, and the absence of empty or unexpected files.
- Add a clean-install package smoke test proving an extracted host binary validates both embedded agents without Node.js, an editor, `sshd`, or port 22.

## Acceptance criteria

- `cargo xtask dist` produces four checksum-verified host archives from a clean locked checkout.
- Each archive contains exactly the expected host binary and release metadata for its platform.
- Each extracted host binary reports its version and validates both embedded Linux agent artifacts with the shared build ID and protocol.
- Package-content and clean-install tests fail on missing, empty, unexpected, wrong-architecture, dynamically linked, or checksum-mismatched artifacts.
