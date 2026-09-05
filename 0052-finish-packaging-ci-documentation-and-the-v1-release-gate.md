---
id: 52
created: 2026-08-20
depends-on:
  - 49
  - 50
  - 51
---

# Finish cross-platform release packaging

_Originally converted from [`implementation-chunks/52-packaging-ci-docs-and-release.md`] on 2026-08-20. Coverage, mutation testing, and CI/documentation work are tracked separately in 64, 65, and 66._

**Parent phase:** [Implementation plan §17.7 and Chunk 18](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-18-doctor-packaging-ci-and-release)

**Blocks:** 66

## Goal

Produce verifiable host release archives for every supported host platform.

## Work

- Make `cargo xtask dist` produce one reproducible archive for its host target, and add a release matrix that builds macOS arm64/x86_64 and Linux arm64/x86_64 archives. Every host must embed both verified Linux musl agents with one shared build ID.
- Verify exact archive contents, adjacent checksums, host executable format/architecture, embedded agent ELF architecture/static linkage, artifact IDs/protocols, version output, and the absence of empty or unexpected files.
- Add a package-only smoke test that extracts each archive and exercises the host binary's version and embedded-artifact validation paths for both agent architectures.

The package smoke test ends at binary/artifact validation. Issue 66 owns installed operational workflows, Docker/OpenSSH smoke coverage, and the assertions that no `sshd` or published port 22 is used.

## Acceptance criteria

- The release matrix runs `cargo xtask dist` from clean locked checkouts and produces all four checksum-verified host archives.
- Each archive contains exactly one executable for its declared host platform; deterministic release/checksum metadata is emitted alongside it.
- Each extracted host binary reports its version and validates both embedded Linux agent artifacts with the shared build ID and protocol.
- Package tests fail on missing, empty, unexpected, wrong-format, wrong-architecture, or checksum-mismatched host artifacts, and on dynamically linked or otherwise invalid Linux agent artifacts.
