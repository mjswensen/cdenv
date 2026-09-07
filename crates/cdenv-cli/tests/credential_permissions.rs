#![cfg(unix)]
//! Network/Docker/keychain-free host permission and CLI boundary coverage.

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::Path;
use std::process::{Command, ExitCode};

use cdenv_cli::{
    CancellationToken, CdenvRoot, CliCommand, CommandLine, CreateWorkspaceRequest,
    CredentialCommandError, CredentialPermissionState, CredentialStatusReport, GitAdapter,
    Installation, LockBehavior, LockGuard, LockMode, ProcessEnvironment, create_workspace,
    credential_status, ensure_lock_file, mutate_credentials, render_credentials_application,
};
use cdenv_core::WorkspaceName;
use cdenv_core::credentials::{CredentialCapability, HttpsOrigin, SshAgentSelector};
use clap::Parser;
use serde_json::Value;

fn root(path: &Path) -> CdenvRoot {
    CdenvRoot::resolve(Some(path), &ProcessEnvironment).expect("absolute private root")
}

// The helper below borrows the actual parsed command, so tests do not need a
// test-only production API or a hand-built grammar substitute.
fn mutate(
    root: &CdenvRoot,
    arguments: &[&str],
) -> Result<CredentialStatusReport, CredentialCommandError> {
    let cli = CommandLine::try_parse_from(
        ["cdenv", "credentials"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .expect("credential command");
    let CliCommand::Credentials(arguments) = cli.command() else {
        panic!("credentials command");
    };
    mutate_credentials(root, &arguments.command)
}

fn project() -> WorkspaceName {
    WorkspaceName::parse("project").expect("workspace")
}

fn policy_bytes(root: &CdenvRoot) -> Vec<u8> {
    fs::read(root.credential_permission_file(&project())).expect("permission file")
}

fn fake_git(directory: &Path, fail: bool) -> GitAdapter {
    let path = directory.join(if fail { "fail-git" } else { "fake-git" });
    fs::write(&path, if fail {
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'git version 2.47.1\\n'; exit 0; fi\nexit 1\n"
    } else {
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'git version 2.47.1\\n'; exit 0; fi\nmkdir -p \"$4\"\nprintf 'preserved\\n' > \"$4/sentinel\"\n"
    }).expect("fake executable");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).expect("executable mode");
    GitAdapter::new(path)
}

fn clone_workspace(
    root: &CdenvRoot,
    git: &GitAdapter,
    explicit: bool,
) -> Result<cdenv_cli::CreatedWorkspace, cdenv_cli::CreateWorkspaceError> {
    let name = project();
    create_workspace(
        root,
        CreateWorkspaceRequest {
            source: "https://original.internal/team/project.git",
            name: explicit.then_some(&name),
            config: None,
        },
        git,
        &CancellationToken::default(),
    )
}

#[test]
fn status_on_an_absent_root_does_not_create_or_probe_anything() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("missing-root"));
    let status = credential_status(&root, &project());
    assert_eq!(status.permission(), CredentialPermissionState::Absent);
    assert!(!root.as_path().exists());
}

#[test]
fn staged_enable_is_independent_private_and_has_no_checkout_or_selected_socket() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    let status = mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://git.internal",
        ],
    )
    .expect("stage");
    assert_eq!(status.permission(), CredentialPermissionState::Staged);
    assert!(!status.grants().enabled(CredentialCapability::SshAgent));
    assert!(!status.grants().enabled(CredentialCapability::GitIdentity));
    assert!(!root.workspace(&project()).root().exists());
    let policy: Value = serde_json::from_slice(&policy_bytes(&root)).expect("policy");
    assert_eq!(
        policy["grants"]["gitHttps"],
        serde_json::json!(["https://git.internal:443"])
    );
    assert_eq!(
        fs::metadata(root.credential_permission_file(&project()))
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::metadata(root.credential_permissions_dir())
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let status = mutate(&root, &["enable", "project", "ssh-agent"]).expect("stage agent");
    assert_eq!(
        status.grants().ssh_selector(),
        Some(&SshAgentSelector::automatic())
    );
    assert!(
        !String::from_utf8(policy_bytes(&root))
            .expect("UTF-8")
            .contains("SSH_AUTH_SOCK")
    );
}

#[test]
fn multiple_and_repeated_host_flags_normalize_and_deduplicate() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    let status = mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://git.internal:443",
            "https://git.internal/",
            "https://[2001:db8::1]",
            "--host",
            "https://other.internal:8443",
        ],
    )
    .expect("multiple origins");
    assert_eq!(
        status
            .grants()
            .https_origins()
            .expect("HTTPS enabled")
            .len(),
        3
    );
}

