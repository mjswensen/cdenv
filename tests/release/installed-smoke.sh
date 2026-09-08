#!/usr/bin/env bash
# Installed, editor-independent V1 operational smoke for an issue-52 archive.
set -Eeuo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <cdenv-release.tar>" >&2
  exit 2
fi
if [[ $(uname -s) != Linux ]]; then
  echo "installed smoke requires Linux" >&2
  exit 1
fi

archive=$(realpath "$1")
checksum="${archive}.sha256"
[[ -s "$archive" && -s "$checksum" ]] || {
  echo "archive and adjacent checksum are required" >&2
  exit 1
}
(
  cd "$(dirname "$archive")"
  sha256sum --check "$(basename "$checksum")"
)

work=$(mktemp -d "${TMPDIR:-/tmp}/cdenv-installed-smoke.XXXXXX")
root="$work/root"
home="$work/home"
repository="$work/repository"
install="$work/install"
workspace=installed-smoke
container_id=
unrelated_id=
forward_pid=
cleanup() {
  if [[ -n "$forward_pid" ]]; then
    kill "$forward_pid" 2>/dev/null || true
    wait "$forward_pid" 2>/dev/null || true
  fi
  "$install/cdenv" --root "$root" down "$workspace" >/dev/null 2>&1 || true
  docker ps -aq --filter "label=cdenv.workspace=$workspace" | xargs -r docker rm -f >/dev/null 2>&1 || true
  [[ -z "$unrelated_id" ]] || docker rm -f "$unrelated_id" >/dev/null 2>&1 || true
  docker volume rm installed-smoke-data >/dev/null 2>&1 || true
  rm -rf "$work"
}
trap cleanup EXIT

mkdir -p "$install" "$home" "$repository"
tar -xf "$archive" -C "$install"
[[ -x "$install/cdenv" ]] || {
  echo "archive did not install one executable cdenv" >&2
  exit 1
}
mkdir -p "$work/forbidden-tools"
for tool in node npm npx code; do
  printf '#!/bin/sh\necho "%s is forbidden in the installed smoke" >&2\nexit 97\n' "$tool" >"$work/forbidden-tools/$tool"
  chmod +x "$work/forbidden-tools/$tool"
done
export HOME="$home"
export PATH="$install:$work/forbidden-tools:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
unset NODE NODE_PATH npm_config_prefix ELECTRON_RUN_AS_NODE VSCODE_IPC_HOOK_CLI
[[ $(command -v cdenv) == "$install/cdenv" ]]

cat >"$repository/.devcontainer.json" <<'JSON'
{
  "name": "installed-release-smoke",
  "image": "alpine:3.22",
  "workspaceFolder": "/workspaces/installed-release-smoke",
  "mounts": ["source=installed-smoke-data,target=/data,type=volume"]
}
JSON
printf 'tracked-base\n' >"$repository/tracked-smoke.txt"
git -C "$repository" init --quiet
git -C "$repository" config user.name "cdenv release smoke"
git -C "$repository" config user.email "cdenv-smoke.invalid"
git -C "$repository" add .devcontainer.json tracked-smoke.txt
git -C "$repository" commit --quiet -m fixture

cdenv --root "$root" --no-modify-ssh-config create "$repository" --name "$workspace"
checkout="$root/workspaces/$workspace/checkout/$workspace"
printf 'tracked-change\n' >>"$checkout/tracked-smoke.txt"
printf 'untracked-change\n' >"$checkout/untracked-smoke.txt"
host_key=$(sha256sum "$root/ssh/host_keys/$workspace.pub" | awk '{print $1}')
container_id=$(docker ps -q --filter "label=cdenv.workspace=$workspace")
generation=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["active"]["generation"])' "$root/workspaces/$workspace/state.json")
docker exec "$container_id" sh -c 'printf volume-data >/data/smoke'
unrelated_id=$(docker run --detach --rm alpine:3.22 sh -c 'while sleep 3600; do :; done')
cdenv --root "$root" list
cdenv --root "$root" list --json
cdenv --root "$root" status "$workspace"
cdenv --root "$root" status "$workspace" --json
cdenv --root "$root" doctor
cdenv --root "$root" doctor --json
cdenv --root "$root" lock "$workspace"
cdenv --root "$root" ssh "$workspace" -- printf installed-ssh | grep -qx installed-ssh
ssh -F "$root/ssh/config" "$workspace.cdenv" printf direct-openssh | grep -qx direct-openssh

