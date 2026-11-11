---
id: 78
created: 2026-09-07
depends-on: []
---

# Credential release gate work superseded by focused follow-ups

## Disposition (2026-11-11)

Retire this oversized coordination issue in favor of the finite remaining tasks below. Its closure records this split; it does not certify that every issue-68 product requirement or macOS observation is complete. Issue 68 remains the normative contract and depends directly on the successors.

## Already delivered

- The strict opt-in credential suite, authenticated HTTPS fixtures, packaged lifecycle/SSH/recovery/revocation baseline, both native Linux architectures, coverage inventory and compatibility documentation exist. Baseline source `c268e9e8980701e55de20b589e14f815062cc33a` passed [CI 42833912796](https://github.com/mjswensen/cdenv/actions/runs/42833912796) and [Release packages 42833912999](https://github.com/mjswensen/cdenv/actions/runs/42833912999), with 3 credential tests per native minimum/pinned lane.
- Source `7c3c6f3` added controlled Git-over-SSH clone/fetch/push, unknown/changed host-key rejection, empty-agent recovery, and lifecycle cancellation safety regressions. These are implemented tests, not new feature work; the cited baseline predates them and does not prove their execution.
- Focused identity precedence/removal tests already exist in `crates/cdenv-agent/tests/git_identity.rs`. Core broker tests cover malformed/truncated/oversized frames, scoped identity/revision mismatches and admission bounds. Host broker tests cover policy-epoch response suppression and rejected-agent-open draining. Do not recreate these as generic audit tickets.

## Remaining work

| Issue | Finite scope |
|---|---|
| [84](0084-cancel-active-detached-lifecycle-work-with-verified-per-run-ownership.md) | Cancel active detached lifecycle work with verified per-run ownership |
| [85](0085-test-packaged-ssh-agent-selection-refresh-and-confirmation-timeouts.md) | Test packaged SSH-agent selection refresh and confirmation timeouts |
| [86](0086-test-synchronized-credential-revocation-and-old-client-denial.md) | Test synchronized credential revocation and old-client denial |
| [87](0087-test-credential-generation-rollback-after-a-failed-rebuild.md) | Test credential generation rollback after a failed rebuild |
| [88](0088-test-credentials-during-compose-partial-replacement-recovery.md) | Test credentials during Compose partial replacement recovery |
| [89](0089-test-public-credential-backend-and-bridge-failure-classification.md) | Test public credential backend and bridge failure classification |
| [90](0090-cover-missing-credential-secret-marker-surfaces.md) | Cover missing credential secret-marker surfaces |
| [91](0091-record-macos-docker-desktop-credential-helper-and-agent-smoke.md) | Record macOS Docker Desktop credential helper and agent smoke |

Each successor owns its relevant tests and traceability/documentation update. Normal required CI checks accompany changes; there is no replacement umbrella demanding perpetual final evidence collection. Existing baseline evidence remains scoped to its revision.

## Historical record

The full original requirements, work packages, CI triage and chronology are preserved in tracker history at `e6d6855e86392111fa8c4251490290a24ecfbdc8`:

```sh
git -C .issues show e6d6855e86392111fa8c4251490290a24ecfbdc8:0078-gate-credential-workflows-security-and-cross-platform-release-compatibility.md
```

The current evidence map is `tests/integration/fixtures/credential-coverage.json`. No unchecked historical checklist is retained here as a second competing work queue.
