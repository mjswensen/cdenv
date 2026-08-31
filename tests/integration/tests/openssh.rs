//! Standard OpenSSH interoperability release contract.

#![cfg(feature = "openssh")]

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cdenv_agent::{EnvironmentCaptureRequest, EnvironmentProbe, capture_environment};
use tempfile::TempDir;

struct Fixture {
    temporary: TempDir,
    config: PathBuf,
    client_key: PathBuf,
    authorized_key: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("temporary fixture");
        let root = temporary.path();
        let workspace = root.join("workspace");
        let state = root.join("state");
        fs::create_dir(&workspace).expect("workspace");
        let host_key = root.join("host-key");
        let client_key = root.join("client-key");
        generate_key(&host_key);
        generate_key(&client_key);
        let authorized_key = root.join("authorized-key");
        fs::copy(client_key.with_extension("pub"), &authorized_key).expect("authorized key");
        private(&authorized_key);
        let environment = capture_environment(&EnvironmentCaptureRequest {
            generation: "openssh-gate".to_owned(),
            state_directory: state.display().to_string(),
            probe: EnvironmentProbe::None,
            remote_environment: BTreeMap::new(),
        })
        .expect("environment snapshot");
        let host_public = String::from_utf8(
            Command::new("ssh-keygen")
                .args(["-y", "-f"])
                .arg(&host_key)
                .output()
                .expect("read host public key")
                .stdout,
        )
        .expect("host public key text");
        let known_hosts = root.join("known-hosts");
        fs::write(&known_hosts, format!("openssh-gate {host_public}")).expect("known hosts");
        private(&known_hosts);
        let config = root.join("ssh-config");
        fs::write(
            &config,
            format!(
                "Host openssh-gate\n  HostName openssh-gate\n  User cdenv\n  IdentityFile {}\n  IdentitiesOnly yes\n  UserKnownHostsFile {}\n  StrictHostKeyChecking yes\n  ProxyCommand {} {} {} {} {}\n",
                client_key.display(),
                known_hosts.display(),
                env!("CARGO_BIN_EXE_openssh-fixture-server"),
                host_key.display(),
                authorized_key.display(),
                environment.snapshot_path,
                workspace.display(),
            ),
        )
        .expect("SSH config");
        Self {
            temporary,
            config,
            client_key,
            authorized_key,
        }
    }

    fn ssh(&self, arguments: &[&str]) -> Output {
        Command::new("ssh")
            .arg("-F")
            .arg(&self.config)
            .args(arguments)
            .output()
            .expect("start OpenSSH")
    }
}

fn private(path: &Path) {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("private mode");
}

fn generate_key(path: &Path) {
    let status = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", "", "-f"])
        .arg(path)
        .status()
        .expect("start ssh-keygen");
    assert!(status.success());
    private(path);
}

#[test]
fn standard_openssh_authenticates_and_preserves_exact_exec_streams_and_status() {
    let fixture = Fixture::new();
    let output = fixture.ssh(&[
        "openssh-gate",
        "printf 'out\\0'; printf stderr >&2; exit 23",
    ]);

    assert_eq!(output.stdout, b"out\0");
    assert!(!output.status.success());
    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stderr, b"stderr");
}

#[test]
fn standard_openssh_rejects_an_untrusted_host_and_an_invalid_client_key() {
    let fixture = Fixture::new();
    let unknown_host = fixture.ssh(&["-o", "HostKeyAlias=untrusted-host", "openssh-gate", "true"]);
    assert!(!unknown_host.status.success());
    assert!(!unknown_host.stderr.is_empty());

    let wrong_key = fixture.temporary.path().join("wrong-key");
    generate_key(&wrong_key);
    fs::copy(wrong_key.with_extension("pub"), &fixture.authorized_key)
        .expect("replace authorized key");
    private(&fixture.authorized_key);
    let rejected = fixture.ssh(&["openssh-gate", "true"]);
    assert!(!rejected.status.success());
}

#[test]
fn standard_openssh_reconnects_over_fresh_scoped_stdio_transports() {
    let fixture = Fixture::new();
    let first = fixture.ssh(&["openssh-gate", "printf first"]);
    let second = fixture.ssh(&["openssh-gate", "printf second"]);

    assert!(first.status.success());
    assert!(second.status.success());
    assert_eq!(first.stdout, b"first");
    assert_eq!(second.stdout, b"second");
}

#[test]
fn fixture_uses_a_scoped_stdio_server_not_a_network_ssh_daemon() {
    let fixture = Fixture::new();
    let config = fs::read_to_string(&fixture.config).expect("fixture config");
    assert!(config.contains("ProxyCommand"));
    assert!(!config.contains("Port 22"));
    assert!(!config.contains("sshd"));
    assert!(!config.contains("ListenAddress"));
    assert!(fixture.client_key.exists());
}
