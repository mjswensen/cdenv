//! SSH identity, explicit configuration, and argument contract tests.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::process::Command;

use cdenv_cli::{
    CdenvRoot, ProcessEnvironment, ensure_workspace_ssh_identity, load_workspace_ssh_assets,
    regenerate_managed_ssh, render_managed_config, resolve_invoked_executable,
    system_ssh_arguments,
};
use cdenv_core::WorkspaceName;
use ssh_key::{Algorithm, PrivateKey, PublicKey};

fn root_at(path: &Path) -> CdenvRoot {
    CdenvRoot::resolve(Some(path), &ProcessEnvironment).expect("absolute test root should resolve")
}

#[test]
fn generated_roles_are_distinct_matching_ed25519_identities() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let name: WorkspaceName = "project".parse().expect("workspace name");

    ensure_workspace_ssh_identity(&root, &name).expect("identities should be generated");
    let client_private = PrivateKey::read_openssh_file(root.ssh().private_key())
        .expect("client private key should parse");
    let client_public = PublicKey::read_openssh_file(root.ssh().public_key())
        .expect("client public key should parse");
    let host_private = PrivateKey::read_openssh_file(root.ssh().host_private_key(&name))
        .expect("host private key should parse");
    let host_public = PublicKey::read_openssh_file(root.ssh().host_public_key(&name))
        .expect("host public key should parse");

    assert_eq!(client_private.algorithm(), Algorithm::Ed25519);
    assert_eq!(host_private.algorithm(), Algorithm::Ed25519);
    assert_eq!(
        client_private.public_key().key_data(),
        client_public.key_data()
    );
    assert_eq!(host_private.public_key().key_data(), host_public.key_data());
    assert_ne!(client_public.key_data(), host_public.key_data());
}

#[test]
fn workspace_host_key_remains_stable_across_repeated_setup() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let name: WorkspaceName = "project".parse().expect("workspace name");

    ensure_workspace_ssh_identity(&root, &name).expect("first setup");
    let before = fs::read(root.ssh().host_private_key(&name)).expect("host key");
    ensure_workspace_ssh_identity(&root, &name).expect("rebuild setup");
    let after = fs::read(root.ssh().host_private_key(&name)).expect("host key");

    assert_eq!(after, before);
}

#[cfg(unix)]
#[test]
fn generated_identity_and_configuration_modes_are_strict() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let name: WorkspaceName = "project".parse().expect("workspace name");
    regenerate_managed_ssh(&root, Path::new("/usr/bin/cdenv"), [name.clone()]).expect("SSH setup");
    let modes = [
        root.ssh().root(),
        root.ssh().private_key(),
        root.ssh().public_key(),
        root.ssh().host_private_key(&name),
        root.ssh().host_public_key(&name),
        root.ssh().config(),
        root.ssh().known_hosts(),
    ]
    .map(|path| fs::metadata(path).expect("metadata").permissions().mode() & 0o777);

    assert_eq!(modes, [0o700, 0o600, 0o644, 0o600, 0o644, 0o600, 0o600]);
}

#[test]
fn mismatched_managed_public_key_is_rejected() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let project: WorkspaceName = "project".parse().expect("workspace name");
    let other: WorkspaceName = "other".parse().expect("workspace name");
    ensure_workspace_ssh_identity(&root, &project).expect("project identity");
    ensure_workspace_ssh_identity(&root, &other).expect("other identity");
    fs::copy(
        root.ssh().host_public_key(&other),
        root.ssh().host_public_key(&project),
    )
    .expect("replace public key");

    let error = ensure_workspace_ssh_identity(&root, &project)
        .expect_err("mismatched public key must fail");

    assert!(error.to_string().contains("does not match"));
}

#[test]
fn provision_assets_contain_host_private_and_exact_client_public_roles() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let name: WorkspaceName = "project".parse().expect("workspace name");
    ensure_workspace_ssh_identity(&root, &name).expect("identity setup");

    let assets = load_workspace_ssh_assets(&root, &name).expect("provision assets");
    let host = PrivateKey::from_openssh(
        std::str::from_utf8(assets.host_private_key()).expect("host key UTF-8"),
    )
    .expect("host private key");
    let authorized = PublicKey::from_openssh(
        std::str::from_utf8(assets.authorized_client_key())
            .expect("authorized key UTF-8")
            .trim_end(),
    )
    .expect("authorized public key");
    let client = PublicKey::read_openssh_file(root.ssh().public_key()).expect("client public key");
    let provision = assets.provision_assets(
        "/home/project/.cdenv/host_key",
        "/home/project/.cdenv/authorized_keys",
    );

    assert_ne!(host.public_key().key_data(), authorized.key_data());
    assert_eq!(authorized.key_data(), client.key_data());
    assert_eq!(provision.map(|asset| asset.mode), [0o600, 0o600]);
}

