---
id: 58
created: 2026-08-21
---

# Preserve Dockerignore semantics in context validation

**Parent phase:** Implementation plan §11.4, §12, and Chunk 5
**Depends on:** 22
**Blocks:** 28

## Goal

Prevent cdenv's preflight traversal from rejecting build-context content that Docker would exclude through `.dockerignore`.

## Work

- Reconcile context safety/size validation with the ADR decision that Docker owns `.dockerignore`, Dockerfile parsing, and context semantics.
- Do not count or reject ignored oversized/special entries when Docker would not send them.
- Avoid implementing a subtly incompatible partial `.dockerignore` parser; either use a maintained compatible implementation or redesign preflight so Docker remains authoritative while cdenv-owned generated material stays bounded.
- Preserve checkout containment and escaping-symlink protections required by the V1 trust boundary.
- Cover repository and generated contexts separately.

## Acceptance criteria

- [ ] A large ignored file/directory does not cause cdenv rejection when Docker excludes it.
- [ ] Negation, anchored patterns, nested ignore files as applicable, and Dockerfile-specific context behavior match the selected Docker semantics.
- [ ] A non-ignored oversized context still fails before unbounded cdenv-owned materialization.
- [ ] Escaping symlinks and generated-context traversal remain rejected.
- [ ] Fake/optional real-Docker tests demonstrate cdenv and Docker agree on the effective context.
- [ ] Standard workspace quality commands pass.
