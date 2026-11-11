# GitHub Actions maintenance

The [CI workflow](../.github/workflows/ci.yml) and
[release workflow](../.github/workflows/release.yml) define runner images, action
versions, dependency baselines, caches, timeouts, and artifact retention. The
reviewed Node.js 24 action allowlist is enforced by
[`test-action-runtimes.py`](../.github/scripts/test-action-runtimes.py).

For an action addition or major upgrade, review its declared runtime, minimum
runner version, architecture support, and migration notes before updating the
allowlist. Check artifact upload/download compatibility and download directory
layout, including pattern downloads and `merge-multiple`: agent assembly and
release collection depend on those layouts. If adding self-hosted runners, verify
the actions' runner requirements explicitly.

Keep these boundaries when changing CI:

- Caches may hold build inputs/outputs, never release evidence or verified-agent
  downloads. Cargo fingerprints and package identity checks still apply after a
  cache restore.
- Coverage reruns must remain independent of ordinary release gates and of each
  other. See [coverage reporting](coverage.md) for interpretation and review.
- Version tags call CI from the release commit. Keep branch-push CI enabled when
  suppressing duplicate tag invocations; a tag-only filter suppresses branch
  pushes too.
- Missing, skipped, cancelled, or failed required jobs must prevent a review-ready
  draft. Publication remains a maintainer action after the
  [manual macOS handoff](release-packaging.md#manual-apple-silicon-handoff).

The [OpenSSH bootstrap regressions](../.github/scripts/test-openssh-bootstrap.py)
cover preinstalled dependencies, missing inputs, and failed setup without requiring
network access. Preserve checksum validation and bounded retries when changing
[the installer](../.github/scripts/install-openssh.sh).
