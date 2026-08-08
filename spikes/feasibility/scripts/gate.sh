#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
STARTED=$(date +%s)

for dependency in cargo rustc git docker ssh ssh-keygen openssl curl tar gzip sha256sum tmux readelf; do
  command -v "$dependency" >/dev/null || {
    echo "missing declared feasibility-gate dependency: $dependency" >&2
    exit 1
  }
done
docker info >/dev/null
docker compose version >/dev/null
docker buildx version >/dev/null

cargo fmt --manifest-path "$ROOT/Cargo.toml" -- --check
cargo clippy --manifest-path "$ROOT/Cargo.toml" \
  --all-targets --all-features --locked -- -D warnings
cargo test --manifest-path "$ROOT/Cargo.toml" --locked
RUSTDOCFLAGS='-D warnings' cargo doc --manifest-path "$ROOT/Cargo.toml" --no-deps --locked
cargo run --manifest-path "$ROOT/Cargo.toml" --locked \
  --bin feasibility-spike -- baseline

"$ROOT/scripts/profile-integration.sh"
"$ROOT/scripts/agent-cross-build.sh"
"$ROOT/scripts/docker-build-matrix.sh"
"$ROOT/scripts/docker-compose-integration.sh"
"$ROOT/scripts/ssh-integration.sh"
"$ROOT/scripts/quarantine-check.sh"

elapsed=$(( $(date +%s) - STARTED ))
printf 'all feasibility gates passed in %ss\n' "$elapsed"
