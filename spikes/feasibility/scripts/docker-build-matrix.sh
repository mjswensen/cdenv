#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
RUNTIME="$ROOT/target/spike-runtime/build-matrix"
CONTEXT="$RUNTIME/context"
LOG="$RUNTIME/matrix.log"
IMAGES=()

cleanup() {
  if ((${#IMAGES[@]})); then
    docker image rm -f "${IMAGES[@]}" >/dev/null 2>&1 || true
  fi
}
trap cleanup EXIT

for dependency in docker awk grep; do
  command -v "$dependency" >/dev/null || {
    echo "missing required Docker matrix dependency: $dependency" >&2
    exit 1
  }
done

docker info >/dev/null
rm -rf "$RUNTIME"
mkdir -p "$CONTEXT/features"
cp -R "$ROOT/fixtures/dockerfile-feature/.devcontainer/features/base-marker" "$CONTEXT/features/"
cp -R "$ROOT/fixtures/dockerfile-feature/.devcontainer/features/top-marker" "$CONTEXT/features/"
printf 'context-ok\n' > "$CONTEXT/fixture-content.txt"
printf 'must-not-enter-the-build-context\n' > "$CONTEXT/ignored-secret.txt"
printf 'ignored-secret.txt\n' > "$CONTEXT/.dockerignore"

cat > "$CONTEXT/Dockerfile" <<'EOF'
# syntax=docker/dockerfile:1
ARG BASE_IMAGE
FROM ${BASE_IMAGE} AS source
ARG SPIKE_BUILD_ARG
ARG TARGETARCH
COPY . /tmp/context-check
RUN test "$SPIKE_BUILD_ARG" = bounded-default \
    && test "$(cat /tmp/context-check/fixture-content.txt)" = context-ok \
    && test ! -e /tmp/context-check/ignored-secret.txt \
    && printf '%s\n' "$TARGETARCH" > /tmp/target-arch
RUN printf 'spike:x:1000:\n' >> /etc/group \
    && printf 'spike:x:1000:1000:spike:/home/spike:/bin/sh\n' >> /etc/passwd \
    && mkdir -p /home/spike \
    && chown 1000:1000 /home/spike

FROM source AS feature-base-marker
COPY features/base-marker /tmp/features/base-marker
RUN MESSAGE=dependency /bin/sh /tmp/features/base-marker/install.sh

FROM feature-base-marker AS feature-top-marker
COPY features/top-marker /tmp/features/top-marker
RUN MESSAGE=top /bin/sh /tmp/features/top-marker/install.sh

FROM feature-top-marker AS uid-helper
RUN ! grep -Eq '^[^:]+:[^:]*:1234:' /etc/passwd \
    && ! grep -Eq '^[^:]+:[^:]*:1234:' /etc/group \
    && sed -i 's/^spike:x:1000:1000:/spike:x:1234:1234:/' /etc/passwd \
    && sed -i 's/^spike:x:1000:/spike:x:1234:/' /etc/group \
    && chown -R 1234:1234 /home/spike
USER 1234:1234
CMD ["/bin/sh", "-c", "while sleep 3600; do :; done"]
EOF

: > "$LOG"
for os in debian alpine; do
  case "$os" in
    debian) base=debian:13-slim ;;
    alpine) base=alpine:3.22 ;;
  esac
  for arch in amd64 arm64; do
    tag="cdenv-feasibility/${os}-${arch}:matrix"
    IMAGES+=("$tag")
    echo "building $os/$arch from $base" | tee -a "$LOG"
    docker build --pull --progress=plain --platform "linux/$arch" \
      --build-arg "BASE_IMAGE=$base" --build-arg SPIKE_BUILD_ARG=bounded-default \
      --tag "$tag" "$CONTEXT" >>"$LOG" 2>&1

    history=$(docker history --no-trunc --format '{{.CreatedBy}}' "$tag")
    install_layers=$(grep -c '/tmp/features/.*/install.sh' <<<"$history" || true)
    [[ "$install_layers" == 2 ]] || {
      echo "$os/$arch did not preserve one generated layer per Feature (saw $install_layers)" >&2
      exit 1
    }

    case "$arch" in
      amd64) expected_machine=x86_64 ;;
      arm64) expected_machine=aarch64 ;;
    esac
    actual=$(docker run --rm --platform "linux/$arch" --entrypoint /bin/sh "$tag" -c '
      set -eu
      test "$(id -u)" = 1234
      test "$(id -g)" = 1234
      test "$(cat /usr/local/share/cdenv-spike/base-marker)" = dependency
      test "$(cat /usr/local/share/cdenv-spike/top-marker)" = top
      cat /tmp/target-arch
      uname -m
    ')
    target_arch=$(head -n 1 <<<"$actual")
    machine=$(tail -n 1 <<<"$actual")
    [[ "$target_arch" == "$arch" && "$machine" == "$expected_machine" ]] || {
      echo "$os/$arch architecture mismatch: TARGETARCH=$target_arch uname=$machine" >&2
      exit 1
    }
  done
done

# Also exercise the repository's Dockerfile target/options/context fixture directly.
fixture_tag=cdenv-feasibility/dockerfile-fixture:matrix
IMAGES+=("$fixture_tag")
docker build --progress=plain --target development \
  --build-arg SPIKE_BUILD_ARG=bounded-default \
  --add-host host.docker.internal:host-gateway \
  --file "$ROOT/fixtures/dockerfile-feature/.devcontainer/Dockerfile" \
  --tag "$fixture_tag" "$ROOT/fixtures/dockerfile-feature" >>"$LOG" 2>&1
docker run --rm --entrypoint /bin/sh "$fixture_tag" -c '
  test "$(cat /opt/cdenv-spike/fixture-content.txt)" = context-ok
  test ! -e /tmp/context-check/ignored-secret.txt
'

printf 'Docker build matrix ok: Debian/Alpine x amd64/arm64, ordered Feature layers, UID/GID rewrite, .dockerignore\n'
