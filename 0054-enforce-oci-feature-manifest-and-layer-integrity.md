---
id: 54
created: 2026-08-21
---

# Enforce OCI Feature manifest and layer integrity

**Parent phase:** Implementation plan §11.5 and Chunk 6
**Depends on:** 26
**Blocks:** 28 and 56

## Goal

Accept only the exact digest-verified OCI artifact shape promised by `cdenv-devcontainer-v1`.

## Work

- Verify a manifest selected from an OCI index/manifest list against the descriptor digest before parsing or using it.
- Define deterministic index selection consistent with the architecture-independent Feature contract; reject ambiguous or unsupported selections rather than taking the first item.
- Require exactly one Dev Container Feature layer with an accepted Feature media type.
- Reject zero Feature layers, multiple Feature layers, and generic OCI image layers in place of the Feature payload.
- Verify declared descriptor sizes and SHA-256 digests before extraction and preserve precise media-type/integrity errors.

## Acceptance criteria

- [ ] Mock OCI tests cover direct manifests and supported index/list variants.
- [ ] Child-manifest digest mismatch fails before any blob is trusted.
- [ ] Zero, duplicate, ambiguous, generic-image-only, and unsupported Feature layers fail closed.
- [ ] Exactly one valid Feature layer resolves deterministically independent of JSON object ordering.
- [ ] Manifest, descriptor, blob size, digest, and media-type errors retain actionable typed context without leaking response bodies.
- [ ] Standard workspace quality commands pass.
