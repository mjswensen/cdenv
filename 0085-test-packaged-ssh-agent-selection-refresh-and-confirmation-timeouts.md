---
id: 85
created: 2026-11-11
depends-on: []
---

# Test packaged SSH-agent selection refresh and confirmation timeouts

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

The packaged credential workflow already contains real signing, concurrent clients, same-path restart, absent/empty-agent recovery, and controlled Git-over-SSH clone/fetch/push with unknown/changed host-key rejection. The remaining selection and confirmation paths are absent from that workflow.

## Done when

- Controlled agents with distinguishable identities demonstrate explicit selection and automatic `SSH_AUTH_SOCK` selection through public commands.
- Changing the automatic socket followed by explicit reconciliation refreshes selection; an explicit selector never silently falls back to a different agent. A container-supplied socket path cannot choose a host endpoint.
- A controlled confirmation-capable agent demonstrates allowed/rejected requests and bounded timeout with recovery, without stopping unrelated clients. Label simulated confirmation behavior explicitly.
- Record the named scenarios in the SSH-agent traceability row and run their required native credential-suite checks.

Do not reimplement Git-over-SSH or identity-default precedence/removal: the former is already in `tests/integration/tests/credentials.rs`; the latter has focused tests in `crates/cdenv-agent/tests/git_identity.rs`. Real platform confirmation observations belong to issue 91.
