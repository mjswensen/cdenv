---
id: 78
created: 2026-09-07
depends-on:
  - 77
  - 79
  - 80
  - 81
  - 82
  - 83
---

# Gate credential workflows security and cross-platform release compatibility

**Split from:** 68, primarily section 9 and the documentation/compatibility work in section 8.

## Goal

Provide reproducible, nonempty end-to-end and release evidence for the complete workspace credential product. Existing generic profile/OpenSSH tests and the permission/parser foundation's passing Linux arm64 tests are not evidence that live credential forwarding works.

## Implementation status

_Last reconciled: 2026-11-10._

### Passing clean baseline (2026-11-10)

Revision `c268e9e8980701e55de20b589e14f815062cc33a` passes
[CI 42833912796](https://github.com/mjswensen/cdenv/actions/runs/42833912796)
and [Release packages 42833912999](https://github.com/mjswensen/cdenv/actions/runs/42833912999).
Each native Linux x86_64/arm64 minimum/pinned credential leg discovers, executes,
and passes **3 tests**, both in release execution and instrumented coverage.
This supersedes the failing November 9 baseline and establishes work package 1.
The passing workflows include workspace/quality checks, release suites, installed
package smoke and coverage inventory. The CI log was inspected locally at
`/tmp/cdenv-78-ci.log`; the linked run retains the durable evidence.

This baseline covers the existing fixtures only. Active detached cancellation,
Git-over-SSH and remaining agent-selection cases, synchronized in-flight
revocation, rollback/partial replacement, complete secret-surface auditing, and
actual macOS Docker Desktop/helper observations remain required. macOS packaging
and workspace tests do not substitute for that smoke. Repeat affected gates after
implementation changes; this baseline does not close the acceptance checklist.

### Historical CI baseline (2026-11-09)

Pushed revision `bd2e41f4c470aaf6bda7063871d9bcc095274ae3` was checked in
[CI 42669798783](https://github.com/mjswensen/cdenv/actions/runs/42669798783) and
[Release packages 42669798825](https://github.com/mjswensen/cdenv/actions/runs/42669798825).
Dependencies 77 and 79–83 are closed in this source history, but the complete
credential release evidence remains outstanding.

- Strict Rust/cargo-deny, both Linux workspace coverage jobs, all security mutation
  shards, and native agent builds pass. The prior installed key-path rejection and
  unbounded failure hang no longer appear.
- macOS workspace coverage fails in `status_on_an_absent_root_does_not_create_or_probe_anything`:
  its temporary root exceeds the conservative supervisor socket path limit.
- All four native Linux credential gates still discover three tests, with only
  traceability passing. Both live workflows fail during create because the detached
  supervisor exits with status 1 before authenticated readiness. A prior missing
  credential-parent fix and passing component tests have not established passing
  packaged startup.
- Both installed Linux package smokes now pass key metadata checks but time out at
  the first `cdenv ssh` command (60 seconds). The key destination fix is not sufficient
  evidence of successful SSH operation.
- Coverage inventory remains unsuccessful because required observations fail.
  No acceptance checkbox is closed by these results. Current local follow-up fixes
  require a clean pushed matrix rerun after review.


### Local follow-up diagnosis (2026-11-09; not release evidence)

- `70fb88a` makes credential permission fixtures use short private canonical
  temporary roots; all 27 tests pass even with deliberately long TMPDIR.
- `8653977` flushes interactive Docker Exec input/output before EOF. Buffered-stream
  regressions fail with the old behavior and pass with the fix. This resolves the
  first installed SSH timeout in a native ARM64 diagnostic environment.
- `072279e` and `1023f4b` repair installed-smoke shell quoting, process-name checks,
  the Alpine HTTP fixture, and bounded response retries. The entire diagnostic
  installed smoke passes on Docker Engine/CLI 29.7.1, Compose 5.3.1, OpenSSH 10.0p2,
  including lifecycle recovery, forwarding, rebuilds and preservation assertions.
- `d92ad9f` secures the owned shared `.cdenv` state root during environment capture.
  Live comparison confirmed the old agent rejected its permissive ancestor before
  bridge readiness, while the rebuilt native ARM64 agent completes credential
  create and initial foreground/detached hooks. Ownership/symlink checks remain
  strict and home permissions remain unchanged.

- `27217f0` preserves the bridge across rejected host-agent opens using bounded
  known-stream draining and an actual agent Cancel acknowledgment; arbitrary
  unknown IDs still fail. `cd23862` fixes remote-shell quoting in credential tests.
- `73a1a23` consumes bounded known Git capability advertisements and HTTP challenge
  metadata at the helper boundary; the private credential parser remains strict.
- `8ef56db` records exact detached Exec identity and confirmed start so a successful
  completed hook can be recognized during down/up. Active, failed, unstarted, or
  unverifiable work remains indeterminate; active detached cancellation is still
  an explicit runtime limitation, not completed evidence.
- `67f1b65` safely recovers exact stale supervisors under the lifetime lock and
  hardens private runtime record reads. Live-lock, foreign identity, unsafe paths,
  and symlink replacement remain rejected.

Final local validation used native Linux ARM64 with Docker Engine/CLI 29.7.1,
Compose 5.3.1, OpenSSH 10.0p2, proper TLS trust, and rebuilt agent artifacts.
`cargo xtask test-integration --suite credentials` enforced the declared
prerequisites and reported **3 discovered, 3 executed, 3 passed**. Both live
workflows pass, including host-agent loss/recovery, down/up, rebuild, controlled
supervisor loss, revocation, and the authenticated HTTPS cases. The complete
installed operational smoke and ordinary macOS workspace tests also pass locally.
Diagnostic logs are retained locally at `/tmp/cdenv-credentials-final-gate.log`
and `/tmp/cdenv-installed-smoke-final.log`.

These observations used a diagnostic host/archive with an explicit prior build
identity and rebuilt ARM64 agent; they are not clean release matrix evidence.
Clean native x86_64/ARM64 minimum/pinned CI gates and the actual macOS smoke remain
required before closure. No acceptance checkbox is closed by this local validation.

### Historical CI triage (2026-09-26)

CI [36219272407](https://github.com/mjswensen/cdenv/actions/runs/36219272407), revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`, was categorized from the saved `ci.log`. Follow-up blockers:

- **79 — CI bootstrap:** web-agent release lookup returns HTTP 403 and aborts the development-container bootstrap before strict Rust/cargo-deny checks run.
- **80 — Unix-socket path portability:** the macOS registry socket test and both packaged credential workflows in all four Linux release jobs fail with `path must be shorter than SUN_LEN`. This groups the repeated failures across platforms/matrix legs.
- **81 — Evidence retention:** early credential failures skip OpenSSH and all release coverage runs; 12 artifact uploads find no files. The aggregate coverage inventory subsequently fails with only workspace reports available, including failed macOS evidence. Preserve diagnostics and explicit not-run outcomes without weakening the required-success gate.
- **82 — Action runtime maintenance:** migrate the actions named in Node.js 20 deprecation warnings to supported runtimes and triage their dependency deprecations. These warnings are not the cause of the failing tests.

Unlike the earlier package-preparation failure below, this run builds the packaged host, passes eight Dev Container profile tests per Linux release job, and discovers/executes three credential tests per job. Only the traceability test passes; both live workflows fail during create. Linux ordinary workspace coverage and security mutation jobs pass, but this is still not passing credential release evidence. Skipped OpenSSH/coverage steps are not test passes or independent demonstrated runtime failures.

Other log noise (unstable branch-coverage notices, third-party OpenSSH build warnings, Docker cleanup of absent nftables tables, and expected negative-test Git errors in passing suites) was not classified as additional demonstrated gate failures. No acceptance checkbox is closed by this triage.

### Historical release-workflow triage (2026-09-26)

Release packages [36219272470](https://github.com/mjswensen/cdenv/actions/runs/36219272470), at revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`, was categorized from `release.log`:

- **83 — Installed lifecycle/SSH integration:** both native Linux package jobs pass archive/reproducibility checks and create a healthy-looking workspace, then fail SSH attach with `postAttachCommand failed: cannot read lifecycle manifest: No such file or directory (os error 2)`. The installed fixture defines no lifecycle hooks. Later operational assertions remain unverified and Linux package uploads are skipped downstream.
- **82 — Shared action runtime maintenance:** the release workflow repeats the existing Node.js runtime/dependency warnings; evidence was added to issue 82 rather than creating a duplicate.

Static agent builds/assembly and macOS packaging pass, but the latter does not execute Docker Desktop operational smoke. Draft release preparation is tag-only and this run is a main-branch push; its skipped state is not independently a publication failure. Third-party build/probe and cleanup messages are not additional demonstrated primary failures. Relevant excerpts/run identifiers are retained in the issues before deleting the local `ci.log` and `release.log` files. No acceptance checkbox is closed by this triage.

### Completed so far (`11b0651`, `63a6815`, `20db16f`, `65b6375`, `2af3ec2`; `Refs: 78`)

- Added the named opt-in `credentials` integration suite to strict nonzero discovery, exact discovered/passed-count enforcement, locked release execution, coverage inventory, and Linux CI. It now discovers three tests.
- Added packaged public-command workflows that validate embedded artifacts and exercise staged credential enable, explicit-name create, credential readiness before foreground/detached hooks, pre-SSH `postAttachCommand`, PTY/non-PTY and concurrent managed SSH, zero-port ownership, status, down/up, rebuild, controlled supervisor-loss recovery, live disable, old-session/new-child isolation, and absence of the revoked socket in a new SSH child. The SSH fixture uses a controlled real host `ssh-agent` with an Ed25519 key, verifies a real identity-backed signature, refreshes after a same-path agent restart, observes unavailable-backend failure, and verifies recovery without restarting the workspace.
- Added a bounded controlled smart-HTTP TLS server with a private test CA and hostname-valid certificate. The packaged workflow performs real first-hook recursive HTTPS clone, fetch, push, two path/account lookups on one origin, private-submodule authentication, explicit stale-token failure and recovery, uncached token rotation, denied-origin checks, native-helper get/store/erase isolation with byte-preserved Git configuration, and secret-token scans without disabling certificate verification or contacting a public Git service.
- Added `tests/integration/fixtures/credential-coverage.json`, mapping issue-68 criteria to component/integration evidence and recording unverified criteria as blockers rather than passing evidence.
- Restored Linux x86_64 support alongside arm64 for embedded static agents, host architecture selection, package/artifact validation, installer selection, release packaging, CI release matrices, integration architecture checks, and coverage inventory.
- Expanded the declared Git/OpenSSH/helper compatibility matrix and noninteractive/provider limitations in the operations documentation.
- Revised the Apple-silicon Docker Desktop checklist to require credential HTTPS/keychain, SSH-agent refresh/lifetime, revocation, and secret-surface observations. This is a checklist only; it is not a completed smoke record.
- `cargo xtask check`, strict Clippy, xtask tests, installer tests, coverage-plumbing tests, and the traceability test pass. Both static agent architectures and an arm64 packaged host were built and artifact-validated locally.
- The current credential target discovers exactly three tests. The TLS certificate chain and controlled smart-HTTP server were directly validated locally, including authenticated `ls-remote` and push. This component validation is not a substitute for a passing packaged Docker workflow.
- Triaged the first pushed credential-gate run (`CI` 35471232827 and `Release packages` 35471232818). The credential suites did not execute: release-gate jobs attempted the opposite-architecture agent without QEMU, macOS exposed a Linux-only managed-environment call, and installed smoke discarded the workflow-selected Docker/OpenSSH paths. Native agent artifacts are now assembled once for all release-gate jobs, the unsupported-host path fails closed at runtime while compiling on macOS, and installed smoke retains selected tool directories while shadowing forbidden tools. The macOS Bash 3 empty-array coverage-report failure was also removed. These are fixes awaiting a clean rerun, not passing evidence.

### Remaining implementation plan (2026-11-09)

The November 9 baseline and local follow-up above supersede the historical failure
reports. Local diagnostic credential workflows now pass 3/3, but their explicit
prior build identity and mixed agent artifacts do not establish release readiness.
The following work packages remain; retain the acceptance criteria below until
all applicable implementation and platform evidence is complete.

#### 1. Establish a clean CI baseline for the reviewed fixes

- After review and push of the local fixes through `67f1b65`, build the host and both native static-agent artifacts from the same clean, locked source revision and normal build identity. Do not reuse the diagnostic archive as release evidence.
- Inspect CI and installed-package release jobs on native Linux x86_64 and arm64, including minimum and pinned dependency lanes, macOS workspace tests, and coverage inventory. Record run links, source SHA, artifact identities/checksums, actual architecture/dependency versions, and discovered/executed/passed counts. The current credential suite has three tests; update expected counts when expanding it.
- Confirm artifact reuse/cache behavior and bounded failure propagation from actual job timings: retain build/test durations, cache outcomes, and failure diagnostics. A failed build must terminate dependent work rather than leave a stalled release job. Fix new failures in the responsible component without relaxing the gates.
- **Exit evidence:** passing clean baseline jobs, or explicitly identified remaining failures with retained logs. This establishes the starting point; repeat the complete matrix after subsequent implementation changes.

#### 2. Complete active detached-lifecycle cancellation

Implementation audit (2026-11-10): production `execute_lifecycle` still launches
concatenated shell commands through detached Docker Exec and
`run-managed-environment`. The agent's separate `run_lifecycle` runner is not a
drop-in replacement: its request lacks installation/workspace/container/per-run
identity, its paths and terminal checkpoints are permanently generation-scoped,
and it does not enroll credential grants as `run_post_attach` does. Reusing it
would skip subsequent same-generation `postStartCommand` runs and lose credential
availability. The host execution record identifies the exact Exec but has no
per-run manifest identity or cancellation request/acknowledgment contract.

The implementation therefore needs coordinated agent request/manifest and host
record changes, credential enrollment in the owned runner, per-start paths,
authenticated cancellation and cleanup acknowledgment, authoritative final Exec
verification, and distinct checkpoint outcomes for cancelled one-time work versus
repeatable post-start work. Routing production directly to the existing cancel
API or signaling a recorded PID is insufficient.

Local safety follow-up removes timeout signaling through a persisted runner PID
(which can be reused) and verifies the immutable plan before writing a cancel
marker. Named regressions are `cancellation_timeout_does_not_signal_a_persisted_pid`
and `mismatched_cancellation_does_not_publish_a_cancel_request`. They are Linux-only;
macOS compilation/zero-test execution is not passing runtime evidence. These
changes preserve an active/indeterminate outcome on timeout and do not complete
active detached cancellation.

- Extend the lifecycle runner/host supervision contract with durable per-start/run identity, bound to installation, workspace, container, generation, build/protocol, and execution ownership. Preserve rerunning `postStartCommand` after down/up within the same generation; the existing immutable per-generation runner cannot simply be reused for this.
- Implement authenticated cancellation of an active owned execution with bounded graceful/forced cleanup and a precise verified outcome. Keep the completed-Exec inspection path, and keep missing, mismatched, unstarted, legacy, or unverifiable records fail-closed. Do not clear a running checkpoint or guess a process identity to make down/rebuild succeed.
- Cover completed and active work, failed/missing execution, identity mismatch, launch/crash recovery, repeated same-generation starts, and down/rebuild while detached work is active. Verify child cleanup and preservation of unrelated processes/containers.
- **Exit evidence:** component regressions plus packaged public-command tests demonstrate safe active cancellation and repeatable lifecycle execution. Record any remaining indeterminate cases and their recovery path.

#### 3. Finish controlled Git-over-SSH and agent-selection workflows

- Add a controlled authenticated Git-over-SSH server and real clone/fetch/push with pinned host-key trust. Assert unknown/changed host keys fail; never disable host-key verification or depend on a public service.
- Extend packaged tests for empty agents, explicit versus automatic selection, changed automatic socket refresh, and confirmation/timeout behavior, alongside existing signing, same-path restart, unavailable-backend recovery, and concurrency checks. Verify independent identity-default precedence/removal without changing signing semantics.
- Label simulated confirmation/provider behavior explicitly; reserve claims about real host integrations for the macOS observation below.
- **Exit evidence:** named packaged scenarios pass with bounded waits and strict discovery counts; traceability identifies each scenario instead of treating generic agent signing as Git-over-SSH evidence.

#### 4. Finish revocation, failure, and replacement scenarios

- Exercise revocation during an in-flight credential operation and from old clients, with explicit synchronization so the test proves the operation was active. Assert the supported revocation boundary without claiming recall of already delivered credentials, signatures, or connections.
- Distinguish degraded host backends from a failed/not-ready bridge and verify recovery, diagnostics, and failure propagation through public commands. Extend rejected-open/cancellation race coverage, including local close before peer error, without weakening unknown-stream rejection.
- Add failed rebuild/rollback and credential-enabled Compose partial replacement. Verify generation ownership, readiness, checkout/named-volume preservation, isolation from unrelated containers, and whether old/new sessions may obtain credentials at each transition.
- **Dependencies:** coordinate active-lifecycle scenarios with work package 2; other fixtures can proceed independently.
- **Exit evidence:** deterministic packaged regressions cover successful recovery and fail-closed paths, including supervisor loss, with no silently skipped or indefinitely waiting operations.

#### 5. Complete adversarial and secret-surface evidence

- Map malformed/truncated/oversized frames, identity/build/protocol/revision mismatches, unsafe paths, injection, cancellation races, concurrency, and backpressure to specific component tests. Add missing regressions at the responsible layer rather than relying solely on a release script.
- Expand unique test-marker checks across intended private recipients and all managed persistence, logs/debug/errors/JSON, process arguments/environment, snapshots, helper configuration, and native container helper storage. Verify expected delivery as well as absence from prohibited surfaces; include failure/revocation paths and keep live personal secrets out of fixtures and retained evidence.
- Audit `tests/integration/fixtures/credential-coverage.json` against issue 68 criterion by criterion. Distinguish configured coverage, passing component evidence, packaged observations, and platform verification; remove unsupported verified claims and retain explicit blockers.
- **Exit evidence:** each required surface and adversarial case has a named test or justified platform observation, and every parent criterion has an accurate evidence reference or an unresolved blocker.

#### 6. Record actual Apple-silicon macOS / Docker Desktop smoke

- Execute `docs/smoke/macos-docker-desktop-v1.md` revision 2 on the macOS host using the candidate archive, real host keychain/helper behavior, and selected/automatic SSH agents. Cover private Exec transport, HTTPS authentication, refresh/lifetime, revocation, and the checklist's secret-surface observations.
- Retain hardware, OS/build, Docker Desktop and other dependency versions, source revision, archive/checksum, date, scenario outcomes, and exactly which provider/confirmation interactions were real or simulated. Record required human interaction or unavailable observations as pending evidence, not passes.
- **Dependencies:** use the candidate containing the relevant fixes and expanded scenarios. Linux containers accessing Docker Desktop cannot substitute for host-helper validation.
- **Exit evidence:** a completed, reviewable smoke record with all required observations passing; rerun affected observations if the candidate changes.

#### 7. Reconcile documentation and run the final release gates

- Update ADR 0002, operations/profile support, implementation-plan/runtime descriptions, release packaging, and smoke guidance to the implemented guarantees. Publish minimum/tested Git, OpenSSH, and helper versions and provider/noninteractive limitations.
- Preserve explicit opt-in and managed-process-only scope, neutral host-context/clone-portability limits, host reauthentication and host-key/agent remediation, socket refresh, and accurate cancellation/revocation limits. Do not claim arbitrary helpers cannot display a GUI or that raw agent access imposes signing restrictions.
- After packages 2–5 and documentation changes, rerun clean locked profile/OpenSSH/credential, preservation, installed-package, release/agent/control compatibility, quality/dependency, and coverage gates across the required architecture/dependency matrix. Retain exact counts (nonzero, discovered = executed = passed), versions, checksums, source revisions, run links, and failure diagnostics. Missing prerequisites, skips, or wrong architectures must fail required gates.
- Reconcile the traceability manifest with those final runs and the macOS smoke record, identifying evidence revisions and rerunning affected checks after any further fix. Check acceptance items only when their entire scope is demonstrated; resolve issue 78 only after all its acceptance criteria pass. Issue 68 still requires its separate final contract review.

**Execution order:** start with package 1; packages 2, 3, and the independent parts
of 4/5 can proceed in parallel with separate file ownership. Integrate runtime
changes before finalizing dependent lifecycle tests. Finish package 5 against the
combined implementation, then complete platform observations and final evidence
in 6/7. Source and issue commits remain local until the user's review/push.

## Work

- Add opt-in integration coverage using actual host/container Git and OpenSSH with controlled authenticated HTTPS/SSH fixtures. Use proper TLS CA/hostname trust and SSH host-key verification; no verification bypass, public-network dependency, live personal credentials, or provider-specific container runtime is an acceptable shortcut.
- Exercise the public staged-enable/create/up/ssh/credentials/down/rebuild command paths and packaged host/static-agent artifacts, not only library fakes or manually fabricated ready state.
- Cover HTTPS fetch/push, token rotation, two repository paths/accounts on one origin, private submodules and denied additional origins, lookup-only store/erase behavior, and full native-helper lookup/approve/store isolation with byte-preserved underlying configuration.
- Cover SSH-agent concurrency, unavailable/empty agents, explicit/automatic selection and refresh, same-path agent restart, confirmation/timeouts, and independent identity-default precedence/removal without signing changes.
- Cover first foreground hook, detached work, postAttach, PTY/non-PTY sessions, zero-port/zero-client ownership, revocation of in-flight/old-client operations, reconnect/supervisor loss, down/up, rebuild success/rollback, and Compose partial replacement.
- Consolidate adversarial and secret-marker evidence from the component issues: malformed/truncated/oversized frames, all identity/build/protocol/revision mismatches, unsafe paths, injection attempts, cancellation, concurrency/backpressure, and all cdenv-managed persistence/diagnostic/argument/environment/helper-storage surfaces. Do not defer basic safety tests solely to a release script.
- Add the credential suite to strict nonempty discovery/execution enforcement and CI for supported Linux x86_64 and arm64 with declared minimum/tested dependencies. Missing prerequisites, unavailable/empty suites, skipped tests, or wrong declared architecture must fail the required gate rather than count as success.
- Record a real macOS Apple-silicon Docker Desktop smoke for private Exec transport, host keychain/helper behavior, selected/automatic SSH-agent refresh, and relevant revocation/lifetime behavior. Linux-container access to Docker Desktop is not a macOS host-helper test. Record hardware/OS/dependency versions, archive/checksum, date, and outcome.
- Publish the minimum/tested host and container Git/OpenSSH/helper matrix and exactly which provider/confirmation interactions are simulated versus verified. Test/document supported noninteractive helper behavior without claiming arbitrary helpers cannot display a GUI.
- Update ADR 0002, operations/profile support, implementation-plan/runtime descriptions, release packaging and smoke guidance to the actual implemented guarantees. Retain explicit opt-in, managed-process-only scope, neutral host-context/clone-portability limits, host reauthentication and host-key/agent remediation, socket refresh, and revocation limitations.

## Acceptance criteria

- [ ] A named opt-in credential suite discovers and executes a nonzero, matching test count; ordinary workspace tests still need no Docker, public network, keychain, or real credentials.
- [ ] Controlled real Git HTTPS/SSH and packaged public-command workflows satisfy the parent acceptance scenarios, with correct TLS/host-key trust and no host credential/private-key/socket mounts or copied stores.
- [ ] A traceability record maps every issue-68 acceptance criterion to a concrete component/integration test or recorded platform observation. Unverified requirements remain explicit blockers rather than being treated as equivalent to passing parser or generic SSH tests.
- [ ] Secret markers appear only in intended private transports/recipients and never in cdenv persistence, logs/debug/error/JSON diagnostics, arguments/environment, snapshots, helper configuration, or native container credential storage.
- [ ] Linux x86_64 and arm64 required gates pass with declared dependency/fixture/architecture enforcement; required macOS smoke is recorded, not silently skipped or inferred from another platform.
- [ ] `cargo xtask check`, existing profile/OpenSSH interoperability, checkout/named-volume preservation, installed workflows, and release/agent/control protocol compatibility gates pass from the appropriate clean locked release builds.
- [ ] Documentation no longer advertises only a permission foundation once the runtime is actually complete, and does not claim unsupported host Git/SSH/provider portability, signing restrictions on raw agent access, or recall of delivered credentials/signatures/connections.

## Closure boundary

This issue owns the final test/release evidence and documentation, not permission to weaken failing runtime requirements. Fix failures in the responsible component/integration issue. Issue 68 remains the umbrella for final contract review and is resolved separately only when its complete agreed acceptance criteria are met.
