---
id: 53
created: 2026-08-21
---

# Secure OCI bearer authentication and redirect handling

**Parent phase:** Implementation plan §11.5 and Chunk 6
**Depends on:** 26
**Blocks:** 28 and 56

## Goal

Make public anonymous OCI authentication work for the complete manifest/index/blob flow without disclosing bearer credentials across origins.

## Work

- Retain the anonymous bearer token for every same-registry request that requires it, including selected index manifests and Feature blobs.
- Retry supported unauthorized requests only through the declared anonymous Bearer challenge; continue rejecting private/basic authentication.
- Never forward `Authorization` across an origin change. Strip it before cross-origin redirects and define/test the same-origin comparison, including scheme, host, and effective port.
- Preserve verified HTTPS, redirect-count, response-size, cancellation, and redacted-diagnostic behavior.
- Migrate the GHCR token-on-blob behavior recorded in ADR 0001 into production mock-server regression tests.

## Acceptance criteria

- [ ] A deterministic mock registry proves challenge → token → manifest → blob succeeds and that the blob request receives the required bearer token.
- [ ] Index-child manifest requests use authentication when required.
- [ ] Same-origin redirects retain authorization only where needed; cross-origin redirects receive no token.
- [ ] Malformed challenges, rejected tokens, unsupported auth methods, redirect loops, and unauthorized blobs fail with typed redacted errors.
- [ ] No token appears in errors, logs, snapshots, or captured non-registry requests.
- [ ] Standard workspace quality commands pass.
