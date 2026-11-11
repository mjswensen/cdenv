---
id: 88
created: 2026-11-11
depends-on: []
---

# Test credentials during Compose partial replacement recovery

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

The credential suite has no Compose fixture that exercises partial replacement. Generic Compose lifecycle/recovery tests do not observe credential lease and endpoint ownership.

## Done when

- A credential-enabled Compose fixture deterministically causes partial replacement and exercises the existing explicit recovery path through public commands.
- Assert which verified primary generation owns credential access before failure and after recovery; failed/replaced candidates cannot retain access or receive cross-generation replies. Do not adopt a container based on its own identity claims.
- Verify checkout/named-volume data, unrelated containers and unaffected services survive according to the existing Compose recovery contract; bound all waits and clean up the fixture.
- Add the named Compose scenario to the generation traceability row and execute the affected credential suite.

This is independent of successful image rebuild coverage and of the failed image-rebuild fixture in issue 87.
