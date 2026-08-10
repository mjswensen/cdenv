# Vendored Dev Container schema

`devContainer.base.schema.json` is unmodified from
[`devcontainers/spec@c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421`](https://github.com/devcontainers/spec/commit/c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421).
Its SHA-256 is `a0883c0405ff433db188849d458fb20b9c0d73e0ba1a6e44c1d83f3b485408dd`.

The accepted profile ADR is [`docs/adr/0001-cdenv-devcontainer-v1-feasibility.md`](../../../docs/adr/0001-cdenv-devcontainer-v1-feasibility.md).
Runtime validation uses this base schema offline and never follows user or schema network references. `build.rs` rejects a checksum mismatch at compile time.
