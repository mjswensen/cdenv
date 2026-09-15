---
id: 78
created: 2026-09-07
depends-on:
  - 77
---

# Gate credential workflows security and cross-platform release compatibility

**Split from:** 68, primarily section 9 and the documentation/compatibility work in section 8.

## Goal

Provide reproducible, nonempty end-to-end and release evidence for the complete workspace credential product. Existing generic profile/OpenSSH tests and the permission/parser foundation's passing Linux arm64 tests are not evidence that live credential forwarding works.

## Implementation status

_Last reconciled: 2026-09-15._

### Completed so far (`11b0651`, `63a6815`, `20db16f`, `65b6375`; `Refs: 78`)

- Added the named opt-in `credentials` integration suite to strict nonzero discovery, exact discovered/passed-count enforcement, locked release execution, coverage inventory, and Linux CI. It now discovers three tests.
- Added packaged public-command workflows that validate embedded artifacts and exercise staged credential enable, explicit-name create, credential readiness before foreground/detached hooks, pre-SSH `postAttachCommand`, PTY/non-PTY and concurrent managed SSH, zero-port ownership, status, down/up, rebuild, controlled supervisor-loss recovery, live disable, old-session/new-child isolation, and absence of the revoked socket in a new SSH child. The SSH fixture uses a controlled real host `ssh-agent` with an Ed25519 key, verifies a real identity-backed signature, refreshes after a same-path agent restart, observes unavailable-backend failure, and verifies recovery without restarting the workspace.
- Added a bounded controlled smart-HTTP TLS server with a private test CA and hostname-valid certificate. The packaged workflow performs real first-hook recursive HTTPS clone, fetch, push, two path/account lookups on one origin, private-submodule authentication, explicit stale-token failure and recovery, uncached token rotation, denied-origin checks, native-helper get/store/erase isolation with byte-preserved Git configuration, and secret-token scans without disabling certificate verification or contacting a public Git service.
- Added `tests/integration/fixtures/credential-coverage.json`, mapping issue-68 criteria to component/integration evidence and recording unverified criteria as blockers rather than passing evidence.
- Restored Linux x86_64 support alongside arm64 for embedded static agents, host architecture selection, package/artifact validation, installer selection, release packaging, CI release matrices, integration architecture checks, and coverage inventory.
- Expanded the declared Git/OpenSSH/helper compatibility matrix and noninteractive/provider limitations in the operations documentation.
- Revised the Apple-silicon Docker Desktop checklist to require credential HTTPS/keychain, SSH-agent refresh/lifetime, revocation, and secret-surface observations. This is a checklist only; it is not a completed smoke record.
- `cargo xtask check`, strict Clippy, xtask tests, installer tests, coverage-plumbing tests, and the traceability test pass. Both static agent architectures and an arm64 packaged host were built and artifact-validated locally.
- The current credential target discovers exactly three tests. The TLS certificate chain and controlled smart-HTTP server were directly validated locally, including authenticated `ls-remote` and push. This component validation is not a substitute for a passing packaged Docker workflow.

### Remaining implementation and evidence required before closure

- Add controlled Git-over-SSH evidence with strict host-key verification. The selected agent now performs a real identity-backed signing operation and packaged same-path restart/unavailable-backend recovery; empty agents, confirmation timeout behavior, explicit versus automatic selection, and changed automatic socket refresh still need packaged evidence.
- Expand packaged lifecycle coverage from the implemented detached work, PTY/non-PTY sessions, supervisor-loss recovery, rebuild success, and old-session/new-child isolation to in-flight credential-operation revocation, backend-degraded versus bridge-readiness failure, rebuild rollback, and credential-enabled Compose partial replacement.
- Complete the secret-marker audit across intended private recipients plus persistence, logs, diagnostics/errors/JSON, process arguments and environment, snapshots, helper configuration, and native container helper storage.
- Obtain passing required credential-suite runs on clean native Linux x86_64 and arm64 minimum and pinned dependency jobs. The current suite discovers three tests. Local nested-container attempts have confirmed strict discovery but could not provide release evidence: one lacked a daemon-shared temporary namespace, and a later run intentionally using a pre-fix packaged binary failed the permission-safety gate before workflow execution. Neither failure is a skip or passing observation.
- Run and retain the revision-2 smoke record on actual Apple-silicon macOS with Docker Desktop and real host keychain/helper behavior. Record hardware, OS/build, all dependency versions, archive/checksum, date, simulated-versus-real provider/confirmation interactions, and outcome. Linux access to Docker Desktop cannot satisfy this item.
- Update the traceability blockers to verified evidence only after those observations pass, then rerun all profile/OpenSSH/credential, preservation, installed-package, protocol compatibility, and `cargo xtask check` gates from clean locked release builds.
- Keep every acceptance checkbox open until its complete scenario and required platform observations pass; configured tests and CI jobs are not completed release evidence.

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
