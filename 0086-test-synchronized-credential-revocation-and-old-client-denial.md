---
id: 86
created: 2026-11-11
depends-on: []
---

# Test synchronized credential revocation and old-client denial

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

The existing packaged test keeps an old SSH session alive and checks that a new child lacks `SSH_AUTH_SOCK` after disable. It does not hold a credential request in flight or retry through the old client's existing endpoint. The component test `queued_lookup_result_is_suppressed_when_policy_epoch_changes` already verifies epoch-based suppression.

## Done when

- A controlled helper synchronizes request-start and response-release so public disable/origin deny occurs during a demonstrably pending HTTPS lookup; its revoked response is not delivered.
- A synchronized active agent stream is closed on selective revocation, and an old client cannot obtain new credentials/signatures through its retained endpoint. Other capabilities and unrelated SSH/forwarding work remain usable.
- Explicit restart/reconciliation cannot restore persisted revoked permission; unacknowledged revocation reports nonzero rather than false success.
- Add the uncovered local-close-before-peer-error ordering to the rejected-open/cancellation component regression, retaining unknown-stream rejection and bounded completion.
- Update the revocation traceability row with these named tests and their execution results. State the actual boundary: delivered credentials/signatures and established authenticated connections cannot be recalled.

Keep the existing broker epoch and failed-agent-open tests; this work adds the missing public-command synchronization and race ordering.
