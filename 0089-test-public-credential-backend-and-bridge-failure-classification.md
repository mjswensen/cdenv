---
id: 89
created: 2026-11-11
depends-on: []
---

# Test public credential backend and bridge failure classification

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

The packaged workflow observes an unavailable host agent and recovery of a credential operation. It does not assert the complete public `up`/status distinction between an unavailable backend and failure to establish required bridge readiness.

## Done when

- With a safe configured but absent/empty agent or unavailable helper, `up` succeeds and human/JSON status reports the affected backend as unavailable/degraded (or untested when no lookup has occurred), without claiming authentication success.
- Deterministically fail required bridge setup/readiness; the mutating command returns nonzero with value-free actionable diagnostics while preserving checkout, volumes and the otherwise running container.
- Restore the controlled prerequisite and use explicit reconciliation to recover; read-only status/doctor does not repair services, retrieve tokens or sign to improve its report. An authorized credential-dependent hook still fails under its normal checkpoint rules when its backend is unavailable.
- Add these named public-command assertions to the backend-failure traceability row and run the affected tests.

Reuse existing absent-agent/recovery setup and component classification coverage; do not build a new generic health framework.
