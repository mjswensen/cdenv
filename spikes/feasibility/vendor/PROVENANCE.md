# Vendored feasibility inputs

## Dev Container specification

- Repository: <https://github.com/devcontainers/spec>
- Commit: `c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421`
- Commit date: 2026-03-20
- Candidate profile name: `cdenv-devcontainer-v1`
- Unmodified inputs: `devcontainers-spec/schemas/` and the normative documents under `devcontainers-spec/docs/specs/`

The upstream repository has no test-fixture directory at this revision. The gate therefore vendors the schemas and normative examples, and keeps cdenv's behavior fixtures under `../../fixtures/`. This absence is recorded rather than inventing upstream provenance for local fixtures.

`devContainer.schema.json` at the selected specification commit contains mutable references to `microsoft/vscode/main`. Runtime validation must not follow them. The spike validates behavioral configuration against the unmodified, pinned `devContainer.base.schema.json`; it also vendors the two referenced editor schemas at a fixed VS Code commit so the original composite schema can be audited offline.

## VS Code editor schema closure

- Repository: <https://github.com/microsoft/vscode>
- Commit: `eb55ea151447d7607654d7046a18b37d3dca4704`
- Files: `vscode/schemas/devContainer.codespaces.schema.json` and `vscode/schemas/devContainer.vscode.schema.json`

These editor-specific inputs do not define cdenv profile behavior. They only close the mutable references present in the upstream aggregate schema for review.

## Integrity

From this directory, run:

```bash
sha256sum --check SHA256SUMS
```

`SHA256SUMS` covers every vendored byte and is itself reviewed in Git.
