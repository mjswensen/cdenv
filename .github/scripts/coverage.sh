#!/usr/bin/env bash
# Informational instrumentation only; the pinned-toolchain release gates stay separate.
set -euo pipefail
scope=${1:?expected workspace, devcontainer-v1, or openssh}
case "$scope" in
  workspace) profile=() ;;
  devcontainer-v1|openssh) profile=(--release) ;;
  *) echo "Unknown coverage scope: $scope" >&2; exit 2 ;;
esac
: "${COVERAGE_ID:?matrix-qualified artifact ID required}"
: "${COVERAGE_TOOLCHAIN:?pinned nightly required for branch instrumentation}"
export RUSTUP_TOOLCHAIN="$COVERAGE_TOOLCHAIN"
# show-env uses Cargo metadata's target directory for subsequent report discovery.
export CARGO_TARGET_DIR="$PWD/target/coverage-build/$scope"
export CARGO_LLVM_COV_TARGET_DIR="$CARGO_TARGET_DIR"
out="$PWD/target/coverage/$COVERAGE_ID"
mkdir -p "$out"
# Capture setup output before evaluating: a failed show-env must not be swallowed.
cargo llvm-cov show-env --branch --locked --sh > "$out/environment.sh"
source "$out/environment.sh"
cargo llvm-cov clean --workspace
{
  rustc --version --verbose
  cargo llvm-cov --version
  git rev-parse HEAD
  printf 'scope=%s\nprofile=%s\n' "$scope" "${profile[*]:-debug}"
} > "$out/versions.txt"

status=0
if [[ "$scope" == workspace ]]; then
  cargo test --workspace --locked --all-targets 2>&1 | tee "$out/tests.log" || status=$?
else
  # Use the real gate, including dependency/architecture checks and discovered == passed.
  cargo xtask test-integration --suite "$scope" 2>&1 | tee "$out/tests.log" || status=$?
fi
printf '%s\n' "$status" > "$out/test-exit-code.txt"
# Even failed tests may produce useful partial reports, but never turn green.
report_status=0
for format in json lcov html; do
  args=(--output-path "$out/coverage.$format")
  if [[ "$format" == html ]]; then args=(--output-dir "$out"); fi
  cargo llvm-cov report "${profile[@]}" --branch --locked \
    --ignore-filename-regex '(^|/)(tests|xtask)/' \
    "--$format" "${args[@]}" 2>&1 | tee -a "$out/reports.log" || report_status=$?
done
printf '%s\n' "$report_status" > "$out/report-exit-code.txt"
if (( status != 0 )); then exit "$status"; fi
exit "$report_status"
