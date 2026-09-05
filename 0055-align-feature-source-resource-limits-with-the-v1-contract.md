---
id: 55
created: 2026-08-21
depends-on:
  - 26
---

# Align Feature source resource limits with the V1 contract

**Parent phase:** Implementation plan §11.5 and the `cdenv-devcontainer-v1` support matrix

**Blocks:** 28 and 56

## Goal

Make production Feature download and extraction limits exactly match the frozen V1 compatibility contract.

## Work

- Align defaults with the documented limits: 64 MiB compressed download, 128 MiB expanded data, 4096 entries, and 1 MiB Feature metadata.
- Confirm path, redirect, expansion-ratio, and integer-overflow behavior is explicitly documented and bounded.
- Apply the same limits to OCI, HTTPS, cached, and local Feature paths where the support matrix requires equivalent protection.
- Keep test-only limit injection while preventing production defaults from drifting independently of the support matrix.
- Add a single-source or guard-test mechanism that detects future code/documentation disagreement.

## Acceptance criteria

- [ ] Production constants and support-matrix values agree exactly.
- [ ] Boundary tests accept each exact maximum and reject one unit over it for download bytes, expanded bytes, entry count, and metadata bytes.
- [ ] Checked arithmetic covers cumulative bytes and expansion calculations without overflow.
- [ ] OCI, HTTPS, cached, and local paths exercise the applicable limits.
- [ ] A regression test fails if the published V1 bounds and implementation defaults diverge.
- [ ] Standard workspace quality commands pass.
