---
id: 67
created: 2026-09-05
depends-on: []
---

# Investigate Socktainer compatibility as an Apple container-backed Docker endpoint

## Goal

Determine whether [Socktainer](https://github.com/socktainer/socktainer) can satisfy cdenv's Docker CLI, Compose V2, and Docker Engine API contracts over its Apple `container`-backed Unix socket without introducing a second runtime implementation in cdenv.

This issue is an evidence-gathering and compatibility-design task. It must not weaken the existing `cdenv-devcontainer-v1` guarantees merely to make a partial compatibility layer appear supported.

## Investigation

### Pin the test surface

- Record exact versions and provenance for macOS, Apple silicon hardware, Apple `container`, Socktainer, Docker CLI, Docker Compose, and the Engine API level Socktainer advertises.
- Install and run Socktainer through its documented Unix socket and test explicit `DOCKER_HOST=unix://...` selection before considering automatic discovery.
- Establish whether the existing Docker CLI and Bollard clients target exactly the same Socktainer socket for every test.
- Keep ordinary workspace tests Docker- and macOS-independent; add a named, non-empty integration entry point for environment-backed investigation.

### Exercise cdenv's exact adapter contracts

- Probe `/_ping` and `/version`, including server version parsing, minimum API negotiation, newer client behavior, and actionable diagnostics.
- Exercise public and authenticated image pulls with Docker's normal credential path.
- Exercise repository and generated-context builds, including `.dockerignore`, Dockerfile selection, target, build arguments, labels, `cacheFrom`, no-cache behavior, arbitrary accepted build options, `--iidfile`, full `sha256:` image claims, and generated-image label inspection.
- Exercise container creation with full synthetic IDs, names, identity labels, users, working directories, environment, commands, bind/named-volume mounts, mount options, init, capabilities, security options, privileged intent, GPU intent, fixed and ephemeral port publications, and loopback/non-loopback bindings.
- Exercise authoritative list/filter and inspect behavior through Bollard, including exact IDs, image IDs rather than references, labels, state, mounts, ports, architecture, duplicate/stale correlation, and container/image cleanup guards.
- Exercise start, graceful stop, delete, image delete, and any rename behavior actually required by reconciliation or rollback.
- Exercise archive upload into running shell-free/distroless containers, preserving directory/file/symlink behavior, modes, ownership expectations, and cdenv's tool-free provisioning contract.
- Exercise attached and detached Exec creation/inspection with the exact ID shape cdenv accepts, separated binary-clean stdout/stderr, bidirectional stdin, EOF, exit status, cancellation, TTY resize/signals, and payloads large enough to expose framing or buffering faults.
- Exercise the exact Compose commands cdenv uses: `config`, `build`, `images`, `up`, `ps`, and `stop`; verify project/service labels, managed dependency sets, networks, service DNS, volumes, isolation, duplicate-primary rejection, and drift-safe resume.

### Run complete cdenv workflows

- Run image, Dockerfile, Feature-generated-image, and Compose fixtures through create/up/down/up and rebuild/rollback.
- Verify agent upload and identity, lifecycle environment capture, lifecycle checkpoints, SSH non-PTY and PTY sessions, signals, direct TCP forwarding, declared forwarding supervisors, and post-attach behavior.
- Verify `list`, `status`, and read-only `doctor` behavior when Socktainer is healthy, absent, stopped, stale, or returns malformed/partial responses.
- Confirm checkout immutability, bounded output, cancellation cleanup, secret redaction, and operation-owned resource cleanup remain intact.

### Resolve compatibility and selection policy

Initial source review suggests the following hypotheses; verify them against a pinned release rather than treating them as established behavior:

- Socktainer advertises Docker Engine API 1.51 while cdenv currently requires API 1.55.
- Socktainer's server `Version` may be an API-shaped `v1.51` value rather than the three-component Engine version cdenv parses.
- Socktainer may return UUID-shaped Exec IDs while cdenv accepts only full lowercase hexadecimal IDs.
- Its build stream may not emit the Docker auxiliary image-ID result needed by `docker build --iidfile`.
- Archive upload into a running container may invoke `/bin/sh`, `mkdir`, `ln`, and `test`, conflicting with cdenv's shell-free provisioning contract.
- Container rename is currently stubbed, and some Docker options are approximated or ignored because Apple `container` has no exact equivalent.

For every confirmed gap, decide whether the correct resolution is a generic Socktainer fix, a generic cdenv interoperability improvement, an explicit unsupported-capability failure, or a new compatibility profile. Do not special-case malformed output or silently accept weaker semantics.

Define endpoint precedence and persistence before proposing automatic selection. Existing Docker-backed workspaces must not become invisible or attach to the wrong runtime merely because Socktainer is later installed. Explicit `DOCKER_HOST` must remain authoritative, stale sockets must not trigger unsafe fallback, and read-only commands must not start or mutate either runtime.

## Deliverables

- A checked-in compatibility report mapping every cdenv Docker CLI, Compose, and Bollard operation to reproducible pass/fail evidence and the relevant Socktainer route.
- A minimal macOS Apple-silicon integration harness or script that records exact dependency versions and cannot pass with zero executed tests.
- Upstream Socktainer issues or pull requests for generic compatibility defects, linked from the report.
- Follow-up cdenv issues split by independently reviewable changes, with estimates and explicit profile/state migration consequences.
- A go/no-go recommendation covering supported versions, residual semantic differences, automatic selection/fallback behavior, and whether Socktainer can remain an implementation detail behind cdenv's existing Docker boundary.

## Acceptance criteria

- The investigation runs against pinned dependencies on supported Apple-silicon macOS hardware and records a positive executed-test count.
- Image, Dockerfile, generated Feature image, and Compose workflows each have end-to-end results, including agent provisioning, lifecycle, SSH, forwarding, rebuild, down/up, and live reporting.
- Every operation and invariant listed above has reproducible evidence; confirmed gaps include minimal reproductions and an upstream/downstream disposition.
- Version/API probing and socket selection are designed from demonstrated capabilities rather than vendor-name spoofing or unconditional minimum-version relaxation.
- The recommendation explicitly addresses shell-free archive upload, build IID claims and labels, full container/image/Exec identities, attached Exec streaming, Compose service discovery, and unsupported Docker options.
- Existing Docker behavior, Linux release gates, read-only command guarantees, and the `cdenv-devcontainer-v1` contract remain unchanged unless a separately reviewed follow-up issue intentionally revises them.
