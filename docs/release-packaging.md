# Release packages

For informational workspace and release-suite function/branch reports, artifact
retention and baseline review, see [coverage reporting](coverage.md). Coverage does
not replace the required release gates or installed-package smoke checks. The
independent [Feature security mutation gate](security-mutations.md) requires the
reviewed targeted mutants to be killed or explicitly owned and time-limited. The
[GitHub Actions runtime baseline](ci-actions.md) records workflow action versions,
runner requirements, and artifact compatibility.

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

On Linux, `cargo xtask test-installed <archive.tar>` first repeats package and
adjacent-checksum validation, then extracts that exact archive into an isolated
`PATH` and root and runs `tests/release/installed-smoke.sh`. The workflow shadows
Node/editor commands with failing stubs, creates a local Git fixture, exercises
the applicable implementation-plan §19 list/status/doctor/lock/down/up/rebuild,
OpenSSH, and foreground-forward commands, and verifies that no `sshd` or published
port 22 exists. It covers no-hook SSH, configured pre-session `postAttachCommand`
execution, reconnect and generation transitions, and fail-closed recovery from
missing/corrupt lifecycle manifests. It cleans up its container/root and never
substitutes a build-tree binary. The release workflow runs this installed smoke on
both Linux archive architectures. The macOS equivalent is the separately retained, versioned
[Docker Desktop checklist](smoke/macos-docker-desktop-v1.md).

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

Pushing the tag runs the following automated gates:

1. Validate SemVer, tag/workspace/dependency/lockfile agreement, and classify
   prereleases such as `v0.2.0-rc.1`.
2. Call `ci.yml` from the same commit. All quality, mutation, workspace coverage,
   native agent, and minimum/pinned Linux integration jobs must succeed. The
   aggregate gate rejects failed, cancelled, skipped, and missing jobs. Ordinary
   version-tag CI runs are suppressed to avoid duplicating this invocation.
3. Build both static Linux agents and all three native host archives. Run the
   existing reproducibility, package, and installed Linux smoke gates. CI and
   release agent artifact names are separate within the shared workflow run.
4. Require exactly the three expected packages, matching versions/build IDs,
   matching embedded-agent hashes, correct archive checksums, identical tagged
   installers, completed package evidence, the complete coverage/release-suite
   inventory, and successful mutation records for the exact commit.
5. Attach the assets below, generate release notes with the manual handoff, and
   create a draft. Prerelease versions are also marked as GitHub prereleases.

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

Choose one invocation; each creates a new private, short directory under
`/private/tmp`. It downloads the draft's manifest, exact macOS archive, adjacent
checksum, metadata, and checklist. It verifies their hashes and runtime build/
agent identity before using the binary. It prepares three local Git fixtures
(image, Dockerfile with local Feature, two-service Compose), a shell environment,
machine/dependency observations, a partially filled `smoke-record.md`, and
`MANUAL.md` with the remaining steps and scoped cleanup instructions. Authentication
comes from `gh`; no public access to draft assets is assumed.

With `--run-basics`, it requires a local Docker Desktop endpoint, runs `doctor`,
creates the fixtures, inspects status/list output, tests non-PTY and direct
OpenSSH access, checks source preservation through image down/up/rebuild/no-cache,
verifies the Feature installed, and checks repeated lock stability. Logs and
environments remain available even if a check fails. Each subprocess is bounded
to ten minutes; use Ctrl+C to interrupt and inspect the retained working directory.

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
The script never uploads evidence, publishes, modifies your normal install, or
automatically removes the retained Docker resources.

Draft releases are not available to the public installer. The default installation
command continues to use the latest published, non-prerelease release until a
maintainer publishes the new release. Mark release candidates as prereleases
before publishing them so they do not become the default installation.

`.github/workflows/release.yml` builds the two agents concurrently on matching
native x86_64 and ARM64 Linux runners. Each build executes and validates its
static agent without CPU emulation. An assembly job downloads both verified
artifacts, runs `cargo xtask finalize-agents` to validate their ELF identities and
create the shared manifest, and uploads the complete set. Each clean native host
job downloads that same manifest and pair of agents under `target/`, sets
`CDENV_AGENT_ARTIFACT_DIR`, then runs `cargo xtask dist`. Supplied agents must match
the current commit and manifest hashes and pass ELF validation again. The three
supported hosts are Linux x86_64, Linux aarch64, and macOS aarch64 (Apple Silicon).
Each job rebuilds the host to check identical archive checksums and uploads the
archive and its adjacent metadata. No Docker daemon is needed for macOS package
construction. Docker Desktop runtime behavior is recorded manually with the
versioned checklist because hosted macOS packaging is not an automated Docker
Desktop guarantee.
