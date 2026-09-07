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
