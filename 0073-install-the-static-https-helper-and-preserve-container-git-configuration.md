---
id: 73
created: 2026-09-07
depends-on:
  - 71
  - 72
---

# Install the static HTTPS helper and preserve container Git configuration

**Split from:** 68, primarily section 3 and the managed-process/configuration preservation requirements.

## Goal

Make container Git use the static agent's lookup-only HTTPS helper for exactly granted origins, while preserving all underlying configuration and preventing native container helpers from receiving forwarded tokens.

## Work

- Add the static agent helper operation over issue 71's private credential endpoint and issue 72's trusted host backend. No additional runtime or provider client may be required inside the container.
- Bound and validate raw helper input before any use or diagnostic. Only `get` delegates to the host; `store` and `erase` are successful no-ops, with no host operation or secret input logging. Unknown helper operations fail closed.
- Authorize the actual normalized origin at the host and retain actual protocol/host/port/path/username context. Set `credential.useHttpPath=true` or equivalent correct behavior for granted origins; never substitute the original workspace repository path.
- Build reversible cdenv-owned container fragments/process overlays outside the checkout. For a granted origin, reset the effective matching helper chain and install only cdenv's helper. On lookup failure, do not fall back to another configured container helper; ordinary caller-side Git prompting remains separate from host login.
- Preserve native helper behavior for ungranted origins. Compose deliberately with generic and URL/path-specific helpers, includes, both global configuration locations, repository/local configuration, and existing `GIT_CONFIG_*` command-environment settings. Do not overwrite a count, replace the global configuration incompletely, remove an entire credential section, or rewrite shared `.git/config`.
- Provide process-enrollment and owned-integration refresh/removal seams for issues 75–77. Future invocations must regain underlying behavior after disable/reconfiguration; existing integrated clients remain subject to current host policy even if their environment is older.
- Persist only executable/socket paths and nonsecret integration metadata, never returned tokens or raw credential bodies. Deliberate user/container overrides are trusted code, not something this feature claims to prohibit.

## Acceptance criteria

- [ ] Static-helper get returns only the supported credential fields through the private Git helper pipe, reflects host token rotation, and preserves actual account/path context for second remotes and submodules.
- [ ] The complete Git get followed by approve/store and reject/erase sequence is tested with fake generic, path-specific, store, and cache helpers. For granted origins those helpers receive neither lookup traffic nor forwarded credentials/store/erase traffic.
- [ ] Ungranted origins retain their normal helpers and gain no host lookup access. Adding/changing a remote or submodule cannot widen host grants.
- [ ] Existing system/global/local/include files are byte-for-byte unchanged, including with preexisting Git command-environment configuration. Removal deletes only cdenv-owned integration and restores underlying behavior for future invocations.
- [ ] Missing backend, denied origin, malformed/oversized input, disconnected bridge, and unsupported fields fail promptly with value-free diagnostics and no placeholder credential or native-helper fallback for granted origins.
- [ ] Marker tests inspect helper configuration, native helper storage, snapshots, arguments/environment, logs, and status output. Tokens appear only in the intended private transport and Git recipient.
- [ ] Ordinary component tests use fake backends/agents without Docker, public network, or real credentials; `cargo xtask check` passes. Real authenticated fetch/push fixtures and packaged workflows are required separately by issue 78.

## Boundaries and implementation guidance

This issue supplies the working helper and owned integration mechanism. Issue 76 wires live consent/revocation; issue 77 enrolls all actual lifecycle/PTY/non-PTY SSH children and descendants. Issue 75 adds identity defaults without abusing the helper overlay's precedence. Load the Rust best-practices skill and preserve the V1 profile boundary.