#[test]
fn first_staged_https_enable_requires_explicit_origins() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    assert!(mutate(&root, &["enable", "project", "git-https"]).is_err());
    assert!(!root.credential_permission_file(&project()).exists());
}

#[test]
fn idempotent_enable_keeps_bytes_revision_and_explicit_agent_selector() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://git.internal",
        ],
    )
    .expect("stage");
    let before = policy_bytes(&root);
    mutate(&root, &["enable", "project", "git-https"]).expect("idempotent");
    assert_eq!(policy_bytes(&root), before);
    mutate(
        &root,
        &[
            "enable",
            "project",
            "ssh-agent",
            "--socket",
            "/tmp/explicit-agent",
        ],
    )
    .expect("stage selector");
    let before = policy_bytes(&root);
    mutate(&root, &["enable", "project", "ssh-agent"]).expect("preserve selector");
    assert_eq!(policy_bytes(&root), before);
}

#[test]
fn origin_adjustments_never_enable_and_selective_disable_discards_grants() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    assert!(
        mutate(
            &root,
            &["allow", "project", "git-https", "https://git.internal"]
        )
        .is_err()
    );
    mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://git.internal",
        ],
    )
    .expect("stage");
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage identity");
    mutate(
        &root,
        &[
            "allow",
            "project",
            "git-https",
            "https://other.internal:8443",
        ],
    )
    .expect("allow");
    let status = mutate(
        &root,
        &["deny", "project", "git-https", "https://git.internal/"],
    )
    .expect("deny canonical origin");
    assert_eq!(
        status
            .grants()
            .https_origins()
            .expect("HTTPS enabled")
            .len(),
        1
    );
    let status = mutate(&root, &["disable", "project", "git-https"]).expect("disable HTTPS");
    assert!(status.grants().enabled(CredentialCapability::GitIdentity));
    assert!(status.grants().https_origins().is_none());
    assert!(mutate(&root, &["enable", "project", "git-https"]).is_err());
    let status = mutate(&root, &["disable", "project"]).expect("disable all");
    assert!(status.grants().is_empty());
}

#[test]
fn explicit_successful_clone_consumes_staged_permission_before_later_work() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://approved.internal",
        ],
    )
    .expect("stage");
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone and bind");
    let status = credential_status(&root, &project());
    assert_eq!(status.permission(), CredentialPermissionState::Bound);
    assert!(
        !status
            .grants()
            .allows(&HttpsOrigin::parse("https://original.internal").expect("origin"))
    );
    let checkout = root.workspace(&project()).checkout();
    assert_eq!(
        fs::read(checkout.join("sentinel")).expect("checkout sentinel"),
        b"preserved\n"
    );
    assert!(!checkout.join("credential-binding.json").exists());
}

#[test]
fn failed_clone_leaves_staged_bytes_untouched_for_explicit_retry() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let before = policy_bytes(&root);
    assert!(clone_workspace(&root, &fake_git(temporary.path(), true), true).is_err());
    assert_eq!(policy_bytes(&root), before);
    assert!(!root.workspace(&project()).root().exists());
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("retry clone");
    assert_eq!(
        credential_status(&root, &project()).permission(),
        CredentialPermissionState::Bound
    );
}

#[test]
fn automatically_selected_name_cannot_consume_or_later_inherit_staged_permission() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let before = policy_bytes(&root);
    assert!(clone_workspace(&root, &fake_git(temporary.path(), false), false).is_err());
    assert_eq!(policy_bytes(&root), before);
    assert!(root.workspace(&project()).checkout().is_dir());
    assert!(matches!(
        mutate(&root, &["enable", "project", "ssh-agent"]),
        Err(CredentialCommandError::ExplicitNameRequired)
    ));
}

#[test]
fn disabled_workflow_does_not_create_permission_or_receipt_files() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    clone_workspace(&root, &fake_git(temporary.path(), false), false).expect("normal clone");
    assert!(!root.credential_permissions_dir().exists());
    assert!(
        !root
            .workspace(&project())
            .credential_binding_file()
            .exists()
    );
}

#[test]
fn first_bound_https_enable_uses_only_original_host_metadata() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    fs::write(
        root.workspace(&project())
            .checkout()
            .join("devcontainer.json"),
        b"{\"customizations\":{\"cdenv\":{\"credentials\":true}}}",
    )
    .expect("untrusted repository config");
    let status = mutate(&root, &["enable", "project", "git-https"]).expect("bound enable");
    assert_eq!(status.permission(), CredentialPermissionState::Bound);
    assert_eq!(
        status
            .grants()
            .https_origins()
            .expect("enabled")
            .iter()
            .next()
            .expect("one origin")
            .as_str(),
        "https://original.internal:443"
    );
}

