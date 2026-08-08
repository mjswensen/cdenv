#!/usr/bin/env bash
set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REPO=$(cd "$ROOT/../.." && pwd)
RUNTIME="$ROOT/target/spike-runtime/ssh"
BIN="$ROOT/target/debug/feasibility-spike"
AGENT="$ROOT/target/debug/feasibility-agent"
CONTAINER=cdenv-feasibility-ssh
HOST_ALIAS=spike.cdenv
HOST_KEY_ALIAS=cdenv-stdio-spike
MASTER_DIR="/tmp/cdenv-feasibility-ssh-$$"
MASTER="$MASTER_DIR/control-master.sock"
TMUX_SESSION="cdenv-pty-$$"
LOCAL_FORWARD_PORT=$((22000 + $$ % 10000))
SSH_PID=
DETACHED_PID=

ssh_spike() {
  ssh -F "$RUNTIME/ssh_config" "$HOST_ALIAS" "$@"
}

wait_for_container_file() {
  local path=$1
  for _ in $(seq 1 100); do
    docker exec "$CONTAINER" test -s "$path" >/dev/null 2>&1 && return 0
    sleep 0.1
  done
  echo "timed out waiting for container file $path" >&2
  return 1
}

cleanup() {
  if [[ "${KEEP_SSH_FIXTURE:-0}" == 1 ]]; then
    return
  fi
  if [[ -n "${SSH_PID:-}" ]]; then
    kill "$SSH_PID" 2>/dev/null || true
    wait "$SSH_PID" 2>/dev/null || true
  fi
  tmux kill-session -t "$TMUX_SESSION" >/dev/null 2>&1 || true
  if [[ -S "$MASTER" ]]; then
    ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -O exit "$HOST_ALIAS" >/dev/null 2>&1 || true
  fi
  if docker inspect "$CONTAINER" >/dev/null 2>&1; then
    if [[ -n "${DETACHED_PID:-}" ]]; then
      docker exec "$CONTAINER" sh -c "kill '$DETACHED_PID' 2>/dev/null || true" >/dev/null 2>&1 || true
    fi
    docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  fi
  rm -rf "$MASTER_DIR"
}
trap cleanup EXIT

for dependency in cargo docker ssh ssh-keygen tmux curl sha256sum cmp awk grep; do
  command -v "$dependency" >/dev/null || {
    echo "missing required SSH spike dependency: $dependency" >&2
    exit 1
  }
done
docker info >/dev/null
cargo build --manifest-path "$ROOT/Cargo.toml" --locked --bins

rm -rf "$RUNTIME" "$MASTER_DIR"
mkdir -p "$RUNTIME/keys" "$MASTER_DIR"
chmod 700 "$RUNTIME" "$RUNTIME/keys" "$MASTER_DIR"
ssh-keygen -q -t ed25519 -N '' -C cdenv-client -f "$RUNTIME/keys/client"
ssh-keygen -q -t ed25519 -N '' -C cdenv-wrong-client -f "$RUNTIME/keys/wrong-client"
ssh-keygen -q -t ed25519 -N '' -C cdenv-host -f "$RUNTIME/keys/host"
ssh-keygen -q -t ed25519 -N '' -C cdenv-wrong-host -f "$RUNTIME/keys/wrong-host"
chmod 600 "$RUNTIME/keys/client" "$RUNTIME/keys/wrong-client" "$RUNTIME/keys/host" "$RUNTIME/keys/wrong-host"
awk -v host="$HOST_KEY_ALIAS" '{print host, $1, $2}' "$RUNTIME/keys/host.pub" > "$RUNTIME/known_hosts"
awk -v host="$HOST_KEY_ALIAS" '{print host, $1, $2}' "$RUNTIME/keys/wrong-host.pub" > "$RUNTIME/wrong_known_hosts"
chmod 600 "$RUNTIME/known_hosts" "$RUNTIME/wrong_known_hosts"

HOST_REPO=$(docker inspect "$(hostname)" --format "{{range .Mounts}}{{if eq .Destination \"$REPO\"}}{{.Source}}{{end}}{{end}}" 2>/dev/null || true)
if [[ -z "$HOST_REPO" ]]; then
  HOST_REPO=$REPO
fi
HOST_AGENT="$HOST_REPO/${AGENT#"$REPO"/}"

docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
docker create --name "$CONTAINER" \
  --label cdenv.installation=spike-installation \
  --label cdenv.workspace=ssh-spike \
  --label cdenv.generation=1 \
  --label cdenv.profile=cdenv-devcontainer-v1 \
  --mount "type=bind,source=$HOST_AGENT,target=/opt/cdenv-spike/agent,readonly" \
  debian:13-slim sh -c 'while sleep 3600; do :; done' >/dev/null
docker start "$CONTAINER" >/dev/null
docker exec "$CONTAINER" mkdir -p /run/cdenv-keys /workspace
docker cp "$RUNTIME/keys/host" "$CONTAINER:/run/cdenv-keys/host"
docker cp "$RUNTIME/keys/client.pub" "$CONTAINER:/run/cdenv-keys/allowed.pub"
docker exec "$CONTAINER" chmod 600 /run/cdenv-keys/host /run/cdenv-keys/allowed.pub

cat > "$RUNTIME/ssh_config" <<EOF
Host $HOST_ALIAS
    HostName cdenv-stdio.invalid
    HostKeyAlias $HOST_KEY_ALIAS
    User cdenv
    IdentityFile $RUNTIME/keys/client
    IdentitiesOnly yes
    UserKnownHostsFile $RUNTIME/known_hosts
    StrictHostKeyChecking yes
    PasswordAuthentication no
    KbdInteractiveAuthentication no
    PreferredAuthentications publickey
    ProxyCommand $BIN proxy $CONTAINER /opt/cdenv-spike/agent /run/cdenv-keys/host /run/cdenv-keys/allowed.pub /workspace
    ServerAliveInterval 5
    ServerAliveCountMax 3
    LogLevel ERROR
EOF
chmod 600 "$RUNTIME/ssh_config"
sed "s|IdentityFile .*|IdentityFile $RUNTIME/keys/wrong-client|" \
  "$RUNTIME/ssh_config" > "$RUNTIME/wrong-key-config"
sed "s|UserKnownHostsFile .*|UserKnownHostsFile $RUNTIME/wrong_known_hosts|" \
  "$RUNTIME/ssh_config" > "$RUNTIME/wrong-host-config"

# There is no listening sshd and no published host port.
[[ "$(docker inspect --format '{{len .HostConfig.PortBindings}}' "$CONTAINER")" == 0 ]]
if docker exec "$CONTAINER" sh -c 'command -v sshd' >/dev/null 2>&1; then
  echo "fixture unexpectedly contains sshd" >&2
  exit 1
fi
if docker top "$CONTAINER" -eo pid,args | grep -q '[s]shd'; then
  echo "fixture unexpectedly runs sshd" >&2
  exit 1
fi

# Host-key verification and public-key authentication fail closed.
if ssh -F "$RUNTIME/wrong-key-config" "$HOST_ALIAS" true \
  >"$RUNTIME/wrong-key.stdout" 2>"$RUNTIME/wrong-key.stderr"; then
  echo "unapproved client key was accepted" >&2
  exit 1
fi
grep -Eiq 'permission denied|publickey|authentication' "$RUNTIME/wrong-key.stderr"
if ssh -F "$RUNTIME/wrong-host-config" "$HOST_ALIAS" true \
  >"$RUNTIME/wrong-host.stdout" 2>"$RUNTIME/wrong-host.stderr"; then
  echo "changed host key was accepted" >&2
  exit 1
fi
grep -Eiq 'host key verification failed|remote host identification has changed' "$RUNTIME/wrong-host.stderr"

# Exact stdout/stderr and exit-status propagation through Docker's multiplexed
# Exec stream and Russh packet framing.
set +e
ssh_spike "printf 'stdout-exact\\n'; printf 'stderr-exact\\n' >&2; exit 23" \
  >"$RUNTIME/exec.stdout" 2>"$RUNTIME/exec.stderr"
status=$?
set -e
[[ "$status" == 23 ]]
printf 'stdout-exact\n' > "$RUNTIME/exec.stdout.expected"
printf 'stderr-exact\n' > "$RUNTIME/exec.stderr.expected"
cmp "$RUNTIME/exec.stdout.expected" "$RUNTIME/exec.stdout"
cmp "$RUNTIME/exec.stderr.expected" "$RUNTIME/exec.stderr"
[[ "$(ssh_spike pwd)" == /workspace ]]

