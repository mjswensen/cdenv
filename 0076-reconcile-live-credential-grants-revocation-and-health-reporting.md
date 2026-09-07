---
id: 76
created: 2026-09-07
depends-on:
  - 71
  - 73
  - 74
  - 75
---

# Reconcile live credential grants revocation and health reporting

**Split from:** 68, primarily sections 1 and 6's permission/revocation/health contracts.

## Goal

Replace the permission foundation's active-generation `RuntimeUnavailable` and stopped-supervisor-only revocation behavior with authenticated live reconciliation. A successful disable/deny must mean that live access was revoked, not merely that a JSON file changed.

## Existing foundation

Reuse the staged/bound/binding-pending receipt model, strict capability/origin validation, private storage, and monotonic revision machinery already present in the working-tree implementation. Keep issue 68 as the parent contract; do not reimplement its CLI or reinterpret old grants as broader authority.

## Work

- Reconcile enable/allow/deny/selective/all disable against the verified running generation and issue 71's authenticated supervisor leases, using the backends/integration from issues 73–75. Do not rerun hooks or stop the container to change permission.
- Refresh automatic SSH socket selection and identity metadata only from explicit host mutating reconciliation. Preserve explicit selectors and omission/idempotence rules. If agent/provisioning compatibility needs repair, give explicit `up` guidance rather than repairing during read-only/SSH operations.
- Validate current authority at host dispatch and immediately before releasing every newly obtained HTTPS response. Use revisions/epochs and targeted cancellation so revocation suppresses affected in-flight results and cannot be undone by queued replies or supervisor restart.
- Persist revocation and require live acknowledgement before reporting success. Close affected SSH-agent streams and cancel affected lookups across authorized active/candidate leases without terminating unrelated capabilities, listeners, SSH sessions, jobs, or containers.
- If acknowledgement is unavailable, prove only verified owned resources stopped or return nonzero with explicit revocation-unconfirmed guidance. Never select/signal an unverified PID, treat unknown control state as authorization, or forget the durable revocation on a retry.
- Reconfigure/remove only cdenv-owned integration for future invocations. Already integrated clients use current host policy; enabling a previously absent environment integration may require a new cdenv SSH session and must say so.
- Replace constant inactive/uninspected reporting with shared per-capability configured/bound/active/transport/backend facts in dedicated status, list/status, and doctor. Distinguish disabled/inactive, healthy transport, untested lookup availability, backend degradation, and missing advisory identity metadata.
- Keep status/doctor/SSH connection read-only with respect to grants, migration, startup, and repair. Health inspection must not retrieve tokens, sign, erase credentials, or initiate login. Unknown capability/protocol/state versions fail closed.

## Acceptance criteria

- [ ] Real command/control component tests reconcile each capability independently on an already running verified generation; no lifecycle hook, unrelated listener, session, or container is restarted.
- [ ] Denying one origin or disabling a capability suppresses its pending HTTPS results, closes its SSH streams where applicable, and blocks later old-client/restarted-broker access; unaffected authority remains usable.
- [ ] Paused helper replies, queued transport responses, concurrent requests/revocation, candidate leases, supervisor loss, acknowledgement timeouts, and retry/restart races have deterministic tests.
- [ ] Success requires confirmed revocation. Unknown or mismatched control identity returns an honest nonzero result after durable revocation, never an unsafe PID signal or a false positive based only on persisted policy.
- [ ] Staging, binding-pending retries, root/name reuse isolation, idempotence, selective disable, allowlist/selector removal, and unknown-capability rejection retain the foundation's guarantees.
- [ ] Shared human/JSON health facts contain no credentials or default identity values and never claim unqueried credentials work. Read-only checks are proven not to mutate, grant, launch helpers/login, sign, migrate, or start/repair services.
- [ ] Component boundary/secret-marker tests and `cargo xtask check` pass. Document environment-restart guidance and the inability to recall already delivered tokens/identity, completed signatures, or authenticated connections.

## Boundaries and implementation guidance

Issue 77 invokes these reconciliation APIs from actual create/up/down/rebuild and enrolls lifecycle/SSH processes. Issue 78 supplies cross-platform public-workflow evidence. Load the Rust best-practices skill; preserve atomic persistence/lock ordering and use structured, scope-specific cancellation rather than tearing down the whole workspace owner.