#[test]
fn policy_copy_to_another_installation_cannot_transfer_grants() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let first = root(&temporary.path().join("first"));
    let second = root(&temporary.path().join("second"));
    mutate(&first, &["enable", "project", "git-identity"]).expect("first stage");
    mutate(&second, &["enable", "project", "ssh-agent"]).expect("second stage");
    fs::write(
        second.credential_permission_file(&project()),
        policy_bytes(&first),
    )
    .expect("copy policy");
    assert!(credential_status(&second, &project()).is_unavailable());
    assert!(matches!(
        mutate(&second, &["enable", "project", "git-identity"]),
        Err(CredentialCommandError::Scope)
    ));
}

#[test]
fn workspace_replacement_does_not_inherit_an_old_bound_grant() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    let git = fake_git(temporary.path(), false);
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    clone_workspace(&root, &git, true).expect("first workspace");
    fs::rename(
        root.workspace(&project()).root(),
        temporary.path().join("preserved-old-workspace"),
    )
    .expect("preserve old identity");
    assert!(clone_workspace(&root, &git, true).is_err());
    assert_eq!(
        credential_status(&root, &project()).permission(),
        CredentialPermissionState::Stale
    );
    mutate(&root, &["disable", "project"]).expect("discard old grants");
    let status = mutate(&root, &["enable", "project", "ssh-agent"]).expect("new explicit grant");
    assert!(!status.grants().enabled(CredentialCapability::GitIdentity));
    assert_eq!(status.permission(), CredentialPermissionState::Bound);
}

#[test]
fn receipt_survives_a_policy_write_interruption_without_becoming_a_wildcard() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let staged = policy_bytes(&root);
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    // Simulate a crash after receipt durability, before the bound policy rename.
    fs::write(root.credential_permission_file(&project()), staged).expect("restore staged policy");
    let before = policy_bytes(&root);
    assert_eq!(
        credential_status(&root, &project()).permission(),
        CredentialPermissionState::BindingPending
    );
    assert_eq!(policy_bytes(&root), before);
    let status =
        mutate(&root, &["enable", "project", "git-identity"]).expect("retry against exact receipt");
    assert_eq!(status.permission(), CredentialPermissionState::Bound);
}

#[test]
fn live_supervisor_lock_prevents_a_false_revocation_success_but_disk_is_revoked() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    let lifetime = root.workspace(&project()).supervisor_lifetime_lock();
    ensure_lock_file(&lifetime).expect("lifetime lock");
    let guard = LockGuard::acquire(&lifetime, LockMode::Exclusive, LockBehavior::FailFast)
        .expect("simulate owned supervisor");
    assert!(matches!(
        mutate(&root, &["disable", "project"]),
        Err(CredentialCommandError::RevocationUnconfirmed)
    ));
    assert!(credential_status(&root, &project()).grants().is_empty());
    assert!(LockGuard::acquire(&lifetime, LockMode::Exclusive, LockBehavior::FailFast).is_err());
    drop(guard);
    mutate(&root, &["disable", "project"]).expect("confirmed stopped retry");
}

#[test]
fn unknown_control_state_is_not_permission_to_signal_an_unverified_pid() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "ssh-agent"]).expect("stage");
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    let state = root.workspace(&project()).supervisor_state_file();
    let sentinel = format!("{{\"pid\":{},\"protocol\":999}}", std::process::id());
    fs::write(&state, &sentinel).expect("unknown control state");
    assert!(matches!(
        mutate(&root, &["disable", "project"]),
        Err(CredentialCommandError::RevocationUnconfirmed)
    ));
    assert_eq!(
        fs::read_to_string(state).expect("no signal or cleanup"),
        sentinel
    );
}

#[test]
fn status_and_mutation_reject_symlinks_hardlinks_and_unsafe_modes_without_repair() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let path = root.credential_permission_file(&project());
    let original = policy_bytes(&root);
    let target = temporary.path().join("outside");
    fs::write(&target, &original).expect("outside");
    fs::remove_file(&path).expect("remove fixture policy");
    symlink(&target, &path).expect("symlink fixture");
    assert!(credential_status(&root, &project()).is_unavailable());
    assert!(mutate(&root, &["enable", "project", "git-identity"]).is_err());
    assert_eq!(fs::read(&target).expect("outside unchanged"), original);
    fs::remove_file(&path).expect("remove symlink");
    fs::hard_link(&target, &path).expect("hard link");
    fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).expect("mode");
    assert!(credential_status(&root, &project()).is_unavailable());
    fs::remove_file(&path).expect("remove hard link");
    fs::write(&path, &original).expect("restore file");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("unsafe mode");
    assert!(mutate(&root, &["enable", "project", "git-identity"]).is_err());
    assert_eq!(
        fs::metadata(&path)
            .expect("mode unchanged")
            .permissions()
            .mode()
            & 0o777,
        0o644
    );
}

