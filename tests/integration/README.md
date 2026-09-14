# Integration release suites

Run the editor-independent Dev Container profile gate with:

```text
cargo xtask test-integration --suite devcontainer-v1
```

Run the packaged credential-product and standard OpenSSH interoperability gates with:

```text
CDENV_CREDENTIAL_TEST_BINARY=/path/to/packaged/cdenv cargo xtask test-integration --suite credentials
cargo xtask test-integration --suite openssh
```

The credential binary must contain both checksum-verified static agents and pass
`__validate-artifacts`; a development binary is rejected. The commands require
Linux x86_64 or arm64, Docker Engine 29.6.2/API 1.55, Docker CLI 29.7.1,
Compose V2 5.3.1, and OpenSSH 10.0p2 or newer. CI also declares
`CDENV_INTEGRATION_ARCH`; an unsupported or mismatched runner fails. Missing or
below-baseline dependencies, unknown suites, missing required fixtures, zero
discovered tests, ignored tests, or a discrepancy between discovered and passed
counts fail the gate. Native Linux CI uses its ordinary temporary directory. A
nested development container may set `CDENV_INTEGRATION_SHARED_TMP` only to an
absolute directory that exists at the same path for both that container and the
Docker daemon; absence of such a bind is a failed local prerequisite, not a skip. The suite runs in release mode and does not require Node.js,
an editor, or the reference Dev Container CLI.

The gate runs the focused `cdenv-devcontainer` and `cdenv-cli` regression tests,
then the black-box tests in `tests/devcontainer_v1.rs`. The named credential
suite drives staged enable/create/SSH/down/up/rebuild/revocation through the
packaged executable and rejects an empty or partially skipped run. Its
`fixtures/credential-coverage.json` record distinguishes integrated evidence,
component evidence, and requirements still blocked on authenticated TLS/macOS
observations. Base-image pulls are
limited to the fixture-declared Debian and Alpine images. Feature network tests
use controlled fixture servers in focused component tests; normal fixture
parsing is offline.

`fixtures/devcontainer-v1-coverage.json` maps every Section 17.5 support-matrix
row and security promise to release-gate coverage. The editor-server simulator
is a repository-owned protocol fixture rather than an editor dependency.
