---
id: 63
created: 2026-08-22
---

# Split Bollard resource inspection from mutation operations

## Description

`crates/cdenv-cli/src/bollard.rs` combines endpoint discovery and inspection mapping with image/container create, start, cleanup, and verification calls. Keep the existing `exec` split and separate inspection/discovery from resource mutation so exact request and cleanup tests are auditable independently.

## Acceptance criteria

- [ ] Discovery and inspection mapping have focused tests independent of mutations.
- [ ] Mutation and cleanup ownership have focused tests independent of inspection mapping.
- [ ] Public adapter API and typed error behavior remain unchanged.
