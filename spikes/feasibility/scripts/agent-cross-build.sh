#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUNTIME="$ROOT/target/spike-runtime/agent-cross-build"
CONTEXT="$RUNTIME/context"
IMAGES=()
CONTAINERS=()

cleanup() {
  if ((${#CONTAINERS[@]})); then
    docker rm -f "${CONTAINERS[@]}" >/dev/null 2>&1 || true
  fi
  if ((${#IMAGES[@]})); then
    docker image rm -f "${IMAGES[@]}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

for dependency in docker readelf grep; do
  command -v "$dependency" >/dev/null || {
    echo "missing required agent cross-build dependency: $dependency" >&2
    exit 1
  }
done
docker info >/dev/null
docker buildx version >/dev/null

rm -rf "$RUNTIME"
mkdir -p "$CONTEXT/src/bin"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$CONTEXT/"
cp "$ROOT/src/error.rs" "$ROOT/src/main.rs" "$ROOT/src/oci.rs" \
  "$ROOT/src/profile.rs" "$ROOT/src/docker_api.rs" "$ROOT/src/supervisor.rs" "$CONTEXT/src/"
cp "$ROOT/src/bin/feasibility-agent.rs" "$CONTEXT/src/bin/"
cat > "$CONTEXT/Dockerfile" <<'EOF'
# syntax=docker/dockerfile:1
FROM rust:1.97-alpine3.22 AS build
ARG TARGETARCH
RUN apk add --no-cache build-base cmake perl
WORKDIR /source
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,id=cdenv-cargo-registry,target=/usr/local/cargo/registry \
    --mount=type=cache,id=cdenv-agent-${TARGETARCH},target=/source/target \
    cargo build --release --locked --bin feasibility-agent \
    && cp target/release/feasibility-agent /feasibility-agent

FROM scratch
COPY --from=build /feasibility-agent /feasibility-agent
ENTRYPOINT ["/feasibility-agent"]
EOF

for arch in amd64 arm64; do
  tag="cdenv-feasibility/agent-${arch}:cross-build"
  IMAGES+=("$tag")
  docker buildx build --load --progress=plain --platform "linux/$arch" \
    --tag "$tag" "$CONTEXT" > "$RUNTIME/build-$arch.log" 2>&1
  container="cdenv-agent-cross-$arch-$$"
  CONTAINERS+=("$container")
  docker create --name "$container" --platform "linux/$arch" "$tag" >/dev/null
  docker cp "$container:/feasibility-agent" "$RUNTIME/feasibility-agent-$arch"
  chmod 755 "$RUNTIME/feasibility-agent-$arch"
  case "$arch" in
    amd64) expected_machine='Advanced Micro Devices X86-64' ;;
    arm64) expected_machine='AArch64' ;;
  esac
  readelf -h "$RUNTIME/feasibility-agent-$arch" | grep -F "Machine:" | grep -Fq "$expected_machine"
  if readelf -l "$RUNTIME/feasibility-agent-$arch" | grep -q 'INTERP'; then
    echo "$arch agent is dynamically linked rather than static musl" >&2
    exit 1
  fi
  set +e
  output=$(docker run --rm --platform "linux/$arch" "$tag" 2>&1)
  status=$?
  set -e
  [[ "$status" == 1 && "$output" == *'missing subcommand'* ]] || {
    echo "$arch agent did not execute as expected: status=$status output=$output" >&2
    exit 1
  }
done

printf 'agent cross-build ok: static musl amd64/arm64 artifacts via Docker Buildx\n'