#[test]
fn unsupported_corrupt_and_oversized_policy_is_value_free_and_never_migrated() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let original: Value = serde_json::from_slice(&policy_bytes(&root)).expect("policy");
    let mut newer = original.clone();
    newer["schemaVersion"] = serde_json::json!(999);
    let mut unknown = original;
    unknown["grants"]["newCapability"] = serde_json::json!("SECRET-MARKER");
    for bytes in [
        serde_json::to_vec(&newer).expect("newer"),
        serde_json::to_vec(&unknown).expect("unknown"),
        b"SECRET-MARKER".to_vec(),
        vec![b'X'; 256 * 1024 + 1],
    ] {
        fs::write(root.credential_permission_file(&project()), &bytes).expect("invalid fixture");
        let report = credential_status(&root, &project());
        assert!(report.is_unavailable());
        assert!(
            !format!(
                "{report:?} {}",
                serde_json::to_string(&report).expect("safe JSON")
            )
            .contains("SECRET-MARKER")
        );
        assert_eq!(policy_bytes(&root), bytes);
    }
}

#[test]
fn status_json_is_one_document_with_no_repair_or_backend_lookup() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let before = policy_bytes(&root);
    let command =
        CommandLine::try_parse_from(["cdenv", "credentials", "status", "project", "--json"])
            .expect("status CLI");
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    assert_eq!(
        render_credentials_application(&command, &root, &mut stdout, &mut stderr),
        ExitCode::SUCCESS
    );
    let json: Value = serde_json::from_slice(&stdout).expect("one JSON document");
    assert_eq!(json["schemaVersion"], 1);
    assert!(stderr.is_empty());
    assert_eq!(policy_bytes(&root), before);
}

#[test]
fn grammar_rejects_unknown_capabilities_cross_capability_flags_and_raw_origin_echoes() {
    for arguments in [
        vec!["enable", "project"],
        vec!["enable", "project", "gpg-agent"],
        vec![
            "enable",
            "project",
            "git-identity",
            "--host",
            "https://git.internal",
        ],
        vec!["enable", "project", "git-https", "--socket", "auto"],
        vec!["allow", "project", "ssh-agent", "https://git.internal"],
        vec!["disable", "project", "new-capability"],
        vec!["enable", "../project", "git-identity"],
    ] {
        assert!(
            CommandLine::try_parse_from(["cdenv", "credentials"].into_iter().chain(arguments))
                .is_err()
        );
    }
    let error = CommandLine::try_parse_from([
        "cdenv",
        "credentials",
        "enable",
        "project",
        "git-https",
        "--host",
        "https://u:SECRET-MARKER@git.internal",
    ])
    .expect_err("reject userinfo");
    assert!(!error.to_string().contains("SECRET-MARKER"));
}

#[test]
fn active_generation_enable_saves_permission_but_fails_readiness_without_touching_runtime() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    let path = root.workspace(&project()).state_file();
    let mut state: Value =
        serde_json::from_slice(&fs::read(&path).expect("workspace state")).expect("state");
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/state-compose-background.json"))
            .expect("active fixture");
    state["active"] = fixture["active"].clone();
    let before = serde_json::to_vec(&state).expect("state");
    fs::write(&path, &before).expect("active state");

    assert!(matches!(
        mutate(&root, &["enable", "project", "git-identity"]),
        Err(CredentialCommandError::RuntimeUnavailable)
    ));
    assert!(
        credential_status(&root, &project())
            .grants()
            .enabled(CredentialCapability::GitIdentity)
    );
    assert_eq!(fs::read(&path).expect("unchanged runtime state"), before);
    assert!(!root.workspace(&project()).supervisor_socket().exists());
    assert_eq!(
        fs::read(root.workspace(&project()).checkout().join("sentinel")).expect("checkout"),
        b"preserved\n"
    );
}

