---
id: 18
created: 2026-08-20
depends-on:
  - 17
---

# Plan ports and Docker-shaped options

_Converted from [`implementation-chunks/18-ports-and-docker-options-planning.md`] on 2026-08-20._

**Parent phase:** [Implementation plan §11.4 and §11.9](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#119-declared-ports-and-forwarding-supervisor)
**Legacy dependencies (untracked):** 15

## Goal

Turn port and Docker option properties into validated, immutable plan components without invoking Docker.

## Work

- Parse `appPort`, `forwardPorts`, `portsAttributes`, and `otherPortsAttributes` into requested publication/forwarding models with labels, protocol, `onAutoForward`, `requireLocalPort`, and target service/host semantics.
- Encode V1 interpretations: no process-based discovery; deferred regex/range triggers produce structured diagnostics; explicit `forwardPorts` wins over `onAutoForward: ignore`.
- Model numeric `appPort` as same-port loopback publication and retain valid explicit Docker publication strings, flagging non-loopback exposure.
- Model Dockerfile/context/build args/options, supported build properties, `runArgs`, and create settings.
- Parse arguments only far enough to reject conflicts with cdenv-owned labels, names, auto-remove, workspace/asset mounts, Dockerfile/context/result outputs, attach/TTY/stdin modes, and effective users. Preserve all other arguments and ordering.
- Return the exact conflicting argument/property; never silently override or reorder it.

## Rust guidance

Load the `rust-best-practices` skill. Prefer slices/borrows for argument scanning, clear loops for stateful option parsing, typed port/address values, and focused behavior tests.

## Acceptance criteria

- Tests cover numeric/string publications, IPv4/IPv6 forms as supported, zero/out-of-range ports, duplicates/conflicts, attributes, deferred-discovery warnings, and non-loopback warnings.
- Reserved-option tests cover split and `--key=value` forms and report the exact offending token.
- Non-reserved options round-trip byte-for-byte and in order.
- Small snapshots verify requested-versus-assigned rendering inputs without binding listeners.
- No subprocess/network access occurs and all workspace quality commands pass.