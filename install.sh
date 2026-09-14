#!/bin/sh
# Install the latest cdenv release, or the release selected by CDENV_VERSION.
# Intended for: curl -fsSL https://github.com/mjswensen/cdenv/releases/latest/download/install.sh | sh
set -eu

repo=${CDENV_REPOSITORY:-mjswensen/cdenv}
version=${CDENV_VERSION:-latest}
api_root=${CDENV_API_ROOT:-https://api.github.com}

case "$(uname -s):$(uname -m)" in
  Linux:x86_64|Linux:amd64) platform=linux-x86_64 ;;
  Linux:aarch64|Linux:arm64) platform=linux-aarch64 ;;
  Darwin:arm64) platform=macos-aarch64 ;;
  *)
    echo "cdenv: unsupported host $(uname -s)/$(uname -m)" >&2
    exit 1
    ;;
esac

if [ "$version" = latest ]; then
  release_url="$api_root/repos/$repo/releases/latest"
elif printf '%s' "$version" | grep -q '^v'; then
  release_url="$api_root/repos/$repo/releases/tags/$version"
else
  release_url="$api_root/repos/$repo/releases/tags/v$version"
fi

tmp=$(mktemp -d 2>/dev/null || mktemp -d -t cdenv)
trap 'rm -rf "$tmp"' EXIT INT TERM
release_json=$tmp/release.json
curl -fsSL "$release_url" -o "$release_json"

archive_url=$(awk -v prefix="cdenv-$platform-" '
  /"name"[[:space:]]*:/ {
    name=$0
    sub(/^.*"name"[[:space:]]*:[[:space:]]*"/, "", name)
    sub(/".*$/, "", name)
  }
  /"browser_download_url"[[:space:]]*:/ {
    url=$0
    sub(/^.*"browser_download_url"[[:space:]]*:[[:space:]]*"/, "", url)
    sub(/".*$/, "", url)
    if (name ~ ("^" prefix) && name ~ /\.tar$/) {
      print url
      exit
    }
  }
' "$release_json")

if [ -z "$archive_url" ]; then
  echo "cdenv: no $platform archive found in release $version" >&2
  exit 1
fi

archive_name=${archive_url##*/}
checksum_url=${archive_url%.tar}.tar.sha256
archive=$tmp/$archive_name
checksum=$tmp/$archive_name.sha256
curl -fsSL "$archive_url" -o "$archive"
curl -fsSL "$checksum_url" -o "$checksum"
expected=$(awk 'NF { print $1; exit }' "$checksum")
if [ "${#expected}" -ne 64 ]; then
  echo "cdenv: invalid SHA-256 checksum" >&2
  exit 1
fi
case "$expected" in
  *[!0-9a-f]*)
    echo "cdenv: invalid SHA-256 checksum" >&2
    exit 1
    ;;
esac
# Capture the hasher directly: a pipeline would hide its failure behind awk.
case "$(uname -s)" in
  Darwin) actual=$(shasum -a 256 "$archive") ;;
  *) actual=$(sha256sum "$archive") ;;
esac
actual=${actual%% *}
if [ "$expected" != "$actual" ]; then
  echo "cdenv: checksum verification failed" >&2
  exit 1
fi

mkdir -p "$tmp/unpacked"
tar -xf "$archive" -C "$tmp/unpacked"
if [ ! -f "$tmp/unpacked/cdenv" ]; then
  echo "cdenv: release archive did not contain cdenv" >&2
  exit 1
fi
chmod 755 "$tmp/unpacked/cdenv"

install_dir=${CDENV_INSTALL_DIR:-}
if [ -z "$install_dir" ]; then
  old_ifs=$IFS
  IFS=:
  for candidate in $PATH; do
    IFS=$old_ifs
    [ -n "$candidate" ] || candidate=.
    if [ ! -d "$candidate" ] && [ "$candidate" = "${HOME:-}/.local/bin" ]; then
      mkdir -p "$candidate" 2>/dev/null || continue
    fi
    if [ -d "$candidate" ] && [ -w "$candidate" ]; then
      install_dir=$candidate
      break
    fi
    IFS=:
  done
  IFS=$old_ifs
fi
if [ -z "$install_dir" ]; then
  echo "cdenv: no writable directory in PATH; set CDENV_INSTALL_DIR and retry" >&2
  exit 1
fi

mkdir -p "$install_dir"
target=$install_dir/cdenv
installed=$tmp/cdenv
cp "$tmp/unpacked/cdenv" "$installed"
chmod 755 "$installed"
mv "$installed" "$target"
echo "cdenv installed to $target"
