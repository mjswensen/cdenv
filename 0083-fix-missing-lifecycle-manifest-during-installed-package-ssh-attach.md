---
id: 83
created: 2026-09-26
depends-on: []
---

# Fix missing lifecycle manifest during installed package SSH attach

## Category

Packaged runtime / lifecycle-to-SSH integration. Blocks issue 78. One failure category shared by both native Linux release architectures, distinct from issue 80's socket-path failures.

## Evidence

Release packages run [36219272470](https://github.com/mjswensen/cdenv/actions/runs/36219272470), 2026-09-26, source revision `6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6`.

Both `Package linux-x86_64` (job `108341809947`) and `Package linux-aarch64` (job `108341809955`) pass locked archive builds, reproducibility checks, package-only smoke, Docker/Compose setup, and baseline OpenSSH setup. They fail `Run checksum-verified installed operational smoke`, invoking:

```sh
cargo xtask test-installed target/dist/cdenv-linux-<architecture>-6e15e53f1879c1d3fd6c47f4b6b040ca82a761c6.tar
```

The archive checksum is OK. Create succeeds, list/status report a running, idle, locally valid workspace with lifecycle complete through `updateContentCommand`, and doctor reports every check passing. Credentials are disabled. SSH then emits:

```text
cdenv: workspace `installed-smoke` is unavailable (postAttachCommand failed; a later connection will retry); run `cdenv up installed-smoke`
cdenv-agent: postAttachCommand failed: cannot read lifecycle manifest: No such file or directory (os error 2)
Connection closed by UNKNOWN port 65535
```

The smoke exits 1. Original local log locations: `release.log:2521-2567` (x86_64) and `5388-5435` (aarch64). The log is removed after triage; this excerpt and the run/job identifiers preserve the relevant evidence.

`tests/release/installed-smoke.sh` uses an `alpine:3.22` image fixture with a workspace folder and named-volume mount but no lifecycle hooks. After create/list/status/doctor/lock, its first SSH assertion is:

```sh
cdenv --root "$root" ssh "$workspace" -- printf installed-ssh | grep -qx installed-ssh
```

Direct OpenSSH, forwarding, down/up, rebuild, and preservation assertions follow that command and are not established as passing by this run. The log does not identify the missing manifest path or prove whether manifest creation, persistence, or lookup is wrong.

## Work

- Trace lifecycle manifest creation and container provisioning during create, plus the manifest path/identity passed into pre-SSH postAttach execution.
- Define correct no-hook behavior: a valid configuration without `postAttachCommand` must allow SSH without requiring nonexistent hook state. Do not blanket-ignore missing manifests when configured hooks require them.
- Preserve required postAttach execution before the session and fail-closed handling for invalid/missing required lifecycle state, with actionable diagnostics and safe retry/recovery.
- Add regressions through public packaged commands for both no-hook and configured-hook workspaces, including reconnect and generation transitions as appropriate.
- Rerun the complete checksum-verified installed smoke on both native Linux architectures; retain its full result rather than counting archive-only checks as runtime evidence.

## Acceptance criteria

- [ ] Fresh no-hook workspaces support both `cdenv ssh` and direct OpenSSH without a missing-manifest error.
- [ ] Configured postAttach hooks run before session access; missing/corrupt required state fails safely and recovery is tested.
- [ ] `cargo xtask test-installed` passes in full for native Linux x86_64 and aarch64 from clean locked packaged builds, including forwarding, down/up, rebuild, and checkout/volume preservation.
- [ ] Both Linux package jobs reach their normal successful artifact uploads without weakening the installed operational gate.

## Triage boundaries

- Linux package uploads are skipped after the smoke failure; this is a downstream effect, not evidence of a separate upload defect.
- Draft release preparation is skipped on this `main` push; `.github/workflows/release.yml` restricts it to `v*` tags, so its skipped state alone is not a release-publication defect.
- Static agent builds/assembly and the macOS package job pass. The macOS job does not run installed Docker operational smoke and cannot satisfy issue 78's real Docker Desktop observation.
- Node.js runtime/dependency warnings are already tracked by issue 82, now with this run's evidence.
- OpenSSH compiler warnings, Docker's unsupported fsverity probe, deletion of absent nftables tables, and cleanup-time `removal ... is already in progress` messages do not demonstrate additional independent gate failures in this log. Revisit cleanup if reproduction shows leaked resources; do not confuse those later messages with the earlier manifest error.
