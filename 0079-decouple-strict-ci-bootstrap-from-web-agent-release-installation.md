---
id: 79
created: 2026-09-26
depends-on: []
---

# Decouple strict CI bootstrap from web-agent release installation

## Category

CI infrastructure / development-container bootstrap. Blocks issue 78.

## Evidence

CI run [36219272407](https://github.com/mjswensen/cdenv/actions/runs/36219272407), 2026-09-26, source revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`. Local evidence: `ci.log:1272-1310`.

The `Strict Rust and cargo-deny quality gate` job fails during the development container's `postCreateCommand`, before the strict checks execute:

```text
[web-agent-install] curl: (22) The requested URL returned error: 403
[web-agent-install] Unable to resolve latest release tag.
[web-agent-install] ERROR task failed
postCreateCommand from devcontainer.json failed with exit code 1.
Command failed: /bin/sh -c mise install && mise bootstrap
```

`mise.toml` makes `bootstrap` depend on both `cargo-deny-install` and `web-agent-install`; the latter runs the web-agent installer from its main branch. `.devcontainer/devcontainer.json` invokes the shared bootstrap. An unrelated interactive development tool's release lookup can therefore prevent mandatory Rust/security checks from running. The log establishes HTTP 403, not whether its cause is rate limiting, permissions, or another upstream restriction.

## Work

- Separate required CI tooling from optional interactive web-agent installation/startup, or provide an equivalently reproducible CI-specific bootstrap.
- Keep the pinned Rust/cargo-deny checks mandatory; do not mask their installation or execution failures.
- Keep the intended local development bootstrap usable and document any changed task selection.

## Acceptance criteria

- [ ] A clean pinned-container CI run reaches and passes the strict Rust and cargo-deny checks.
- [ ] Simulating an unavailable/403 web-agent release endpoint cannot prevent required CI checks from executing.
- [ ] Required tool failures still fail CI, and optional-tool handling does not use blanket error suppression.