[[ $(wc -w <<<"$container_id") -eq 1 ]] || {
  echo "expected exactly one running smoke container" >&2
  exit 1
}
docker inspect --format '{{json .HostConfig.PortBindings}}' "$container_id" | grep -vq '22/tcp'
docker exec "$container_id" /bin/sh -c '! command -v sshd >/dev/null && ! ps | grep "[s]shd" >/dev/null'

# Exercise the installed foreground forwarding wrapper against a process started
# through the authenticated agent transport. BusyBox httpd daemonizes itself.
cdenv --root "$root" ssh "$workspace" -- /bin/sh -c \
  'mkdir -p /tmp/cdenv-smoke-www; printf forwarded >/tmp/cdenv-smoke-www/index.html; busybox httpd -p 3000 -h /tmp/cdenv-smoke-www'
local_port=$((20000 + ($$ % 20000)))
cdenv --root "$root" forward "$workspace" "$local_port:3000" >"$work/forward.log" 2>&1 &
forward_pid=$!
forwarded=
for _ in $(seq 1 50); do
  if exec 3<>"/dev/tcp/127.0.0.1/$local_port" 2>/dev/null; then
    printf 'GET / HTTP/1.0\r\nHost: localhost\r\n\r\n' >&3
    forwarded=$(cat <&3)
    exec 3>&-
    break
  fi
  sleep 0.1
done
grep -q forwarded <<<"$forwarded"
kill "$forward_pid"
wait "$forward_pid" || true
forward_pid=

cdenv --root "$root" down "$workspace"
cdenv --root "$root" down "$workspace"
cdenv --root "$root" up "$workspace"
[[ $(docker ps -q --filter "label=cdenv.workspace=$workspace") == "$container_id" ]]
cdenv --root "$root" rebuild "$workspace"
rebuilt_id=$(docker ps -q --filter "label=cdenv.workspace=$workspace")
rebuilt_generation=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["active"]["generation"])' "$root/workspaces/$workspace/state.json")
[[ "$rebuilt_id" != "$container_id" && "$rebuilt_generation" -eq $((generation + 1)) ]]
cdenv --root "$root" rebuild "$workspace" --no-cache
no_cache_id=$(docker ps -q --filter "label=cdenv.workspace=$workspace")
no_cache_generation=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["active"]["generation"])' "$root/workspaces/$workspace/state.json")
[[ "$no_cache_id" != "$rebuilt_id" && "$no_cache_generation" -eq $((rebuilt_generation + 1)) ]]
[[ $(sha256sum "$root/ssh/host_keys/$workspace.pub" | awk '{print $1}') == "$host_key" ]]
grep -q tracked-change "$checkout/tracked-smoke.txt"
grep -qx untracked-change "$checkout/untracked-smoke.txt"
[[ $(docker exec "$no_cache_id" cat /data/smoke) == volume-data ]]
[[ $(docker inspect --format '{{.Id}}' "$unrelated_id") == "$unrelated_id" ]]
cdenv --root "$root" down "$workspace"

[[ -z $(docker ps -q --filter "label=cdenv.workspace=$workspace") ]]
[[ ! -e "$home/.ssh/config" ]] || ! grep -q 'cdenv' "$home/.ssh/config"

echo "installed V1 operational smoke passed for $(basename "$archive")"
