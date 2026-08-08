#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REPO=$(cd "$ROOT/../.." && pwd)

metadata=$(cargo metadata --manifest-path "$ROOT/Cargo.toml" --locked --no-deps --format-version 1)
[[ "$metadata" == *"\"workspace_root\":\"$ROOT\""* ]] || {
  echo "feasibility harness is not rooted in its own Cargo workspace" >&2
  exit 1
}

# Production manifests and Rust modules may not point at disposable spike code.
production_paths=()
for candidate in "$REPO/Cargo.toml" "$REPO/crates" "$REPO/xtask" "$REPO/tests"; do
  [[ -e "$candidate" ]] && production_paths+=("$candidate")
done
if ((${#production_paths[@]})) && rg -n \
  'spikes/feasibility|cdenv-feasibility-spike|feasibility-agent' \
  "${production_paths[@]}" --glob '*.toml' --glob '*.rs' --glob '*.lock'; then
  echo "production code depends on the disposable feasibility harness" >&2
  exit 1
fi

if rg -n 'BEGIN (RSA |OPENSSH |EC )?PRIVATE KEY|ghp_[A-Za-z0-9]{20,}|Bearer [A-Za-z0-9_-]{20,}' \
  "$ROOT" --glob '!vendor/**' --glob '!target/**' --glob '!scripts/quarantine-check.sh'; then
  echo "credential-like material is retained in the feasibility harness" >&2
  exit 1
fi

if rg -n '(^|[;&|[:space:]])(node|npm|npx)([[:space:]]|$)' \
  "$ROOT/scripts" --glob '*.sh' --glob '!quarantine-check.sh'; then
  echo "normal feasibility scripts invoke Node.js" >&2
  exit 1
fi

for script in "$ROOT"/scripts/*.sh; do
  [[ -x "$script" ]] || {
    echo "spike command is not executable: $script" >&2
    exit 1
  }
done

printf 'quarantine ok: isolated Cargo workspace, no production dependency, credentials, or Node.js invocation\n'
