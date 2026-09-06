#!/usr/bin/env bash
set -Eeuo pipefail

version=10.0p2
sha256=021a2e709a0edf4250b1256bd5a9e500411a90dddabea830ed59cef90eb9d85c
prefix="${RUNNER_TEMP:?}/openssh-$version"
source_dir="$RUNNER_TEMP/openssh-$version-source"
archive="$RUNNER_TEMP/openssh-$version.tar.gz"

curl --fail --silent --show-error --location \
  "https://cdn.openbsd.org/pub/OpenBSD/OpenSSH/portable/openssh-$version.tar.gz" \
  --output "$archive"
printf '%s  %s\n' "$sha256" "$archive" | sha256sum --check --strict
rm -rf "$source_dir" "$prefix"
mkdir -p "$source_dir" "$prefix/bin"
tar -xzf "$archive" -C "$source_dir" --strip-components=1
(
  cd "$source_dir"
  ./configure --prefix="$prefix" --without-openssl-header-check
  make -j"$(getconf _NPROCESSORS_ONLN)" ssh ssh-keygen
)
install -m 0755 "$source_dir/ssh" "$source_dir/ssh-keygen" "$prefix/bin/"
"$prefix/bin/ssh" -V
if [[ -n ${GITHUB_PATH:-} ]]; then
  printf '%s\n' "$prefix/bin" >>"$GITHUB_PATH"
else
  printf '%s\n' "$prefix/bin"
fi
