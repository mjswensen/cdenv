# Release packages

For informational workspace and release-suite function/branch reports, artifact
retention and baseline review, see [coverage reporting](coverage.md). Coverage does
not replace the required release gates or installed-package smoke checks. The
independent [Feature security mutation gate](security-mutations.md) requires the
reviewed targeted mutants to be killed or explicitly owned and time-limited. The
[GitHub Actions maintenance guide](ci-actions.md) describes action upgrade and
artifact compatibility review.

From a clean checkout with the pinned Rust toolchain, run `cargo xtask dist`.
The Cargo alias and all release builds use `--locked`. Docker Buildx and QEMU
support for both Linux architectures are required when building both agents
locally on one architecture. CI instead builds each agent on its matching native
Linux runner with `cargo xtask stage-agent --platform <platform>`.

The command builds and runs the `version` command of both Linux musl agents,
checks their exact build/version/protocol identity and static ELF architecture,
and embeds them in the native host. The full Git commit is the shared build ID.
No timestamp or process ID is used. The tar member has fixed ownership, mode,
and timestamp; host source paths are remapped. Reproducibility assumes the same
pinned toolchain, native SDK/linker and agent builder image/toolchain inputs.

Outputs in `target/dist/`:

- `cdenv-<os>-<architecture>-<commit>.tar`: exactly one regular executable, `cdenv`;
- `.tar.sha256`: SHA-256 and exact archive filename;
- `.tar.json`: deterministic platform, version, build ID, protocol and both
  embedded agent SHA-256 identities;
- `install.sh`: the checksum-verifying GitHub release installer. Tagged builds
  upload this script together with the platform archives to a draft release.

`cargo xtask test-package <archive.tar>` checks the adjacent checksum and metadata,
validates canonical archive contents and native executable format/architecture,
extracts to a temporary directory, and runs `--version` and the private
`__validate-artifacts` entry point. The latter structurally validates both embedded
agents and reports their hashes. Comparing those hashes to the build-time manifest
connects the embedded bytes to the agents whose runtime identities were verified
on Linux; it does not attempt to execute Linux binaries on macOS. Run the smoke
test only on trusted release packages, on their declared native host.

The package-only smoke path clears the host process environment and requires no
Docker, OpenSSH, installation, consent, or home directory. Development binaries
without staged agents fail artifact validation. It deliberately makes no runtime
claim.

On Linux, `cargo xtask test-installed <archive.tar>` validates and extracts that
exact archive into an isolated `PATH` and root, then runs
[`installed-smoke.sh`](../tests/release/installed-smoke.sh). The script owns the
operational scenarios and cleanup. It must exercise the release binary without
Node.js or an editor and verify that no `sshd` or published port 22 exists. Both
Linux archive architectures run this gate; the macOS equivalent is the retained,
versioned [Docker Desktop checklist](smoke/macos-docker-desktop-v1.md).

## Publishing a tagged release

The release workflow stops at a **review-ready draft**, with manual macOS runtime
evidence explicitly pending. Only the release-upload job receives repository
write permission. Nothing in this workflow publishes the release.

Use SemVer for the shared workspace version and a matching `v<version>` tag.
Before tagging, update `[workspace.package].version`, the version requirements
for local `[workspace.dependencies]`, and the workspace entries in `Cargo.lock`
(run Cargo to regenerate the lockfile). Commit those changes. For example:

```sh
cargo check --workspace
cargo xtask check
git tag -a v0.1.0 -m 'Release v0.1.0'
git push origin v0.1.0
```

Pushing the tag invokes the [release workflow](../.github/workflows/release.yml),
including [CI](../.github/workflows/ci.yml) from the same commit. Those workflows
own the job matrix, native agent assembly, reproducibility checks, and package
and installed-smoke commands. The
[release validator](../.github/scripts/release-ready.py) enforces tag/workspace/
lockfile agreement and the complete package, coverage, mutation, and CI evidence
inventory before creating the draft. Its
[regression tests](../.github/scripts/test-release-ready.py) cover rejection paths.

The same run holds both CI and package evidence. Integration suites rebuild a
packaged host from the same commit; installed smoke exercises the actual release
archives. Rebuilt integration binaries are not asserted to be those exact archive
bytes. Detailed quality-job output is available at the linked Actions run;
the durable evidence bundle retains its aggregate job result, not a copy of every
Actions console log.

