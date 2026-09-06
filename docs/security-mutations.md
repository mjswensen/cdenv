# Targeted Feature security mutation gate

This is an automated **source-mutation** gate, independent of informational
[coverage](coverage.md) and the handwritten frozen-lock mutation-boundary cases.
It requires **cargo-mutants 27.1.0**, installed with `--locked`, and the stable
compiler in `rust-toolchain.toml` (initial baseline: Rust 1.97.1). Python 3.12+ and
Git drive selection, isolation, source verification and report validation.
No Docker, registry credentials or public-network test fixtures are needed;
installing the runner and fetching locked Cargo dependencies still require access
to their package sources.

## Reviewed targets and baseline

The version-controlled policy consists of:

- `.cargo/security-mutants.json`: exact mutant names (file, location, operation),
  stable IDs, named shards, security rationale, and the focused test required to
  kill each mutant;
- `.cargo/security-mutants-baseline.json`: SHA-256 of each reviewed runner diff,
  including its source context, keyed by target ID;
- `.cargo/security-mutants.toml`: locked, library-only Cargo invocations with the
  `feature` test-name filter; target/VCS directories are not copied;
- `.cargo/security-mutants-triage.json`: explicit, expiring survivor exceptions
  (initially empty).

Baseline **`security-feature-v1`** was exercised on Linux arm64 with the product
source at `f96b4adcced0` plus the focused test additions in this change. No product
behavior was changed. The gate ran from a clean, committed disposable validation
checkout, and both original source snapshots matched for all three shards:

| Named shard | Targeted | Caught by named test | Survived / unviable / timed out |
| --- | ---: | ---: | ---: |
| reference-lock | 6 | 6 | 0 / 0 / 0 |
| transport | 8 | 8 | 0 / 0 / 0 |
| archive-integrity | 6 | 6 | 0 / 0 / 0 |

The reviewed security boundaries are:

- **Reference and frozen lock:** contained local separators, username-only and
  password-only HTTPS credentials, private hosts, SHA-256 syntax, frozen
  resolution equality, and structural lock validity.
- **OCI authentication and redirects:** preservation of realm/service/scope,
  origin and effective-port boundaries for bearer retention, HTTPS-only targets,
  credential rejection, and private-host rejection during redirect validation.
- **Manifest/blob and archive:** descriptor size/digest verification, changed
  same-length blob content, independent absolute/traversing-path validation,
  exact path byte limits, and exact extraction entry-count limits.

Each ID's rationale and test are in the manifest rather than a second drifting
copy of the target list. Inspection of each mutant's test log confirmed the
**named test failed**, not merely an unrelated test or compiler invocation.
New tests make the credential-only, direct path-policy and same-length blob
boundaries explicit; relying only on downstream tar/filesystem failures can mask
a disabled path validator. Boundary inversions also test that exact valid bounds
remain usable, not just that malformed inputs fail somewhere.

This intentionally bounded list is **not** an exhaustive mutation score. In
particular, pure redirect-policy mutations do not claim an end-to-end TLS or
credential-stripping test of every HTTP dispatch path, and archive targets do not
cover every hostile tar entry type. Add such targets and focused fixtures through
review; do not describe unselected mutants as killed or triaged. The existing
ordinary tests and both required release suites remain separate gates.

## Deterministic execution and isolation

From a clean committed checkout:

```sh
cargo install cargo-mutants --version '=27.1.0' --locked
python3 .github/scripts/security-mutations.py reference-lock
python3 .github/scripts/security-mutations.py transport
python3 .github/scripts/security-mutations.py archive-integrity
```

CI runs these three **named shards on Ubuntu 24.04 x86_64**, with fail-fast disabled
and a 20-minute job limit. All three `Security mutations (...)` checks are required;
a single shard cannot stand in for the complete gate. These CPU/file-only security
checks do not need to duplicate the release-suite dependency/architecture matrix.
The local baseline is not a claim that the GitHub-hosted x86_64 run has executed.

Within each shard the runner uses exact anchored name selection, source order
(`--no-shuffle`), one mutation worker, a 300-second build limit, a 60-second test
limit and an unmutated baseline. Shard membership is checked in explicitly;
there is no sampling, random seed, changed-lines filter, cached-kill iteration,
or percentage threshold. Selection must exactly equal the nonempty reviewed list
and every selected diff must match its baseline hash. A moved, removed or changed
target fails until reviewed, even if the changed code seems equivalent.

