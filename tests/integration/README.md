# Integration release suites

Run the editor-independent Dev Container profile gate with:

```text
cargo xtask test-integration --suite devcontainer-v1
```

Run the standard OpenSSH interoperability gate with:

```text
cargo xtask test-integration --suite openssh
```

The command requires Linux x86_64 or arm64, Docker Engine 29.6.2/API 1.55,
Docker CLI 29.7.1, Compose V2 5.3.1, and OpenSSH 10.0p2 or newer. CI also declares
`CDENV_INTEGRATION_ARCH`; an unsupported or mismatched runner fails. Missing or
below-baseline dependencies, unknown suites, missing required fixtures, zero
discovered tests, ignored tests, or a discrepancy between discovered and passed
counts fail the gate. The suite runs in release mode and does not require Node.js,
an editor, or the reference Dev Container CLI.

The gate runs the focused `cdenv-devcontainer` and `cdenv-cli` regression tests,
then the black-box tests in `tests/devcontainer_v1.rs`. Base-image pulls are
limited to the fixture-declared Debian and Alpine images. Feature network tests
use controlled fixture servers in focused component tests; normal fixture
parsing is offline.

`fixtures/devcontainer-v1-coverage.json` maps every Section 17.5 support-matrix
row and security promise to release-gate coverage. The editor-server simulator
is a repository-owned protocol fixture rather than an editor dependency.