#[test]
fn generated_config_contains_only_explicit_workspace_blocks() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let alpha: WorkspaceName = "alpha".parse().expect("workspace name");
    let beta: WorkspaceName = "beta".parse().expect("workspace name");

    let config = render_managed_config(&root, Path::new("/stable/cdenv"), &[alpha, beta])
        .expect("rendered config");

    assert_eq!(config.matches("Host alpha.cdenv").count(), 1);
    assert_eq!(config.matches("Host beta.cdenv").count(), 1);
    assert!(!config.contains("Host *.cdenv"));
    assert!(!config.contains("%h"));
}

#[cfg(unix)]
#[test]
fn openssh_resolves_supported_spaces_and_quotes_in_explicit_config() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let root_path = temporary.path().join("root with 'quote");
    let root = root_at(&root_path);
    let executable = temporary.path().join("bin with 'quote/cdenv");
    fs::create_dir_all(executable.parent().expect("executable parent")).expect("create bin");
    fs::write(&executable, b"#!/bin/sh\nexit 0\n").expect("fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).expect("executable mode");
    let name: WorkspaceName = "project".parse().expect("workspace name");
    regenerate_managed_ssh(&root, &executable, [name]).expect("SSH setup");

    let output = Command::new("ssh")
        .args(["-G", "-F"])
        .arg(root.ssh().config())
        .arg("project.cdenv")
        .output()
        .expect("system OpenSSH should run");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let resolved = String::from_utf8(output.stdout).expect("ssh -G UTF-8");

    assert!(resolved.contains("hostname project.cdenv\n"));
    assert!(resolved.contains("user cdenv\n"));
    assert!(resolved.contains("stricthostkeychecking true\n"));
    assert!(resolved.contains(&format!(
        "identityfile {}\n",
        root.ssh().private_key().display()
    )));
    let executable_quote = format!(
        "'{}'",
        executable
            .to_str()
            .expect("UTF-8 executable")
            .replace('\'', "'\\''")
    );
    let root_quote = format!(
        "'{}'",
        root.as_path()
            .to_str()
            .expect("UTF-8 root")
            .replace('\'', "'\\''")
    );
    assert!(resolved.contains(&format!(
        "proxycommand {executable_quote} --root {root_quote} proxy project\n"
    )));
}

#[cfg(unix)]
#[test]
fn argv0_path_lookup_retains_the_final_stable_symlink() {
    use std::os::unix::fs::{PermissionsExt, symlink};

    let temporary = tempfile::tempdir().expect("temporary directory");
    let bin = temporary.path().join("bin");
    fs::create_dir(&bin).expect("bin directory");
    let versioned = bin.join("cdenv-0.1");
    fs::write(&versioned, b"#!/bin/sh\n").expect("versioned executable");
    fs::set_permissions(&versioned, fs::Permissions::from_mode(0o755)).expect("mode");
    let stable = bin.join("cdenv");
    symlink(&versioned, &stable).expect("stable symlink");

    let resolved =
        resolve_invoked_executable(OsStr::new("cdenv"), Some(bin.as_os_str()), temporary.path());

    assert_eq!(resolved.as_deref(), Some(stable.as_path()));
}

#[test]
fn ssh_wrapper_arguments_preserve_remote_argv_boundaries() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let name: WorkspaceName = "project".parse().expect("workspace name");
    let remote = vec![OsString::from("printf '%s'"), OsString::from("a b")];

    let arguments = system_ssh_arguments(&root, &name, &remote);

    assert_eq!(
        arguments,
        vec![
            OsString::from("-F"),
            root.ssh().config().into_os_string(),
            OsString::from("project.cdenv"),
            OsString::from("printf '%s'"),
            OsString::from("a b"),
        ]
    );
}
