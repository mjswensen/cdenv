#!/usr/bin/env bash
set -euo pipefail
: "${COVERAGE_TOOLCHAIN:?}"
: "${CARGO_LLVM_COV_VERSION:?}"
rustup toolchain install "$COVERAGE_TOOLCHAIN" --profile minimal --component llvm-tools-preview
# A dedicated install root can be cached without restoring unrelated Cargo tools.
tool_root="$PWD/target/coverage-tools"
export PATH="$tool_root/bin:$PATH"
if [[ ! -x "$tool_root/bin/cargo-llvm-cov" ]] ||
   [[ $("$tool_root/bin/cargo-llvm-cov" --version) != "cargo-llvm-cov $CARGO_LLVM_COV_VERSION" ]]; then
  cargo install cargo-llvm-cov --version "=$CARGO_LLVM_COV_VERSION" --locked --root "$tool_root"
fi
if [[ -n ${GITHUB_PATH:-} ]]; then printf '%s\n' "$tool_root/bin" >> "$GITHUB_PATH"; fi
cargo llvm-cov --version