The wrapper hashes tracked source bytes, symlink targets, file modes, HEAD and
index/worktree status, and refuses dirty or untracked source inputs. It archives
`HEAD` into an OS temporary directory and invokes cargo-mutants there; the runner
then makes its own scratch copy. `--in-place` is never used. Instrumentation/build
environment overrides are removed and the pinned stable toolchain is selected.
A locked Cargo metadata preflight runs before discovery, and the archived lockfile
is compared afterward (the runner does not forward Cargo flags to its own metadata
query). The original checkout is not used for mutation, build or test execution. Cargo's
dependency cache may be shared, but mutable target directories are not copied or
cached. Temporary copies are cleaned on normal completion/failure.

The wrapper verifies source state in `finally`, including error paths. A separate
CI `always()` step independently repeats the comparison:

```sh
python3 .github/scripts/security-mutations.py transport --verify-source
```

Ignored build outputs under `target/`, Cargo caches, Git internal bookkeeping and
the separate ignored `.issues/` worktree are outside the source-byte claim. New
nonignored files, content edits, mode/link changes, index changes and HEAD changes
are detected. A missing pre-run snapshot makes independent verification fail;
a cancelled or incomplete run cannot become a successful gate. Local reruns must
remove their previous `target/security-mutations/<shard>/` report directory first;
stale reports are never consumed as a new result.

## Survivors and errors

The following always fail:

- missing/empty/duplicate targets, stale diff hashes or baseline revisions;
- wrong runner version, nonzero runner errors, incomplete/missing outcomes;
- failed/empty unmutated baseline or an absent/ignored required focused test;
- compile-unviable mutants, timeouts, check-only outcomes, or a supposed kill
  without the named test failing;
- untreated survivors, unknown triage IDs, missing owners/reasons, and expired
  triage (expiry date **on or before** the current UTC date);
- changes to the checked-out source state.

For a survivor, inspect the raw diff and mutant test log and write a focused test
that passes unmutated and fails for that source change. Add/update the exact test
name in the target record and rerun the shard. Prefer killing the mutant over
waiving it. If it is genuinely equivalent or a temporary gap must be accepted,
a maintainer must explicitly review an entry keyed by its existing target ID:

```json
{
  "target-id": {
    "owner": "@responsible-maintainer",
    "expires": "YYYY-MM-DD",
    "reason": "Concrete equivalence argument or tracked remediation issue and why deferred"
  }
}
```

Use a short, actionable expiry (normally within 30 days). This example is not an
active exception. Every triage entry is validated at run start, even if it belongs
to another shard or the mutant now dies. Triage only permits a **MissedMutant**
that actually ran its focused test; it cannot suppress infrastructure, timeout,
compile or selection errors. The report retains both the missed outcome and its
owner/expiry/reason, never relabeling it caught. Remove obsolete exceptions.

## Reports and baseline changes

Each job uploads `security-mutations-linux-x86_64-<shard>` for **14 days**, even on
failure, with missing files treated as an error. Under
`target/security-mutations/<shard>/`:

- `gate.json`: success/error, source revision, runner/compiler identity, baseline
  revision and manifest/baseline hashes, exact command, per-mutant source/test
  linkage and any applied triage;
- `selection.json`: full selected mutants and source diffs before execution;
- `metadata.log`, `runner.log`, `mutants.out/outcomes.json`, `mutants.out/log/` and
  `mutants.out/diff/`: raw runner evidence, including failures and survivors;
- `source-before.json`, `source-after.json`, `source-final.json`: source byte/mode,
  HEAD and index/worktree evidence from wrapper and independent CI verification.

A wrapper error may stop `gate.json` before per-mutant interpretation completes;
raw outcomes/selection still identify all executed/selected mutants. A setup error
may leave no runner output: that is missing evidence, not a pass. Retain the report
with release evidence before CI artifact expiry when approving a new baseline.

For source moves, runner upgrades or intentional target changes, enumerate with
the proposed pinned runner in a disposable clean branch:

```sh
cargo mutants --no-config --list --json \
  --file 'crates/cdenv-devcontainer/src/feature/*.rs' \
  --file 'crates/cdenv-cli/src/feature_sources/*.rs' > target/mutants-candidate.json
```

Review the actual diffs, threat rationale, focused-test assertions and shard
assignment. Update exact names and compute each accepted diff hash as
`hashlib.sha256(mutant["diff"].encode()).hexdigest()`; advance `baseline_revision`
and the matching baseline `revision` together. Never automatically bless a new
list, silently drop a survivor or copy hashes just to turn CI green. Rerun all
three shards, record counts and source/compiler/runner revisions, and retain
before/after evidence. The wrapper's dependency-free negative-path tests run in
ordinary CI via
`PYTHONDONTWRITEBYTECODE=1 python3 .github/scripts/test-security-mutations.py`.