#[test]
fn copying_even_installation_metadata_cannot_retarget_staged_permission_to_another_root() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let first = root(&temporary.path().join("first"));
    let second = root(&temporary.path().join("second"));
    mutate(&first, &["enable", "project", "git-identity"]).expect("first stage");
    mutate(&second, &["enable", "project", "git-identity"]).expect("second stage");
    fs::copy(first.installation_file(), second.installation_file()).expect("copy installation");
    fs::copy(
        first.credential_permission_file(&project()),
        second.credential_permission_file(&project()),
    )
    .expect("copy policy");
    assert!(credential_status(&second, &project()).is_unavailable());
    assert!(matches!(
        mutate(&second, &["enable", "project", "ssh-agent"]),
        Err(CredentialCommandError::Scope)
    ));
}

#[test]
fn revisions_never_wrap_or_allow_old_authority_to_be_reused() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    let mut policy: Value = serde_json::from_slice(&policy_bytes(&root)).expect("policy");
    policy["revision"] = serde_json::json!(u64::MAX);
    let before = serde_json::to_vec(&policy).expect("exhausted revision");
    fs::write(root.credential_permission_file(&project()), &before).expect("fixture");
    mutate(&root, &["enable", "project", "git-identity"])
        .expect("unchanged grant remains idempotent");
    assert!(matches!(
        mutate(&root, &["disable", "project"]),
        Err(CredentialCommandError::Revision)
    ));
    assert_eq!(policy_bytes(&root), before);
}

#[test]
fn list_and_detailed_status_share_the_dedicated_permission_facts() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(&root, &["enable", "project", "git-identity"]).expect("stage");
    clone_workspace(&root, &fake_git(temporary.path(), false), true).expect("clone");
    let entries = cdenv_cli::enumerate_workspaces(&root).expect("enumeration");
    let docker = cdenv_cli::DockerSnapshot::Available(Vec::new());
    let list = cdenv_cli::correlate_workspace_reports(&root, &entries, &docker);
    let status = cdenv_cli::requested_workspace_status(&root, &entries, &docker, &project())
        .expect("status");
    let dedicated =
        serde_json::to_value(credential_status(&root, &project())).expect("dedicated JSON");
    assert_eq!(
        serde_json::to_value(list).expect("list JSON")["workspaces"][0]["credentials"],
        dedicated
    );
    assert_eq!(
        serde_json::to_value(status).expect("status JSON")["credentials"],
        dedicated
    );
}

#[test]
fn doctor_inspects_staged_permission_without_invoking_a_credential_helper() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let root = root(&temporary.path().join("root"));
    mutate(
        &root,
        &[
            "enable",
            "project",
            "git-https",
            "--host",
            "https://git.internal",
        ],
    )
    .expect("stage");
    let tools = temporary.path().join("tools");
    fs::create_dir(&tools).expect("fake tool directory");
    for tool in ["git", "docker", "ssh"] {
        let path = tools.join(tool);
        fs::write(&path, "#!/bin/sh\ncase \"$1\" in --version|-V|version|compose) exit 0;; *) printf 'FORBIDDEN-CREDENTIAL-LOOKUP' > \"$CDENV_TEST_FORBIDDEN\"; exit 91;; esac\n").expect("fake version check");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("executable");
    }
    let before = policy_bytes(&root);
    let output = Command::new(env!("CARGO_BIN_EXE_cdenv"))
        .env("PATH", &tools)
        .env(
            "CDENV_TEST_FORBIDDEN",
            temporary.path().join("forbidden-operation"),
        )
        .args(["--root"])
        .arg(root.as_path())
        .args(["doctor", "--json"])
        .output()
        .expect("doctor");
    let json: Value = serde_json::from_slice(&output.stdout).expect("one JSON report");
    let text = json.to_string();
    assert!(text.contains("staged; inactive"));
    assert!(text.contains("backend uninspected"));
    assert!(!text.contains("FORBIDDEN-CREDENTIAL-LOOKUP"));
    assert!(!temporary.path().join("forbidden-operation").exists());
    assert!(output.stderr.is_empty());
    assert_eq!(policy_bytes(&root), before);
}

#[test]
fn credential_commands_honor_root_selection_in_the_installed_binary_boundary() {
    let temporary = tempfile::tempdir().expect("tempdir");
    let explicit = temporary.path().join("explicit");
    let ignored = temporary.path().join("ignored");
    let output = Command::new(env!("CARGO_BIN_EXE_cdenv"))
        .env("CDENV_HOME", &ignored)
        .args(["credentials", "enable", "project", "git-identity", "--root"])
        .arg(&explicit)
        .output()
        .expect("CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!ignored.exists());
    assert!(Installation::load_record_read_only(&root(&explicit)).is_ok());
}
