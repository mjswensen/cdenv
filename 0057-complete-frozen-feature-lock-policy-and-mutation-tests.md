---
id: 57
created: 2026-08-21
---

# Complete frozen Feature lock policy and mutation tests

**Parent phase:** Implementation plan §10.10, §11.5, and §17.1–17.2
**Depends on:** 27 and 56
**Blocks:** 28

## Goal

Provide the missing evidence that frozen Feature locks are reproducible, fail closed, work offline when promised, and preserve the checkout mutation boundary.

## Work

- Add direct tests for `inspect_existing_container_lock` and `resolve_frozen_features_offline`.
- Cover stale root references, changed root/dependency options, changed dependencies, versions, resolved digests, integrity, malformed/newer locks, and missing records.
- Cover verified offline frozen-cache success, corrupt/missing frozen-cache failure, and unlocked offline failure.
- Test missing-lock policy and existing-container drift inspection without registry access.
- Add a before/after checkout guard covering every non-`lock` cdenv test path; allow only repository-defined command effects where explicitly exercised.
- Prove `lock --config` changes only the target lockfile and never changes desired state, stages Git content, or mutates another checkout path.
- Check exact target mode preservation, symlink/non-regular/escape refusal, atomic replacement, and temporary cleanup under injected faults.

## Acceptance criteria

- [ ] Every acceptance case originally required by issue 27 has a focused automated test.
- [ ] Frozen locks succeed offline only from freshly digest-verified cache content.
- [ ] Stale/inconsistent/corrupt locks fail with `cdenv lock` guidance at the command boundary.
- [ ] Existing-container inspection performs no source/network access and reports current, missing, or drift correctly.
- [ ] The checkout mutation guard detects any cdenv-owned write outside explicit `cdenv lock`.
- [ ] Desired workspace state is byte-identical before and after `lock --config`.
- [ ] Standard workspace quality commands pass.
