---
id: 26
created: 2026-08-20
---

# Implement secure Feature sources, cache, and extraction

_Converted from [`implementation-chunks/26-feature-sources-cache-and-extraction.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.5 and Chunk 6](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#chunk-6-features-cache-lockfile-and-generated-images)
**Depends on:** 19, 21, and the HTTP/OCI decisions in 04

## Goal

Resolve allowed Feature artifacts with bounded, digest-verified I/O and no Docker credential dependency.

## Work

- Implement the minimum OCI Distribution client for public registries: reference normalization, manifest/media negotiation, anonymous bearer challenge/token exchange, blob download, and size/digest verification.
- Add verified-HTTPS tarball retrieval and contained local Feature directory reads. Reject URL credentials, private-auth requirements, insecure HTTP, custom TLS bypasses, and unsafe redirects.
- Store verified blobs in a content-addressed cache under the cdenv root; use atomic writes, bounded retention metadata, and digest checks on every reuse.
- Bound redirects, headers/body sizes, decompression ratio/bytes, file count, path length, and extracted bytes.
- Harden archive handling against absolute/traversal paths, escaping symlinks/hardlinks, devices, FIFOs, sockets, duplicate-conflict tricks, and unsupported entry types.
- Return verified artifact/metadata inputs to the pure Feature planner; do not build images or write lockfiles yet.

## Rust guidance

Load the `rust-best-practices` skill. Stream and hash rather than buffering whole artifacts, use layered transport/cache/archive errors, avoid clones in download loops, and test with focused mock servers.

## Acceptance criteria

- Mock OCI tests cover anonymous bearer flow, manifest variants, redirects, digest/size/media errors, token failures, and cache hit/offline reuse.
- HTTPS/local tests cover success, containment, TLS enforcement, credentials rejection, and all configured bounds.
- Hostile archive fixtures for every prohibited entry fail without writing outside an operation-owned extraction directory.
- Concurrent cache writers produce one valid digest-addressed result and no partial blob.
- Ordinary tests use mock servers/local files; standard workspace quality and deny checks pass.