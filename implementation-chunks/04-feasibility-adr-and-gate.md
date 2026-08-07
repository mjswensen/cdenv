# 04 — Record feasibility decisions and close the spike gate

**Parent phase:** [Implementation plan §5.4 and Chunk 0](../implementation-plan.md#54-spike-outputs)  
**Depends on:** [01](01-feasibility-profile-and-features.md), [02](02-feasibility-docker-compose-lifecycle.md), and [03](03-feasibility-ssh-packet-flow.md)

## Goal

Turn verified spike evidence into the fixed inputs for production implementation, then quarantine or delete disposable code.

## Work

- Write an ADR under `docs/adr/` recording the exact upstream specification commit, vendored schema identity, minimum verified Docker Engine/CLI and Compose V2 versions, and exact volatile Bollard/Russh versions and APIs.
- Publish the `cdenv-devcontainer-v1` property support matrix, including deliberate interpretations, limits, unsupported values, and the policy for additive versus semantic changes.
- Retain reviewed command lines, effective-plan/lock snapshots, fixture definitions, and black-box tests that remain useful without preserving spike abstractions.
- Record Feature, Compose discovery/isolation, lifecycle recovery, forwarding-supervisor, packet-flow, cancellation, multiplexing, PTY, and architecture findings.
- Resolve every criterion in [§5.3](../implementation-plan.md#53-spike-success-criteria) as passed or stop and revise the main plan. Do not waive unresolved profile behavior.
- Delete or clearly quarantine spike implementation code so production crates cannot depend on it.

## Acceptance criteria

- The ADR and support matrix identify exact revisions/versions and link to reproducible evidence for every feasibility criterion.
- A checklist or test command demonstrates all spike gates pass on the declared environment.
- Retained fixtures/tests do not require Node.js in normal CI and contain no credentials or effective secrets.
- A repository search confirms production manifests/modules do not depend on the disposable spike.
- The ADR explicitly authorizes starting production Chunk 1; otherwise subsequent chunks remain blocked.