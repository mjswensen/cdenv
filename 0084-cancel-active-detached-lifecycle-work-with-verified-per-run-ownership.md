---
id: 84
created: 2026-11-11
depends-on: []
---

# Cancel active detached lifecycle work with verified per-run ownership

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

Production `execute_lifecycle` in `crates/cdenv-cli/src/production.rs` still starts concatenated shell commands through detached Docker Exec and `run-managed-environment`. The recorded Exec identity recognizes completed work, but active cancellation remains indeterminate. The separate agent lifecycle runner is generation-scoped and cannot simply replace this path without losing credential enrollment or repeated post-start execution.

## Done when

- An owned runner has durable per-run identity bound to installation, workspace, container, generation, build/protocol and exact execution; credential enrollment remains available to its children.
- Authenticated cancellation acknowledges bounded child cleanup and authoritative final execution state. Missing, mismatched, unstarted, legacy or unverifiable records remain fail-closed; a persisted PID alone never authorizes signaling.
- Component regressions and packaged down/rebuild scenarios cover active and completed work, interrupted launch/crash recovery, unrelated-process preservation, and repeated `postStartCommand` after down/up in the same generation. Cancelled one-time work retains accurate checkpoint/retry semantics.
- Update the lifecycle rows in `credential-coverage.json` and the documented cancellation limitation to those outcomes.

Existing `cancellation_timeout_does_not_signal_a_persisted_pid` and `mismatched_cancellation_does_not_publish_a_cancel_request` regressions are retained; they are safety coverage, not the missing active-cancellation implementation.
