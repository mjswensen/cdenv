#!/usr/bin/env bash
set -euo pipefail
: "${COVERAGE_TOOLCHAIN:?}"
: "${CARGO_LLVM_COV_VERSION:?}"
rustup toolchain install "$COVERAGE_TOOLCHAIN" --profile minimal --component llvm-tools-preview
cargo install cargo-llvm-cov --version "=$CARGO_LLVM_COV_VERSION" --locked
cargo llvm-cov --version
