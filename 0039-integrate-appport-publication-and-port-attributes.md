---
id: 39
created: 2026-08-20
---

# Integrate `appPort` publication and port attributes

_Converted from [`implementation-chunks/39-app-port-publication.md`] on 2026-08-20._


**Parent phase:** [Implementation plan §11.9 and Chunk 10](https://github.com/mjswensen/cdenv/blob/main/implementation-plan.md#119-declared-ports-and-forwarding-supervisor)
**Depends on:** 18, 22, 29, 36, and 38

## Goal

Apply fixed create-time port publication safely and expose its semantics consistently.

## Work

- Translate numeric `appPort` values to `127.0.0.1:<port>:<port>` and preserve validated explicit Docker publication strings.
- Emit an explicit security warning for any non-loopback binding. Never turn an omitted address into non-loopback exposure.
- Apply publication during image container creation and through deterministic Compose overrides, then verify actual bindings through Bollard.
- Classify `appPort` changes as create drift; ordinary `up` must not recreate or silently republish an active generation.
- Render `label`/`protocol`/URL information where applicable and structured warnings for `openBrowser`, `openBrowserOnce`, and unsupported embedded preview behavior without launching UI.
- Keep automatic process/range/regex port discovery deferred and visible as a capability diagnostic.

## Rust guidance

Load the `rust-best-practices` skill. Reuse validated port newtypes, keep adapter translation pure, return typed verification mismatches, and test one binding/security behavior per case.

## Acceptance criteria

- Fake command/override tests prove exact loopback/default and explicit binding arguments.
- Real image and Compose tests inspect the daemon binding and reach a fixture service only through the requested interface.
- Non-loopback inputs always produce a human and JSON security warning.
- Changing `appPort` yields create drift and invokes no recreate during plain `up`.
- No browser/preview process launches; standard workspace quality commands pass.