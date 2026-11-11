---
id: 87
created: 2026-11-11
depends-on: []
---

# Test credential generation rollback after a failed rebuild

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

`packaged_commands_cover_lifecycle_sessions_recovery_and_revocation` covers a successful rebuild. It does not force a credential-enabled candidate to fail and inspect the surviving old generation.

## Done when

- A packaged public-command fixture starts a working credential-enabled workspace, proves candidate hooks can authenticate, then deterministically fails the candidate before handoff.
- Rebuild reports failure, revokes the failed candidate, and retains the valid old generation and its credential services. Old/candidate requests or replies cannot cross generation boundaries.
- The fixture verifies checkout, named-volume data and unrelated-container preservation, with bounded failure handling and cleanup.
- Add the named rollback scenario to the generation traceability row and execute the affected credential suite.

Fix a runtime defect if this fixture reveals one; no broad release-evidence or successful-rebuild reimplementation is required.
