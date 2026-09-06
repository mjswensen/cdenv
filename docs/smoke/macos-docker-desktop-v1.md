# macOS Docker Desktop smoke checklist — V1

Checklist revision: **1**. Complete and retain one copy of this document for every
release candidate. This is a recorded Apple Silicon observation, not an automated
release guarantee. Editor observations are optional, non-gating notes.

## Required record

| Field | Recorded value |
|---|---|
| Checklist revision | 1 |
| Release/date (UTC) | |
| Tester | |
| Mac model/hardware | |
| Host architecture (`uname -m`, must be `arm64`) | |
| macOS version/build (`sw_vers`) | |
| Docker Desktop version | |
| Docker Engine/API version | |
| Docker CLI version | |
| Compose V2 version | |
| OpenSSH version | |
| cdenv archive filename | |
| cdenv archive SHA-256 | |
| Published checksum verified | pass / fail |
| Overall outcome | pass / fail |

A missing field, an Intel host, a failed checksum, or a failed required step is a
failed smoke record. Attach command output with secrets and environment values
redacted.

## Preconditions

- Use Apple Silicon and a local Docker Desktop Unix socket.
- Start from the release archive and adjacent checksum produced by the release
  workflow; do not substitute a development binary.
- Use the dependency baselines in the [operations guide](../operations.md).
- Set a new absolute `CDENV_HOME`, decline SSH Include modification, and ensure
  Node.js and an editor are not needed for any required step.

## Required workflow

1. Verify the adjacent SHA-256, extract the archive, put only its `cdenv` on
   `PATH`, and run `cdenv --version` plus `cdenv __validate-artifacts`.
2. Create separate image, Dockerfile-with-Feature, and two-service Compose V2
   fixture repositories. Run `create` for each and record `list`, `list --json`,
   `status`, and `status --json`.
3. For the image fixture, run `down`, `up`, `rebuild`, and `rebuild --no-cache`.
   Confirm tracked and untracked source changes survive each operation.
4. Run `lock` for the Feature fixture, verify a second invocation is stable, and
   confirm stale or digest-mismatched lock input fails closed.
5. Run a non-PTY command with `cdenv ssh`, the same command with
   `ssh -F "$CDENV_HOME/ssh/config" <name>.cdenv`, and an interactive PTY. Verify
   stdout, stderr, exit status, terminal resize, and Ctrl+C.
6. Exercise a declared loopback forward through `up`, stop/restart the target,
   and confirm recovery. Exercise foreground `cdenv forward`; confirm Ctrl+C
   removes it. Record the warning from one explicit non-loopback bind without
   leaving that listener running.
7. For Compose, confirm project isolation, the primary/dependency managed set,
   complete stop, named-volume preservation, and successful rebuild recovery.
8. Run `doctor`, `doctor --json`, and an interrupted-operation recovery case.
   Confirm diagnostic commands do not repair or migrate state.
9. Inspect every cdenv container: no port `22/tcp` is published, no `sshd`
   executable/process exists, and SSH still works through the stdio proxy.
10. Run `down` for every fixture. Confirm scoped forwarding supervisors stop;
    then apply the documented retention cleanup and uninstall procedure.

## V1 boundaries and optional observations

Record that the smoke made no use of an editor, Node.js, remote Docker, Compose
V1, an in-container SSH daemon, automatic upgrades, or behavior outside
[`cdenv-devcontainer-v1`](../cdenv-devcontainer-v1-support.md). These are V1
non-goals, not omitted tests.

Optional editor name/version and observed result:

> Not tested / observation only:

Editor results never change the required outcome above.
