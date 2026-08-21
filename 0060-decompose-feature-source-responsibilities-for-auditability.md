---
id: 60
created: 2026-08-21
---

# Decompose Feature source responsibilities for auditability

**Parent phase:** Implementation plan §6.1, §6.6, §11.5, and Chunk 6
**Depends on:** 26
**Blocks:** 28 and 56

## Goal

Make the security-critical Feature source implementation reviewable without introducing speculative adapter abstractions.

## Work

- Split `feature_sources.rs` along concrete responsibilities such as OCI protocol/authentication, HTTP/redirect policy, cache, archive extraction, local sources, and shared orchestration.
- Keep public exports narrow and preserve typed layered errors/source chains.
- Do not add `async_trait`, boxed adapter dispatch, generic repositories, or abstractions without a demonstrated test seam.
- Keep focused unit tests beside each responsibility and higher-level resolver tests at the module boundary.
- Assess `feature.rs`, `image_orchestration.rs`, and `bollard.rs` for similarly concrete responsibility splits; record follow-up issues only where names and independent tests improve clarity.

## Acceptance criteria

- [ ] No single Feature source module mixes registry authentication, redirect policy, cache mutation, archive extraction, and local-tree traversal.
- [ ] Existing behavior remains covered during the move; security fixes from 53–56 land against clear module boundaries.
- [ ] Public API and error behavior do not expand merely to enable the split.
- [ ] Tests remain descriptive and keep actions/assertions visible.
- [ ] Any deferred oversized-module recommendation is captured in a focused follow-up issue with a concrete boundary.
- [ ] Standard workspace quality commands pass.