Additional review assets:

| Asset | Contents |
| --- | --- |
| `release-manifest.json` | Version/tag/commit, run and attempt, all required CI job results, package hashes, shared agent hashes, asset hashes, and pending manual status. |
| `release-evidence.tar.gz` | Coverage and ordinary release-suite logs/reports, security mutation records, package build/rebuild/smoke logs, and aggregate CI results. |
| `release-review.md` | Maintainer handoff prepended to generated release notes. |
| `macos-docker-desktop-v1.md` | The checklist from the tagged source. |
| `manual-macos-smoke.py` | The example preparation/basic-smoke helper from the tagged source. |

The manifest hashes detect accidental asset mismatches; they are not an
independent signature or provenance attestation. The draft and its assets remain
trusted maintainer inputs. Assets are also retained as the `review-ready-release`
Actions artifact. A rerun does not overwrite an existing draft or its assets:
`gh release create` fails if the release already exists. Inspect any partial draft
and reconcile it explicitly before retrying; never silently replace reviewed bytes.

## Manual Apple Silicon handoff

Use a checkout of the exact candidate tag, Python 3.11+, authenticated GitHub CLI
with access to the draft, Git, local Docker Desktop, and the OpenSSH baseline.
The helper does not build from source:

```sh
python3 tests/release/manual-macos-smoke.py v0.1.0 --repo mjswensen/cdenv
# To also run the bounded noninteractive baseline:
python3 tests/release/manual-macos-smoke.py v0.1.0 --repo mjswensen/cdenv --run-basics
```

Choose one invocation; each creates a new private directory under `/private/tmp`.
The [helper](../tests/release/manual-macos-smoke.py) verifies the draft assets and
runtime identity, prepares fixtures and a partial `smoke-record.md`, and writes
`MANUAL.md` with remaining steps and scoped cleanup instructions. Authentication
comes from `gh`; draft assets need not be public. `--run-basics` additionally runs
the bounded noninteractive subset. Logs and environments remain available after
success or failure; inspect the retained directory if interrupted.

Preparation and baseline success **never** mark the full smoke passed. Complete
every required field and all twelve steps in the
[Apple Silicon Docker Desktop checklist](smoke/macos-docker-desktop-v1.md), including
PTY behavior, lock rejection, forwarding, Compose isolation/volumes, recovery,
real native helper/keychain and selected-agent observations, secret-marker scans,
and cleanup. Label simulations explicitly; they cannot substitute for a required
real observation. Record test credentials only in controlled private fixtures,
and inspect/redact all evidence before uploading it.

Then:

1. Attach the completed passing `smoke-record.md` and reviewed, redacted logs to
   the draft. The generated `MANUAL.md` includes an example upload command.
2. Review the automated evidence and update the draft's pending-status notes to
   reference the manual record. The manifest's pending field records the original
   CI handoff; keep that original manifest unchanged.
3. Publish through GitHub's release UI. If any required evidence is missing or
   fails, leave the release in draft. Keep candidates marked prerelease; designate
   the intended stable release as latest.

After publication, exercise the public installer against the exact tag:

```sh
install_dir=$(mktemp -d)
curl -fsSL https://github.com/mjswensen/cdenv/releases/download/v0.1.0/install.sh |
  CDENV_VERSION=v0.1.0 CDENV_INSTALL_DIR="$install_dir" sh
"$install_dir/cdenv" --version
"$install_dir/cdenv" __validate-artifacts
```

For a stable release, also exercise the README's `/releases/latest/download/install.sh`
URL with `CDENV_INSTALL_DIR` set to a temporary directory and no version override.
The manual smoke helper never uploads evidence, publishes, modifies your normal
install, or automatically removes the retained Docker resources.

Draft releases are not available to the public installer. The default installation
command continues to use the latest published, non-prerelease release until a
maintainer publishes the new release. Mark release candidates as prereleases
before publishing them so they do not become the default installation.

Hosted macOS packaging requires no Docker daemon and makes no Docker Desktop
runtime claim. That boundary is why the manual checklist remains a publication
gate even after every automated job succeeds.
