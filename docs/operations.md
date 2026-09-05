# cdenv V1 operations guide

cdenv implements only the pinned [`cdenv-devcontainer-v1`](cdenv-devcontainer-v1-support.md)
profile. It does not launch editors, update itself, use remote Docker daemons, install `sshd`, or
publish SSH port 22.

## Install and remove

Download the archive for the host OS/architecture, verify its adjacent `.sha256` file, extract it,
and place `cdenv` on `PATH`. `cargo xtask dist` produces a release archive and checksum after
building the two verified Linux agent artifacts and embedding them in the host binary.

To remove cdenv, run `cdenv down <name>` for every workspace, remove the installed binary, then
remove the managed root. This removes cdenv state and cached/generated images but never removes a
checkout unless the user removes it separately.

## Dependencies

Linux x86_64 and arm64 are the complete automated release platforms. Docker Engine/CLI 29.6.2+
(API 1.55), Docker Compose V2 5.3.1+, and OpenSSH client 10.0p2+ are the verified baseline.
Docker must use a local Unix socket. Git is required for `create`. Node.js and an editor are not
runtime dependencies.

Set `CDENV_HOME` or pass `--root PATH` to select an absolute cdenv root. It contains installation
identity, workspace state, keys, forwarding control files, and cache data. The root is private;
state schema upgrades are explicit compatibility migrations, never a side effect of `doctor`,
`list`, `status`, or SSH connection.

## Trust and state

Only `cdenv lock` writes a Feature lockfile in a checkout. Public OCI and HTTPS Features are
validated with TLS, digest, size, and archive-path limits. Private registries, credentials in
Feature URLs, insecure HTTP, traversal archives, and unchecked redirects are rejected. Docker
image credentials are not Feature credentials.

`cdenv doctor` is diagnostic only: it does not chmod, migrate, regenerate SSH files, start/stop
containers, or repair a forwarding supervisor. Use `cdenv up` or `cdenv rebuild` for explicit
repair/reconciliation. `doctor --json` emits one versioned JSON document and is suitable for
support collection.

## SSH and forwarding

cdenv creates explicit per-workspace OpenSSH configuration and asks before adding an `Include` to
the user SSH configuration. Declining consent does not prevent `cdenv ssh <name>` or an explicit
`ssh -F <root>/ssh/config <name>.cdenv`. Host and client keys are private, and each connection is
an authenticated stdio transport to the container agent; no daemon is installed.

`forwardPorts` are managed loopback listeners with workspace-scoped lifetime. `cdenv forward`
is foreground-only and rejects listener conflicts. Binding a forward beyond loopback requires an
explicit `--bind` and is intentionally warned because it exposes the service to other hosts.

## Recovery and troubleshooting

Use `cdenv status <name>` for workspace state and `cdenv doctor` for independent local checks.
If an interrupted lifecycle operation is indeterminate, rebuild the workspace rather than editing
state. If Docker, Compose, OpenSSH, or Git is missing, install the declared dependency and rerun
the command; cdenv never substitutes an unverified implementation. Preserve checkouts and named
volumes during ordinary down/up and rebuild flows; cleanup retention applies only to cdenv-generated
images and caches after their verified references are no longer retained.

Before a macOS release, run the Docker Desktop smoke checklist on Apple Silicon:
create an image workspace, run `up/down/up`, SSH and PTY/forward it, rebuild it,
run `doctor`, and verify no port 22 or `sshd` exists. Record the hardware, Docker Desktop version,
and result with the release. Editor observations are compatibility notes, never release gates.