# Binary and large payloads prove no Docker frame headers leak and exercise
# backpressure independently on stdout and SSH extended-data stderr.
PAYLOAD_SIZE=${PAYLOAD_SIZE:-4194304}
EXPECTED_ZERO_HASH=$(head -c "$PAYLOAD_SIZE" /dev/zero | sha256sum | awk '{print $1}')
STDOUT_HASH=$(ssh_spike "head -c $PAYLOAD_SIZE /dev/zero" | sha256sum | awk '{print $1}')
[[ "$STDOUT_HASH" == "$EXPECTED_ZERO_HASH" ]]
STDIN_HASH=$(head -c "$PAYLOAD_SIZE" /dev/zero | ssh_spike sha256sum | awk '{print $1}')
[[ "$STDIN_HASH" == "$EXPECTED_ZERO_HASH" ]]
ssh_spike "head -c $PAYLOAD_SIZE /dev/zero >&2; printf payload-ok" \
  >"$RUNTIME/large.stdout" 2>"$RUNTIME/large.stderr"
[[ "$(cat "$RUNTIME/large.stdout")" == payload-ok ]]
[[ "$(sha256sum "$RUNTIME/large.stderr" | awk '{print $1}')" == "$EXPECTED_ZERO_HASH" ]]

# Long-running output is visible before process completion.
ssh_spike "printf begin; sleep 2; printf end" > "$RUNTIME/stream.stdout" 2>"$RUNTIME/stream.stderr" &
SSH_PID=$!
sleep 0.5
[[ "$(cat "$RUNTIME/stream.stdout")" == begin ]]
wait "$SSH_PID"
SSH_PID=
[[ "$(cat "$RUNTIME/stream.stdout")" == beginend ]]
[[ ! -s "$RUNTIME/stream.stderr" ]]

# A killed client closes the transport and the agent reaps the whole command
# process group rather than orphaning shell descendants.
ssh -F "$RUNTIME/ssh_config" "$HOST_ALIAS" \
  'echo $$ > /tmp/cancel-shell.pid; sleep 30 & echo $! > /tmp/cancel-child.pid; wait' \
  >"$RUNTIME/cancel.stdout" 2>"$RUNTIME/cancel.stderr" &
SSH_PID=$!
wait_for_container_file /tmp/cancel-child.pid
kill "$SSH_PID"
wait "$SSH_PID" 2>/dev/null || true
SSH_PID=
for _ in $(seq 1 50); do
  if ! docker exec "$CONTAINER" sh -c 'kill -0 "$(cat /tmp/cancel-shell.pid)" 2>/dev/null || kill -0 "$(cat /tmp/cancel-child.pid)" 2>/dev/null'; then
    break
  fi
  sleep 0.1
done
if docker exec "$CONTAINER" sh -c 'kill -0 "$(cat /tmp/cancel-shell.pid)" 2>/dev/null || kill -0 "$(cat /tmp/cancel-child.pid)" 2>/dev/null'; then
  echo "SSH disconnect orphaned a remote command process" >&2
  exit 1
fi

# One ControlMaster connection carries multiple concurrent session channels.
ssh -F "$RUNTIME/ssh_config" -M -N -f -S "$MASTER" "$HOST_ALIAS"
ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -O check "$HOST_ALIAS" >/dev/null
[[ "$(docker top "$CONTAINER" -eo pid,args | grep -c '[s]sh-server')" == 1 ]]
channel_pids=()
for index in 1 2 3 4; do
  ssh -F "$RUNTIME/ssh_config" -S "$MASTER" "$HOST_ALIAS" \
    "sleep 1; printf channel-$index" > "$RUNTIME/channel-$index.stdout" &
  channel_pids+=("$!")
done
for pid in "${channel_pids[@]}"; do wait "$pid"; done
for index in 1 2 3 4; do [[ "$(cat "$RUNTIME/channel-$index.stdout")" == "channel-$index" ]]; done
[[ "$(docker top "$CONTAINER" -eo pid,args | grep -c '[s]sh-server')" == 1 ]]

# OpenSSH local forwarding uses SSH direct-tcpip channels over the same master.
docker exec "$CONTAINER" sh -c '/opt/cdenv-spike/agent http-server 18080 >/tmp/http-agent.log 2>&1 </dev/null & echo $! > /tmp/http-agent.pid'
wait_for_container_file /tmp/http-agent.pid
ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -O forward \
  -L "127.0.0.1:$LOCAL_FORWARD_PORT:127.0.0.1:18080" "$HOST_ALIAS"
