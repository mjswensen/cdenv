#!/usr/bin/env bash
# Preserve one explicit ordinary-gate and coverage outcome for every release suite.
set -euo pipefail

command=${1:?expected init or run}
shift

case "$command" in
  init)
    (($# > 0)) || { echo "init requires at least one coverage ID" >&2; exit 2; }
    for identifier in "$@"; do
      out="$PWD/target/coverage/$identifier"
      mkdir -p "$out"
      printf 'not-run\n' > "$out/gate-exit-code.txt"
      printf '%s\n' \
        'Ordinary release gate not run: a workflow prerequisite may not have succeeded.' \
        > "$out/gate.log"
      printf 'not-run\n' > "$out/setup-exit-code.txt"
      printf 'not-run\n' > "$out/test-exit-code.txt"
      printf 'not-run\n' > "$out/report-exit-code.txt"
    done
    ;;
  run)
    suite=${1:?run requires a suite}
    : "${COVERAGE_ID:?matrix-qualified artifact ID required}"
    out="$PWD/target/coverage/$COVERAGE_ID"
    mkdir -p "$out"
    status=0
    cargo xtask test-integration --suite "$suite" 2>&1 | tee "$out/gate.log" || status=$?
    printf '%s\n' "$status" > "$out/gate-exit-code.txt"
    exit "$status"
    ;;
  *)
    echo "Unknown release evidence command: $command" >&2
    exit 2
    ;;
esac
