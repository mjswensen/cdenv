#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REPO=$(cd "$ROOT/../.." && pwd)
BIN="$ROOT/target/debug/feasibility-spike"
RUNTIME="$ROOT/target/spike-runtime/https"
WEB="$RUNTIME/web"
PORT=$((20000 + $$ % 20000))
SERVER_PID=
CACHE_FILE=
CACHE_BACKUP=

cleanup() {
  if [[ -n "${CACHE_BACKUP:-}" && -f "$CACHE_BACKUP" ]]; then
    mv -f "$CACHE_BACKUP" "$CACHE_FILE"
  fi
  if [[ -n "${SERVER_PID:-}" ]]; then
    kill "$SERVER_PID" 2>/dev/null || true
    wait "$SERVER_PID" 2>/dev/null || true
  fi
}
trap cleanup EXIT

for dependency in cargo openssl tar gzip sha256sum curl; do
  command -v "$dependency" >/dev/null || {
    echo "missing required profile-spike dependency: $dependency" >&2
    exit 1
  }
done

cargo build --manifest-path "$ROOT/Cargo.toml" --locked --bin feasibility-spike
"$BIN" profile
"$BIN" resolve-oci ghcr.io/devcontainers/features/common-utils:2 check

BLOB_DIGEST=$(grep '"blobDigest"' "$ROOT/expected/public-feature-evidence.json" | cut -d '"' -f4)
CACHE_FILE="$ROOT/target/spike-cache/oci/${BLOB_DIGEST/:/\/}"
CACHE_BACKUP="$RUNTIME.cache-backup"
cp "$CACHE_FILE" "$CACHE_BACKUP"
printf 'corrupt cached Feature\n' > "$CACHE_FILE"
if "$BIN" resolve-oci ghcr.io/devcontainers/features/common-utils:2 check \
  >"$ROOT/target/spike-runtime/corrupt-cache.stdout" \
  2>"$ROOT/target/spike-runtime/corrupt-cache.stderr"; then
  echo "corrupted OCI cache entry unexpectedly passed" >&2
  exit 1
fi
grep -q 'cached Feature blob digest mismatch' "$ROOT/target/spike-runtime/corrupt-cache.stderr"
mv -f "$CACHE_BACKUP" "$CACHE_FILE"
CACHE_BACKUP=
"$BIN" resolve-oci ghcr.io/devcontainers/features/common-utils:2 check >/dev/null
find "$ROOT/expected" -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum \
  > "$ROOT/target/spike-runtime/snapshots.before"
"$BIN" profile >/dev/null
find "$ROOT/expected" -maxdepth 1 -type f -print0 | sort -z | xargs -0 sha256sum \
  > "$ROOT/target/spike-runtime/snapshots.after"
cmp "$ROOT/target/spike-runtime/snapshots.before" "$ROOT/target/spike-runtime/snapshots.after"

rm -rf "$RUNTIME"
mkdir -p "$WEB/package"
cp "$ROOT/fixtures/metadata-feature/.devcontainer/features/metadata-marker/devcontainer-feature.json" "$WEB/package/"
cp "$ROOT/fixtures/metadata-feature/.devcontainer/features/metadata-marker/install.sh" "$WEB/package/"
(
  cd "$WEB/package"
  tar --sort=name --mtime='@0' --owner=0 --group=0 --numeric-owner -cf - . \
    | gzip -n > "$WEB/devcontainer-feature-metadata-marker.tgz"
)
ARCHIVE_DIGEST="sha256:$(sha256sum "$WEB/devcontainer-feature-metadata-marker.tgz" | awk '{print $1}')"

openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -subj '/CN=cdenv feasibility test CA' \
  -keyout "$RUNTIME/ca.key" -out "$RUNTIME/ca.pem" >/dev/null 2>&1
openssl req -newkey rsa:2048 -nodes -subj '/CN=localhost' \
  -keyout "$RUNTIME/server.key" -out "$RUNTIME/server.csr" >/dev/null 2>&1
cat > "$RUNTIME/server.ext" <<'EOF'
subjectAltName=DNS:localhost
basicConstraints=CA:FALSE
keyUsage=digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
EOF
openssl x509 -req -days 2 -sha256 \
  -in "$RUNTIME/server.csr" -CA "$RUNTIME/ca.pem" -CAkey "$RUNTIME/ca.key" \
  -CAcreateserial -extfile "$RUNTIME/server.ext" -out "$RUNTIME/server.pem" >/dev/null 2>&1
openssl req -x509 -newkey rsa:2048 -nodes -days 2 \
  -subj '/CN=wrong feasibility test CA' \
  -keyout "$RUNTIME/wrong-ca.key" -out "$RUNTIME/wrong-ca.pem" >/dev/null 2>&1

(
  cd "$WEB"
  exec openssl s_server -quiet -WWW -accept "127.0.0.1:$PORT" \
    -cert "$RUNTIME/server.pem" -key "$RUNTIME/server.key"
) >"$RUNTIME/https-server.log" 2>&1 &
SERVER_PID=$!

for _ in $(seq 1 50); do
  if curl --silent --show-error --fail --cacert "$RUNTIME/ca.pem" \
    "https://localhost:$PORT/devcontainer-feature-metadata-marker.tgz" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
kill -0 "$SERVER_PID" 2>/dev/null || {
  echo "local verified-HTTPS fixture failed to start" >&2
  exit 1
}

"$BIN" download-https \
  "https://localhost:$PORT/devcontainer-feature-metadata-marker.tgz" \
  "$RUNTIME/ca.pem" "$ARCHIVE_DIGEST"

if "$BIN" download-https \
  "https://localhost:$PORT/devcontainer-feature-metadata-marker.tgz" \
  "$RUNTIME/wrong-ca.pem" "$ARCHIVE_DIGEST" \
  >"$RUNTIME/wrong-ca.stdout" 2>"$RUNTIME/wrong-ca.stderr"; then
  echo "HTTPS Feature unexpectedly succeeded with an untrusted CA" >&2
  exit 1
fi
grep -Eiq 'certificate|issuer|tls|unknown ca' "$RUNTIME/wrong-ca.stderr" || {
  echo "wrong-CA failure was not actionable" >&2
  cat "$RUNTIME/wrong-ca.stderr" >&2
  exit 1
}

# Every command above is a direct Rust/OpenSSL/tar/curl invocation; the gate has no
# Node.js or @devcontainers/cli runtime dependency.
printf 'profile integration ok: OCI bearer/digest/cache, verified HTTPS, TLS rejection, bounded safe archive\n'
