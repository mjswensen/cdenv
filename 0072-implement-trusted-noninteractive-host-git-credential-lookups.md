---
id: 72
created: 2026-09-07
depends-on: []
---

# Implement trusted noninteractive host Git credential lookups

**Split from:** 68, primarily section 3's lookup, context, and trusted-host requirements. The parent contract remains authoritative.

## Goal

Provide the broker's narrow, lookup-only host Git backend without caching tokens, evaluating checkout configuration, initiating login, or exposing credential bodies through the ordinary subprocess logging path.

## Existing foundation

Reuse the working-tree `cdenv-core::credential_protocol` request/result types and exact HTTPS-origin types. They already bound/redact parsing and model expiry, but no host credential adapter or subprocess exists. The backend can be developed independently of issue 71's transport; issue 73 connects it to the static helper and broker.

## Work

- Resolve host Git and trusted configuration/environment from the explicit host launch/configuration context. Execute `git credential fill` in a controlled neutral directory, with repository discovery and config injection controlled. Never execute from the container-writable checkout or import container-supplied execution context.
- Preserve actual HTTPS protocol, hostname/port, repository path, and supplied username on private stdin. Let trusted user/system helpers, per-URL account configuration, and host-owned includes apply their matching policy. Document why checkout-dependent `includeIf gitdir`/`onbranch` configuration is not automatically reproduced.
- Expose only lookup; do not add host approve/store/reject/erase operations or clone interception. No cdenv token cache or credential-bearing persistence is allowed; each lookup consults current host helpers.
- Use a secret-safe subprocess path separate from normal operation logs. Bound input/output, admission/concurrent helper processes, total buffered bytes, and execution time; cancel and reap owned helper subprocess work on timeout, disconnect, or revocation.
- Default to noninteractive host execution: `GIT_TERMINAL_PROMPT=0`, suppressed askpass, and declared/tested helper-specific noninteractive settings. Do not initiate browser/OAuth login. Git's prompt flag alone is not a universal GUI prohibition for arbitrary trusted helper code.
- Validate supported result fields, echoed context, and expiry. Return unavailable/no credential for missing helpers, incomplete/expired results, login-required state, or a failed lookup, with concise value-free host reauthentication guidance. Never manufacture placeholder credentials or forward refresh tokens/unknown config fields.
- Keep grant authorization and final revision checking explicit in the broker contract: issue 71/76 must authorize dispatch and recheck before release; this adapter's success is not authority to return a token after revocation.

## Acceptance criteria

- [ ] Fake host Git/helper fixtures observe only controlled arguments, trusted host environment/configuration, a neutral working directory, and the exact supported request context on stdin.
- [ ] Two paths and two usernames at one allowed origin remain distinguishable; a malicious checkout helper/include, caller-supplied config/path/environment, and unsupported/secret-bearing request fields cannot run host code.
- [ ] Successive requests observe simulated token rotation without a cdenv cache. Expired/incomplete/unsupported results are rejected or unavailable as specified; no store/erase host operation is possible.
- [ ] Helper admission, output bounds, deadlines, cancellation, and process cleanup have boundary tests, including stalled and noisy helpers. Unavailability is a typed backend health outcome, not automatic workspace teardown.
- [ ] Tests verify askpass/noninteractive settings and supported helper behavior; automatic host login is not invoked. Unsupported helper/clone-only authentication mechanisms are documented honestly.
- [ ] Secret markers are absent from host files, logs/debug/error summaries, process arguments/environment, and status/diagnostic output; only intended private pipes and test recipients receive them.
- [ ] Ordinary tests require no Docker, public network, live keychain, or real credentials. Document the backend's numeric limits and compatibility boundary, and pass `cargo xtask check`.

## Boundaries and implementation guidance

Issue 73 owns container helper/configuration and end-to-end Git get/approve isolation. Issue 75 reuses the trusted host configuration context for name/email only. Load the Rust best-practices skill; prefer typed availability/errors and structured, bounded subprocess ownership over general remote execution.
