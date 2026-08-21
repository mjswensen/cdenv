---
id: 59
created: 2026-08-21
---

# Make integration test entry points reject empty suites

**Parent phase:** Implementation plan §17.4–17.6
**Blocks:** 49 and 50

## Goal

Ensure an integration command cannot report success when it discovered or executed no tests.

## Work

- Make `cargo xtask test-integration` explicitly fail or report unavailable until a real suite is selected and discovered.
- Add the planned `--suite devcontainer-v1` and `--suite openssh` interface without allowing unknown or empty suites.
- Record and validate discovered/executed test counts for declared integration runs.
- Distinguish an intentionally unavailable development environment from a declared CI integration environment; declared missing Docker/Compose/OpenSSH dependencies must fail.
- Keep ordinary `cargo test --workspace --locked` Docker-free.

## Acceptance criteria

- [ ] The current zero-test integration package cannot produce a successful integration-gate result.
- [ ] Unknown suite names, suites with zero discovered tests, and suites with all tests skipped fail clearly.
- [ ] Each valid suite reports a positive discovered and executed count.
- [ ] Declared CI runs fail when required external dependencies are missing.
- [ ] README and xtask help describe the exact availability/failure behavior.
- [ ] Standard workspace quality commands pass.
