# 15 — Define and validate the V1 raw profile

**Parent phase:** [Implementation plan §11.2 and Chunk 4](../implementation-plan.md#112-v1-support-matrix)  
**Depends on:** [14](14-config-discovery-and-jsonc.md)

## Goal

Convert parsed JSONC into typed scenario/profile inputs and fail closed on behavior cdenv cannot honor.

## Work

- Define pinned raw models for image, Dockerfile, and Compose V2 scenarios and all V1-supported top-level properties listed in §11.2.
- Validate scenario exclusivity/required fields, Compose `service`/`runServices`, command forms, Feature references, users, mounts, environment, ports, lifecycle, host requirements, and supported behavioral values at the raw level.
- Reject unknown top-level behavioral properties and known unsupported values with exact property paths and the profile revision.
- Accept `$schema`, arbitrary `customizations.<tool>` objects, and advisory `secrets` metadata without interpreting unsupported tools.
- Reject legacy Compose V1, non-Docker orchestrators, deprecated/private/insecure Feature forms, and other explicitly excluded V1 behavior.
- Expose a deterministic profile capability/support report derived from the same definitions, avoiding a second hand-maintained interpretation table where practical.

Do not perform metadata merges, variable substitution, source downloads, or host capability checks.

## Rust guidance

Load the `rust-best-practices` skill. Use focused enums/newtypes instead of stringly typed flags, avoid oversized enum variants, document public invariants/errors, and tolerate a little validation duplication rather than inventing a generic schema framework.

## Acceptance criteria

- Table-driven fixture tests cover every supported property family and every deliberate unsupported interpretation in the support matrix.
- Unknown/unsupported tests assert exact property paths, values, and profile revision.
- Image, Dockerfile, and Compose inputs cannot produce structurally invalid scenario models.
- Capability output is deterministic and has a small reviewed snapshot tied to the vendored profile revision.
- No I/O dependency enters `cdenv-devcontainer`; all standard workspace quality commands pass.