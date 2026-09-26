---
id: 82
created: 2026-09-26
depends-on: []
---

# Migrate CI actions off deprecated Node.js 20 runtimes

## Category

CI dependency maintenance / compatibility warnings (not the cause of this run's test failures). Blocks issue 78's requested CI follow-up set.

## Evidence

CI run [36219272407](https://github.com/mjswensen/cdenv/actions/runs/36219272407), 2026-09-26, source revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`. See `ci.log:144-249` and repeated job-completion warnings, including `1328`, `8389`, and `19974`.

```text
Node.js 20 is deprecated. The following actions target Node.js 20 but are being forced to run on Node.js 24
```

Across the annotations, the named actions are `actions/checkout@v4`, `actions/upload-artifact@v4`, `actions/download-artifact@v4`, and `docker/setup-buildx-action@v3`. Artifact actions also emit `DEP0040` (`punycode`), `DEP0169` (`url.parse()`), and `DEP0005` (`Buffer()`) warnings; examples appear at `ci.log:1906-1912` and `19899-19913`.

These warnings appear in successful jobs too. They must not be confused with the bootstrap, socket-path, or evidence-retention failures.

### Release workflow corroboration (2026-09-26)

Release packages run [36219272470](https://github.com/mjswensen/cdenv/actions/runs/36219272470), at the same source revision, repeats the same Node.js 20-to-24 annotations for all four named actions (`release.log:45-64`; job completions at `399`, `2632`, `3272`, and `5500`). Artifact actions also repeat `DEP0040`, `DEP0005`, and `DEP0169` (examples at `196`, `207`, `370`, `376`, `3239`, and `3245`). These excerpts refer to the original local log, removed after triage; the linked run retains the source context.

Include `.github/workflows/release.yml` in the migration and verify both agent and host packaging jobs. Track this shared maintenance problem here rather than duplicating it for the release workflow. The actual Linux installed-smoke failure is the missing lifecycle manifest tracked in issue 83, not these warnings.

## Work

- Audit CI and related reusable/release workflows for actions whose declared runtime is deprecated, including transitive action usage.
- Select supported releases that explicitly support the current runtime; review migration notes, runner requirements, checkout behavior, artifact compatibility, and architecture support before updating references.
- Recheck dependency deprecations after upgrading and document any remaining upstream-only warnings rather than suppressing runner diagnostics.

## Acceptance criteria

- [ ] Used actions no longer require the runner's forced Node.js 20-to-24 migration; a clean run has no corresponding deprecation annotations.
- [ ] Checkout, native agent assembly, Buildx, and artifact upload/download continue working on the supported runner matrix with existing verification and retention guarantees.
- [ ] Remaining dependency-level warnings are eliminated or explicitly attributed to a supported upstream release with a tracked remediation path.
