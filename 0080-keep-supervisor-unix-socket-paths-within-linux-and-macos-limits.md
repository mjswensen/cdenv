---
id: 80
created: 2026-09-26
depends-on: []
---

# Keep supervisor Unix socket paths within Linux and macOS limits

## Category

Cross-platform runtime/test path handling. Blocks issue 78. Group the repeated `SUN_LEN` failures together rather than filing an issue per test or matrix leg; investigate the production layout and fixture construction separately where needed.

## Evidence

CI run [36219272407](https://github.com/mjswensen/cdenv/actions/runs/36219272407), 2026-09-26, source revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`.

### macOS arm64 ordinary workspace coverage

`ci.log:6346-6372`: `workspace_registry::tests::supervisor_runtime_inspection_accepts_private_socket_and_files` panics at `crates/cdenv-cli/src/workspace_registry.rs:708:14`:

```text
supervisor socket should be bound: Error { kind: InvalidInput, message: "path must be shorter than SUN_LEN" }
```

The CLI library reports 208 passed / 1 failed; coverage exits 101. Canonicalizing the macOS temporary directory spelling has not prevented the overlong bind path. Reports are retained but do not constitute passing tests.

### Linux packaged credential workflows

All four native release jobs (x86_64/arm64 × minimum/pinned dependencies) discover three credential tests, then report one passed / two failed, exiting 101. The traceability test passes; both actual packaged workflows fail during create:

- `packaged_commands_cover_lifecycle_sessions_recovery_and_revocation`
- `packaged_git_https_paths_accounts_submodule_rotation_and_helper_isolation`

Both panic through `tests/integration/tests/credentials.rs:36:5`, with:

```text
cdenv: environment failed: environment reconciliation failed: forwarding supervisor control failed: path must be shorter than SUN_LEN
```

Evidence: `ci.log:11070-11106`, `13911-13947`, `16746-16782`, `19599-19635`. Fixture checkout paths combine `/tmp/cdenv-credential-workflow-<random>/cdenv-root/workspaces/` (or `cdenv-credential-https-<random>`) with long generated workspace names. The exact failing socket endpoint is not printed; identify it rather than assuming the checkout path itself is the socket.

## Work

- Trace supervisor bind/connect path construction and account for the platform's Unix-domain socket byte limit, including the complete root/name/suffix and multibyte paths.
- Make fixtures portable while addressing the real packaged-command limitation, not merely hiding it by shortening CI names. Define safe handling and actionable diagnostics for unsupported roots.
- Preserve private ownership/modes, symlink protections, workspace isolation, endpoint discovery, and cleanup if the socket layout changes. Do not relocate sockets into an unsafe shared directory.
- Add regressions for long roots/names and macOS temporary paths, covering both server and client endpoint selection.

## Acceptance criteria

- [ ] The failing registry test and full ordinary workspace coverage pass on native macOS arm64 without skipping the socket test.
- [ ] All three credential tests pass with exact discovery/execution enforcement on native Linux x86_64 and arm64, minimum and pinned jobs.
- [ ] Boundary/overlong path behavior is covered and documented; supported paths work and unsupported paths fail clearly without unsafe partial state.
- [ ] Permission, isolation, lifecycle/recovery, and socket cleanup checks remain enforced.
