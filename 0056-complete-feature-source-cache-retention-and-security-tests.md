---
id: 56
created: 2026-08-21
depends-on:
  - 53
  - 54
  - 55
  - 60
---

# Complete Feature source cache retention and security tests

**Parent phase:** Implementation plan §11.5, §17.2, and Chunk 6

**Blocks:** 28 and 57

## Goal

Finish the unimplemented cache-retention work and supply the focused security tests required by closed issue 26.

## Work

- Implement bounded retention metadata and deterministic cleanup eligibility for verified digest-addressed Feature blobs/extractions.
- Verify every cache reuse by digest and size; reject symlink, non-regular, partial, corrupt, and stale extraction/cache entries.
- Make concurrent cache downloads and extractions converge on one complete result without trusting a pre-existing incomplete destination.
- Add focused mock OCI/HTTPS tests for bearer flow, manifest variants, redirects, TLS/credential policy, media/digest/size errors, token failures, cache hit, and offline reuse.
- Add hostile archive fixtures covering traversal, absolute/non-UTF-8/oversized paths, symlinks, hard links, duplicate conflicts, devices, FIFOs, sockets, unsupported types, count/size/ratio bounds, and cleanup after failure.
- Split tests so each name describes behavior actually exercised; specifically replace the current overclaiming local-link and combined extraction tests.

## Acceptance criteria

- [ ] Cache retention is bounded, deterministic, documented, and never removes an in-use or unverified foreign entry.
- [ ] Concurrent writers/readers expose only complete digest-verified cache and extraction results.
- [ ] Every required issue 26 transport, cache, bound, and hostile-archive case has a focused automated test.
- [ ] Existing extraction directories are validated rather than accepted solely because they exist.
- [ ] Secret-marker checks cover headers, tokens, URL credentials, errors, logs, and snapshots.
- [ ] Ordinary tests use local mock servers/files and require no public network or Docker.
- [ ] Standard workspace quality and deny commands pass.
