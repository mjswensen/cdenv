# Informational release coverage

CI uses **cargo-llvm-cov 0.9.0** (installed with `--locked`) and
**nightly-2026-09-01 + llvm-tools-preview** for experimental branch instrumentation.
Version 0.9.0 understands the pinned nightly's Cargo artifact layout; older
collectors can silently miss test binaries and produce incomplete/empty reports.
The nightly is intentionally separate from `rust-toolchain.toml`: existing strict
checks and both required release suites still run with the pinned stable compiler.
Coverage cannot replace their results. No percentage threshold is enforced.
Dependency installation, instrumentation, test execution, and report integrity
failures are errors, not informational successes.

## Matrix and artifacts

`.github/workflows/ci.yml` produces seven artifacts, retained for **14 days**:

- `coverage-workspace-{linux-arm64,macos-arm64}`: locked ordinary workspace
  tests (`--all-targets`, default features, debug profile).
- `coverage-{devcontainer-v1,openssh}-linux-arm64-{minimum,pinned}`:
  locked release-profile suite reruns in the same native jobs and selected
  Docker/Compose/OpenSSH baselines as the required gates.
- Additionally, `coverage-index` contains `index.md` and `index.json`, a combined,
  scope-qualified inventory of function/branch counts, also shown in the job summary.

Each collection artifact contains:

- `coverage.json`: full LLVM export, including functions, regions and branches;
- `coverage.lcov`: mergeable LCOV function/line/branch records;
- `html/index.html`: human-readable source navigation, function and branch details;
- `tests.log`, `test-exit-code.txt`, `reports.log`, `report-exit-code.txt`,
  `versions.txt`, `environment.sh`: execution
  outcome, gate discovery/pass counts, compiler/tool/commit identity and the
  generated instrumentation environment (not a dump of runner secrets).

Reports are attempted after test failure and uploaded with `always()`, but the
original failure exit code is preserved. A setup failure or skipped suite may
leave no report: upload and inventory checks fail rather than supplying an empty
successful result. The inventory requires all eleven successful collections,
nonempty artifacts and actual function and branch instrumentation. An all-zero
*covered* count is allowed; a zero instrumentation denominator is not.

The instrumented suites invoke the unchanged `cargo xtask test-integration`
entry point: missing/below-baseline dependencies, unsupported/mismatched native
architecture, zero discovered tests, ignored/unexecuted tests, and test failures
remain errors. A failed earlier gate prevents later coverage steps from running;
missing coverage is not evidence that a gate passed. macOS has ordinary workspace
coverage only, **not** an automated Docker Desktop release-suite claim.

## Isolation, merging and exclusions

Each scope starts with `cargo llvm-cov clean --workspace` in its own
`target/coverage-build/<scope>` target directory. `show-env --branch` instruments
Cargo subprocesses, including xtask's release test invocations. LLVM merges the
profiles for that collection when generating each report. The profile's PID/module
identifiers prevent concurrent test processes from overwriting one another.
No raw profiles or build directories are cached or uploaded.

The full JSON/LCOV exports retain per-file/function/branch records for downstream
analysis. Merge LCOV traces only for identical source revisions, compiler/tool
versions, architecture, profile, feature set and exclusion rules (for example,
repeat/sharded runs of the same scope using `lcov --add-tracefile`). Deduplicate
records by source location/function identity; never add percentages. Raw LLVM
profiles require their exact instrumented binaries and matching LLVM, so are not
a portable interchange format. `coverage-index` deliberately merges presentation,
not execution counts or denominators: do **not** union Linux/macOS, debug/release,
or minimum/pinned rows into a claimed workspace percentage.

Reports exclude paths matching `(^|/)(tests|xtask)/`: test fixtures, integration
harnesses and build/release orchestration are not product coverage. They still
execute. cargo-llvm-cov's default exclusions also remove external dependencies,
Rust's sysroot, generated target files and build scripts; build-script coverage is
not opted in. Inline unit tests share product files and are not separately removed.
Default-feature workspace tests do not activate the feature-gated named suites;
they are collected separately. Doctests are not collected. Prebuilt/static agents,
Docker image contents, external tools, subprocesses that clear instrumentation
environment, and forcibly terminated processes are not implied covered. HTML is
host-side instrumented Rust coverage, not end-to-end coverage of container code.

## Reviewed baseline and regression investigation

The initial **source-reviewed baseline** is the matrix and exclusions above:
ordinary workspace tests concentrate on pure library behavior; Dev Container
coverage additionally includes the focused CLI/devcontainer library regressions;
OpenSSH coverage exercises the interoperability fixture/server. Suite percentages
are expected to differ substantially and are not comparable to workspace totals.
macOS excludes Linux-only agent implementation through conditional compilation.
Branch coverage is experimental and does not prove every Rust pattern or boolean
condition has been exercised.

Initial locally measured and reviewed baseline (Linux arm64, product source at
`10c2755`, with this coverage change; tools above, Docker Engine 29.7.2 and
OpenSSH 10.0p2):

| Scope | Passed tests | Functions covered/total | Branches covered/total |
| --- | ---: | ---: | ---: |
| workspace | 544 | 2313/3422 (67.59%) | 1812/3149 (57.54%) |
| devcontainer-v1 | 240 focused + 8 suite | 1519/2837 (53.54%) | 1057/2223 (47.55%) |
| openssh | 4 suite | 16/299 (5.35%) | 23/488 (4.71%) |

Review confirmed that product source records remain, test/xtask paths are excluded,
function and branch denominators are nonzero, and discovered suite counts equal
passed counts. The low OpenSSH figure measures only the agent source linked into
the fixture server, not all host code or all agent deployment modes. These numbers
are an initial local reference, **not** claims about unexecuted CI matrix rows or
the minimum/pinned Docker baselines. No HTML/profile mismatch warning occurred
with the selected collector. An earlier workspace rerun covered 1811 rather than
1812 branches with identical denominators: retain per-run counts rather than
assuming every execution yields a bit-for-bit identical trace.

The first complete green eleven-report CI run must be reviewed as the matrix
baseline. Record its commit, CI run URL, compiler/tool versions, counts per row
and exclusion changes in the PR/release review, and retain the downloaded
index/relevant reports with release evidence before the 14-day CI retention
expires. Do not invent a percentage target or compare a partial run to that baseline.

For a regression, first confirm all expected tests executed and compare identical
matrix rows/tool versions. Inspect changed uncovered functions and branch arms in
HTML and JSON, then inspect source changes, feature/cfg changes, fixture coverage,
and processes that may not flush profiles. Add focused behavioral tests where
coverage exposes a missing assertion; do not exclude hard-to-test product paths
to improve a number. Review toolchain/exclusion changes as baseline changes and
keep their before/after reports. Coverage cannot replace mutation/security tests.

To reproduce on a supported host with the matching release dependencies:

```sh
export COVERAGE_TOOLCHAIN=nightly-2026-09-01 CARGO_LLVM_COV_VERSION=0.9.0
bash .github/scripts/install-coverage.sh
COVERAGE_ID=workspace-local bash .github/scripts/coverage.sh workspace
CDENV_INTEGRATION=1 CDENV_INTEGRATION_ARCH=arm64 \
  COVERAGE_ID=openssh-local bash .github/scripts/coverage.sh openssh
```

Use the native `arm64` declaration on ARM64 hosts. Local IDs are for inspection,
not inputs to the CI matrix inventory. Reports are under `target/coverage/<ID>/`.
