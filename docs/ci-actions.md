# GitHub Actions runtime baseline

The CI and release workflows use GitHub-hosted `ubuntu-24.04`,
`ubuntu-24.04-arm`, and `macos-15` runners. Their JavaScript actions are kept on
reviewed Node.js 24 release lines:

| Action | Release line | Migration notes |
| --- | --- | --- |
| `actions/cache/restore`, `actions/cache/save` | `v5` | Node.js 24; restore/save build caches even when subsequent tests fail. |
| `actions/checkout` | `v5` | Node.js 24 migration release; checkout and credential behavior used here is unchanged. |
| `actions/upload-artifact` | `v6` | Node.js 24 runtime and updated artifact client; existing names, paths, missing-file errors, and retention inputs remain supported. |
| `actions/download-artifact` | `v7` | Node.js 24 runtime and updated artifact client; downloads by name and by pattern retain their existing layouts. |
| `docker/setup-buildx-action` | `v4` | Node.js 24 migration release; removed deprecated inputs are not used here. |
| `docker/setup-docker-action` | `v5` | Already declares Node.js 24. |
| `docker/setup-compose-action` | `v2` | Already declares Node.js 24. |
| `devcontainers/ci` | `v0.3` | Already declares Node.js 24. |

The migration releases require Actions Runner 2.327.1 or newer. This repository
uses GitHub-hosted runners rather than self-hosted runners, and the supported
x86_64, ARM64, and macOS images satisfy that requirement. All selected actions
publish architecture-independent JavaScript distributions. The Buildx setup is
still exercised natively on both Linux architectures.

Artifact upload `v6` and download `v7` use the same current artifact service and
are compatible with each other. The download path change introduced in `v5`
affects single downloads selected by artifact ID; these workflows select by name
or pattern, and preserve `merge-multiple` where assembly requires a flat target
directory. Upload failure behavior (`if-no-files-found: error`) and coverage
retention remain explicit. The release workflow exercises agent upload,
pattern-based assembly, named host downloads, host-package upload, and final
pattern-based release collection.

The reviewed action manifests and bundled dependencies no longer require the
runner's Node.js 20 compatibility override. The artifact runtime releases also
include the updated artifact client that removes the observed `punycode`,
`url.parse()`, and `Buffer()` deprecations. The actions above are JavaScript
rather than composite actions, so they do not hide additional `uses:` references;
the Dev Container and Docker setup actions were audited alongside the four
warning-producing actions. No known upstream-only runtime warning remains.

`.github/scripts/test-action-runtimes.py` inventories every `uses:` reference in
both workflow files. It fails when an unreviewed action or release line is added,
so any future action addition or major upgrade must review its declared runtime,
minimum runner, architecture support, and migration notes before updating the
baseline table and test.

Rust caches are separated by runner OS/architecture, job purpose, locked dependencies,
toolchain, and coverage configuration. Commit-specific keys restore compatible prior
builds; Cargo still validates fingerprints and the packaging gate validates embedded
agent identities. Caches contain build inputs/outputs, never release evidence or
verified-agent downloads. Release coverage suites share dependency artifacts but clean
workspace packages and profiles before each suite to keep reports independent.
Installed Linux smoke has a five-minute step deadline, bounded SSH calls and cleanup;
package jobs have a twenty-minute outer deadline.
