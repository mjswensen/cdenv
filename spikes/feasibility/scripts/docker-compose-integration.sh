#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REPO=$(cd "$ROOT/../.." && pwd)
FIXTURE="$ROOT/fixtures/compose"
RUNTIME="$ROOT/target/spike-runtime/docker-compose"
BIN="$ROOT/target/debug/feasibility-spike"
OVERRIDE_A="$RUNTIME/override-a.json"
OVERRIDE_B="$RUNTIME/override-b.json"
PROJECT_A=cdenv-feasibility-a
PROJECT_B=cdenv-feasibility-b
WORKSPACE_A=compose-a
WORKSPACE_B=compose-b
IMAGE_CONTAINER=cdenv-feasibility-image
SUPERVISOR_PID=
CONTROL_DIR="/tmp/cdenv-feasibility-forward-$$"
CONTROL_SOCKET="$CONTROL_DIR/control.sock"
CONTAINERS=()
VOLUMES=()

compose() {
  local project=$1 override=$2
  shift 2
  docker compose --project-directory "$FIXTURE" --project-name "$project" \
    --file "$FIXTURE/compose.yaml" --file "$override" "$@"
}

cleanup() {
  if [[ -n "${SUPERVISOR_PID:-}" ]]; then
    kill "$SUPERVISOR_PID" 2>/dev/null || true
    wait "$SUPERVISOR_PID" 2>/dev/null || true
  fi
  mapfile -t labelled < <(docker ps -aq --filter label=cdenv.installation=spike-installation 2>/dev/null || true)
  if ((${#labelled[@]})); then
    docker rm -f "${labelled[@]}" >/dev/null 2>&1 || true
  fi
  for volume in "$PROJECT_A"_dependency-data "$PROJECT_B"_dependency-data; do
    docker volume rm "$volume" >/dev/null 2>&1 || true
  done
  docker network rm "$PROJECT_A"_default "$PROJECT_B"_default >/dev/null 2>&1 || true
  rm -rf "$CONTROL_DIR"
}
trap cleanup EXIT

for dependency in cargo docker curl awk grep sha256sum; do
  command -v "$dependency" >/dev/null || {
    echo "missing required Docker/Compose spike dependency: $dependency" >&2
    exit 1
  }
done
docker info >/dev/null
cargo build --manifest-path "$ROOT/Cargo.toml" --locked --bins

rm -rf "$RUNTIME" "$CONTROL_DIR"
mkdir -p "$RUNTIME" "$CONTROL_DIR"
chmod 700 "$CONTROL_DIR"
"$BIN" docker-versions > "$RUNTIME/docker-versions.json"

AGENT="$ROOT/target/debug/feasibility-agent"
HOST_REPO=$(docker inspect "$(hostname)" --format "{{range .Mounts}}{{if eq .Destination \"$REPO\"}}{{.Source}}{{end}}{{end}}" 2>/dev/null || true)
if [[ -z "$HOST_REPO" ]]; then
  HOST_REPO=$REPO
fi
HOST_AGENT="$HOST_REPO/${AGENT#"$REPO"/}"
"$BIN" compose-override "$WORKSPACE_A" "$HOST_REPO" /workspaces/repository "$HOST_AGENT" "$OVERRIDE_A"
"$BIN" compose-override "$WORKSPACE_B" "$HOST_REPO" /workspaces/repository "$HOST_AGENT" "$OVERRIDE_B"

cat > "$RUNTIME/effective-command-lines.txt" <<EOF
DOCKER_HOST=unix:///var/run/docker.sock docker compose --project-directory <fixture> --project-name $PROJECT_A --file compose.yaml --file override-a.json up --detach workspace dependency
DOCKER_HOST=unix:///var/run/docker.sock docker start <persisted-container-id-set>
DOCKER_HOST=unix:///var/run/docker.sock docker stop <persisted-container-id-set>
EOF
cmp "$ROOT/expected/docker-compose-command-lines.txt" "$RUNTIME/effective-command-lines.txt"

# Image input: no Compose, no host port publication, exact managed labels.
docker pull debian:13-slim >/dev/null
docker create --name "$IMAGE_CONTAINER" \
  --label cdenv.installation=spike-installation \
  --label cdenv.workspace=image-fixture \
  --label cdenv.generation=1 \
  --label cdenv.profile=cdenv-devcontainer-v1 \
  debian:13-slim sh -c 'while sleep 3600; do :; done' >/dev/null
docker start "$IMAGE_CONTAINER" >/dev/null
"$BIN" verify-labels image-fixture 1 > "$RUNTIME/image-label-evidence.json"
cmp "$ROOT/expected/image-label-evidence.json" "$RUNTIME/image-label-evidence.json"
[[ "$(docker inspect --format '{{len .HostConfig.PortBindings}}' "$IMAGE_CONTAINER")" == 0 ]]

# Two identical Compose workspaces prove project/name/network/volume isolation.
compose "$PROJECT_A" "$OVERRIDE_A" up --detach --no-build workspace dependency
compose "$PROJECT_B" "$OVERRIDE_B" up --detach --no-build workspace dependency
"$BIN" verify-labels "$WORKSPACE_A" 2 > "$RUNTIME/compose-a-label-evidence.json"
"$BIN" verify-labels "$WORKSPACE_B" 2 > "$RUNTIME/compose-b-label-evidence.json"
cmp "$ROOT/expected/compose-a-label-evidence.json" "$RUNTIME/compose-a-label-evidence.json"
cmp "$ROOT/expected/compose-b-label-evidence.json" "$RUNTIME/compose-b-label-evidence.json"
grep -q '"dependency"' "$RUNTIME/compose-a-label-evidence.json"
grep -q '"workspace"' "$RUNTIME/compose-a-label-evidence.json"

PRIMARY_A=$(compose "$PROJECT_A" "$OVERRIDE_A" ps --quiet workspace)
DEPENDENCY_A=$(compose "$PROJECT_A" "$OVERRIDE_A" ps --quiet dependency)
PRIMARY_B=$(compose "$PROJECT_B" "$OVERRIDE_B" ps --quiet workspace)
DEPENDENCY_B=$(compose "$PROJECT_B" "$OVERRIDE_B" ps --quiet dependency)
for id in "$PRIMARY_A" "$DEPENDENCY_A" "$PRIMARY_B" "$DEPENDENCY_B"; do
  [[ -n "$id" ]]
done
[[ "$(docker inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$PRIMARY_A")" == workspace ]]
[[ "$(docker inspect --format '{{index .Config.Labels "com.docker.compose.service"}}' "$DEPENDENCY_A")" == dependency ]]
[[ "$PRIMARY_A" != "$PRIMARY_B" && "$DEPENDENCY_A" != "$DEPENDENCY_B" ]]
for volume in "$PROJECT_A"_dependency-data "$PROJECT_B"_dependency-data; do
  docker volume inspect "$volume" >/dev/null
  VOLUMES+=("$volume")
done

# Simulated lifecycle orchestration: serial hooks, deterministic launch order for
# parallel object-form hooks, and waitFor returning while later work is running.
LIFECYCLE_LOG=/tmp/cdenv-lifecycle-order
STATE=/tmp/cdenv-lifecycle-state
echo initialize.host > "$RUNTIME/initialize.log"
docker exec "$PRIMARY_A" sh -c "rm -f '$LIFECYCLE_LOG' '$STATE' '${STATE%/*}/cdenv-lifecycle.tmp'; printf 'onCreateCommand\\n' > '$LIFECYCLE_LOG'"
start_seconds=$(date +%s)
docker exec "$PRIMARY_A" sh -c "
  printf 'updateContentCommand.alpha.start\\nupdateContentCommand.beta.start\\n' >> '$LIFECYCLE_LOG'
  (sleep 2; printf alpha > /tmp/update-alpha) & a=\$!
  (sleep 2; printf beta > /tmp/update-beta) & b=\$!
  wait \$a; wait \$b
  printf 'updateContentCommand.alpha.done\\nupdateContentCommand.beta.done\\n' >> '$LIFECYCLE_LOG'
  printf 'postCreateCommand\\n' >> '$LIFECYCLE_LOG'
"
elapsed=$(( $(date +%s) - start_seconds ))
((elapsed < 4)) || { echo "parallel lifecycle object ran serially" >&2; exit 1; }

docker exec -d "$PRIMARY_A" /opt/cdenv-spike/agent lifecycle-runner "$STATE" "$LIFECYCLE_LOG" 3
for _ in $(seq 1 50); do
  [[ "$(docker exec "$PRIMARY_A" sh -c "cat '$STATE' 2>/dev/null || true")" == running ]] && break
  sleep 0.1
done
[[ "$(docker exec "$PRIMARY_A" cat "$STATE")" == running ]]
if docker exec "$PRIMARY_A" /opt/cdenv-spike/agent lifecycle-runner "$STATE" "$LIFECYCLE_LOG" 0 \
  >"$RUNTIME/repeat-lifecycle.stdout" 2>"$RUNTIME/repeat-lifecycle.stderr"; then
  echo "repeated up duplicated an in-flight lifecycle hook" >&2
  exit 1
fi
grep -q 'indeterminate' "$RUNTIME/repeat-lifecycle.stderr"

# Forwarding supervisor is launched by a short-lived parent and survives it.
printf 'correct horse battery staple\n' > "$RUNTIME/control.token"
printf 'wrong token\n' > "$RUNTIME/wrong.token"
chmod 600 "$RUNTIME/control.token" "$RUNTIME/wrong.token"
cat > "$RUNTIME/launch-forwarder.sh" <<EOF
#!/bin/sh
nohup "$BIN" forward-supervisor "$WORKSPACE_A" 1 "$PRIMARY_A" /opt/cdenv-spike/agent \
  dependency 80 127.0.0.1:0 "$CONTROL_SOCKET" "$RUNTIME/control.token" "$RUNTIME/ready.json" \
  >"$RUNTIME/forwarder.log" 2>&1 </dev/null &
echo \$! > "$RUNTIME/forwarder.pid"
EOF
chmod +x "$RUNTIME/launch-forwarder.sh"
"$RUNTIME/launch-forwarder.sh"
SUPERVISOR_PID=$(cat "$RUNTIME/forwarder.pid")
for _ in $(seq 1 100); do
  [[ -s "$RUNTIME/ready.json" && -S "$CONTROL_SOCKET" ]] && break
  sleep 0.1
done
kill -0 "$SUPERVISOR_PID"
FORWARD_ADDRESS=$(grep -E '^[[:space:]]*"bind"' "$RUNTIME/ready.json" | cut -d '"' -f4)
[[ -n "$FORWARD_ADDRESS" ]]
curl --silent --show-error --fail --max-time 5 "http://$FORWARD_ADDRESS/" | grep -qi nginx

# Temporary target loss does not kill the listener; a later connection recovers.
docker stop "$DEPENDENCY_A" >/dev/null
if curl --silent --show-error --fail --max-time 3 "http://$FORWARD_ADDRESS/" >/dev/null 2>&1; then
  echo "forward unexpectedly succeeded while the dependency was stopped" >&2
  exit 1
fi
kill -0 "$SUPERVISOR_PID"
docker start "$DEPENDENCY_A" >/dev/null
for _ in $(seq 1 50); do
  curl --silent --fail --max-time 2 "http://$FORWARD_ADDRESS/" >/dev/null 2>&1 && break
  sleep 0.1
done
curl --silent --show-error --fail --max-time 5 "http://$FORWARD_ADDRESS/" | grep -qi nginx

# Lifecycle eventually commits once, then repeat calls are no-ops.
for _ in $(seq 1 60); do
  [[ "$(docker exec "$PRIMARY_A" cat "$STATE" 2>/dev/null || true)" == succeeded ]] && break
  sleep 0.1
done
[[ "$(docker exec "$PRIMARY_A" cat "$STATE")" == succeeded ]]
[[ "$(docker exec "$PRIMARY_A" grep -c '^postCreate:start$' "$LIFECYCLE_LOG")" == 1 ]]
docker exec "$PRIMARY_A" /opt/cdenv-spike/agent lifecycle-runner "$STATE" "$LIFECYCLE_LOG" 0
[[ "$(docker exec "$PRIMARY_A" grep -c '^postCreate:start$' "$LIFECYCLE_LOG")" == 1 ]]

docker exec "$PRIMARY_A" cat "$LIFECYCLE_LOG" > "$RUNTIME/lifecycle-order.log"
cmp "$ROOT/expected/lifecycle-order.log" "$RUNTIME/lifecycle-order.log"

# Interrupted lifecycle stays explicitly indeterminate and refuses duplication.
docker exec "$PRIMARY_A" sh -c "rm -f '$STATE' '${STATE%/*}/cdenv-lifecycle.tmp'; : > '$LIFECYCLE_LOG'; /opt/cdenv-spike/agent lifecycle-runner '$STATE' '$LIFECYCLE_LOG' 30 >/tmp/lifecycle.out 2>&1 </dev/null & echo \$! > /tmp/lifecycle.pid"
for _ in $(seq 1 50); do
  [[ "$(docker exec "$PRIMARY_A" sh -c "cat '$STATE' 2>/dev/null || true")" == running ]] && break
  sleep 0.1
done
docker exec "$PRIMARY_A" sh -c 'kill "$(cat /tmp/lifecycle.pid)"'
sleep 0.2
[[ "$(docker exec "$PRIMARY_A" cat "$STATE")" == running ]]
if docker exec "$PRIMARY_A" /opt/cdenv-spike/agent lifecycle-runner "$STATE" "$LIFECYCLE_LOG" 0 >/dev/null 2>&1; then
  echo "indeterminate lifecycle was silently retried" >&2
  exit 1
fi
[[ "$(docker exec "$PRIMARY_A" grep -c '^postCreate:start$' "$LIFECYCLE_LOG")" == 1 ]]
# A definitely-not-started/pending operation is safe to retry.
docker exec "$PRIMARY_A" sh -c "printf 'pending\\n' > '$STATE'; : > '$LIFECYCLE_LOG'"
docker exec "$PRIMARY_A" /opt/cdenv-spike/agent lifecycle-runner "$STATE" "$LIFECYCLE_LOG" 0
[[ "$(docker exec "$PRIMARY_A" cat "$STATE")" == succeeded ]]

# Authenticated and idempotent forward teardown.
if "$BIN" forward-stop "$CONTROL_SOCKET" "$RUNTIME/wrong.token" "$WORKSPACE_A" 1 >/dev/null 2>&1; then
  echo "forward teardown accepted the wrong token" >&2
  exit 1
fi
"$BIN" forward-stop "$CONTROL_SOCKET" "$RUNTIME/control.token" "$WORKSPACE_A" 1
wait "$SUPERVISOR_PID" 2>/dev/null || true
SUPERVISOR_PID=
[[ ! -e "$CONTROL_SOCKET" && ! -e "$RUNTIME/ready.json" ]]
if curl --silent --fail --max-time 2 "http://$FORWARD_ADDRESS/" >/dev/null 2>&1; then
  echo "forward listener survived authenticated teardown" >&2
  exit 1
fi

# Resume and stop operate only on the exact persisted service set, even if a
# desired override later drifts. No broad `compose down` is used.
PERSISTED_A=("$PRIMARY_A" "$DEPENDENCY_A")
docker stop "${PERSISTED_A[@]}" >/dev/null
[[ "$(docker inspect --format '{{.State.Running}}' "$PRIMARY_A")" == false ]]
[[ "$(docker inspect --format '{{.State.Running}}' "$DEPENDENCY_A")" == false ]]
[[ "$(docker inspect --format '{{.State.Running}}' "$PRIMARY_B")" == true ]]
[[ "$(docker inspect --format '{{.State.Running}}' "$DEPENDENCY_B")" == true ]]
docker volume inspect "$PROJECT_A"_dependency-data >/dev/null

docker start "${PERSISTED_A[@]}" >/dev/null
[[ "$(docker inspect --format '{{.Id}}' "$PRIMARY_A")" == "$PRIMARY_A" ]]
[[ "$(docker inspect --format '{{.Id}}' "$DEPENDENCY_A")" == "$DEPENDENCY_A" ]]
[[ -z "$(docker inspect --format '{{index .Config.Labels "cdenv.desired-drift"}}' "$PRIMARY_A")" ]]
docker stop "${PERSISTED_A[@]}" >/dev/null
"$BIN" verify-labels "$WORKSPACE_A" 2 > "$RUNTIME/stopped-label-evidence.json"
docker volume inspect "$PROJECT_A"_dependency-data >/dev/null

printf 'Docker/Compose integration ok: labels, exact service sets, drift-safe resume/stop, lifecycle recovery, forwarding recovery/teardown\n'
