---
id: 91
created: 2026-11-11
depends-on: []
---

# Record macOS Docker Desktop credential helper and agent smoke

Split from issue 78; remaining work under the issue-68 contract. Audited against source `4cf1774` on 2026-11-11.

`docs/smoke/macos-docker-desktop-v1.md` is a checklist, not a completed observation. macOS workspace/package checks and Linux containers running on Docker Desktop do not establish macOS host-helper/keychain compatibility.

## Done when

- Execute the credential/private-Exec scenarios in that checklist on Apple-silicon macOS with Docker Desktop and a matching candidate archive: real host helper/keychain HTTPS behavior, selected/automatic agent refresh and lifetime, revocation and secret-surface checks.
- Save one redacted completed record with hardware, OS/build, Docker Desktop/Git/OpenSSH/helper versions, source revision, archive checksum, date and scenario outcomes. Explicitly label real versus simulated provider/confirmation interactions and any required human interaction.
- Update the macOS traceability row and compatibility documentation only for observed combinations. Unavailable or unperformed required observations remain pending, not inferred passes.

This task owns one platform observation and corrections to claims it substantiates. It does not require re-running all Linux gates or waiting for an undefined final combined release; rerun only observations materially affected by subsequent changes.