for _ in $(seq 1 50); do
  curl --silent --fail --max-time 2 "http://127.0.0.1:$LOCAL_FORWARD_PORT/" > "$RUNTIME/direct-tcp.body" 2>/dev/null && break
  sleep 0.1
done
grep -q 'cdenv-direct-tcpip-ok' "$RUNTIME/direct-tcp.body"
ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -O cancel \
  -L "127.0.0.1:$LOCAL_FORWARD_PORT:127.0.0.1:18080" "$HOST_ALIAS"
if curl --silent --fail --max-time 2 "http://127.0.0.1:$LOCAL_FORWARD_PORT/" >/dev/null 2>&1; then
  echo "cancelled direct-tcpip listener remained reachable" >&2
  exit 1
fi

# PTY allocation, window resize propagation, and Ctrl-C/signal behavior.
cat > "$RUNTIME/remote-pty.sh" <<'EOF'
#!/bin/sh
set -eu
stty size > /tmp/pty-initial
trap 'stty size > /tmp/pty-resized; printf winch > /tmp/pty-winch' WINCH
trap 'printf interrupt > /tmp/pty-interrupt; exit 130' INT
printf ready > /tmp/pty-ready
while :; do sleep 1; done
EOF
docker cp "$RUNTIME/remote-pty.sh" "$CONTAINER:/workspace/remote-pty.sh"
docker exec "$CONTAINER" chmod +x /workspace/remote-pty.sh
cat > "$RUNTIME/pty-client.sh" <<EOF
#!/bin/sh
exec ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -tt "$HOST_ALIAS" /workspace/remote-pty.sh
EOF
chmod +x "$RUNTIME/pty-client.sh"
tmux new-session -d -s "$TMUX_SESSION" -x 80 -y 24 "$RUNTIME/pty-client.sh"
wait_for_container_file /tmp/pty-ready
tmux resize-window -t "$TMUX_SESSION" -x 101 -y 43
wait_for_container_file /tmp/pty-winch
[[ "$(docker exec "$CONTAINER" cat /tmp/pty-resized)" == '43 101' ]]
tmux send-keys -t "$TMUX_SESSION" C-c
wait_for_container_file /tmp/pty-interrupt
for _ in $(seq 1 50); do
  tmux has-session -t "$TMUX_SESSION" 2>/dev/null || break
  sleep 0.1
done
if tmux has-session -t "$TMUX_SESSION" 2>/dev/null; then
  echo "PTY session did not exit after Ctrl-C" >&2
  exit 1
fi

# Explicit detached-helper policy: a command that starts its own session and
# redirects every descriptor may outlive SSH, but its PID is returned and the
# workspace owner must track and terminate it during teardown.
DETACHED_PID=$(ssh -F "$RUNTIME/ssh_config" -S "$MASTER" "$HOST_ALIAS" \
  '/usr/bin/setsid /bin/sh -c "sleep 30" >/dev/null 2>&1 </dev/null & printf "%s\n" "$!"')
[[ "$DETACHED_PID" =~ ^[0-9]+$ ]]
docker exec "$CONTAINER" sh -c "kill -0 '$DETACHED_PID'"
docker exec "$CONTAINER" sh -c "kill '$DETACHED_PID'"
DETACHED_PID=

# Closing the master deterministically tears down the stdio agent. The separate
# direct-forward fixture helper is then stopped by its tracked PID.
ssh -F "$RUNTIME/ssh_config" -S "$MASTER" -O exit "$HOST_ALIAS" >/dev/null
for _ in $(seq 1 100); do
  [[ "$(docker top "$CONTAINER" -eo pid,args | grep -c '[s]sh-server' || true)" == 0 ]] && break
  sleep 0.1
done
[[ "$(docker top "$CONTAINER" -eo pid,args | grep -c '[s]sh-server' || true)" == 0 ]]
docker exec "$CONTAINER" sh -c 'kill "$(cat /tmp/http-agent.pid)"'
if docker top "$CONTAINER" -eo pid,args | grep -q '[s]shd'; then
  echo "sshd appeared during the SSH packet-flow gate" >&2
  exit 1
fi

printf 'SSH integration ok: stdio transport, auth/host keys, exact streams/status, multiplexing, PTY/signals, direct-tcpip, cleanup\n'
