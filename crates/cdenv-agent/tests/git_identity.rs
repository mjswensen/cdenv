//! Controlled Git fixtures for fill-only-missing identity defaults.

#![cfg(target_os = "linux")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cdenv_agent::ManagedGitIdentityIntegration;
use cdenv_core::git_identity::GitIdentityMetadata;

fn run(
    integration: &ManagedGitIdentityIntegration,
    home: &Path,
    repository: &Path,
    global_arguments: &[&str],
    command_arguments: &[&str],
) -> Output {
    let git = which_git();
    let mut arguments = vec![
        "git-with-identity".to_owned(),
        integration.metadata_path().display().to_string(),
        git.display().to_string(),
        "--".to_owned(),
    ];
    arguments.extend(global_arguments.iter().map(ToString::to_string));
    arguments.extend(command_arguments.iter().map(ToString::to_string));
    Command::new(env!("CARGO_BIN_EXE_cdenv-agent"))
        .args(arguments)
        .env("HOME", home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("GIT_CONFIG_COUNT")
        .current_dir(repository)
        .output()
        .expect("identity wrapper")
}

fn which_git() -> PathBuf {
    let output = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("find git");
    PathBuf::from(String::from_utf8(output.stdout).expect("utf8").trim())
}

fn fixture() -> (
    tempfile::TempDir,
    PathBuf,
    PathBuf,
    ManagedGitIdentityIntegration,
) {
    let temporary = tempfile::tempdir().expect("temporary");
    let home = temporary.path().join("home");
    let repository = temporary.path().join("repository");
    fs::create_dir(&home).expect("home");
    fs::create_dir(&repository).expect("repository");
    Command::new(which_git())
        .args(["init", "-q"])
        .current_dir(&repository)
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("git init");
    let metadata = GitIdentityMetadata::new(
        Some("Host 'Name' $(not-executed)".to_owned()),
        Some("host+quoted@example.test".to_owned()),
    )
    .expect("metadata");
    let integration =
        ManagedGitIdentityIntegration::refresh(&temporary.path().join("integration"), &metadata)
            .expect("integration");
    (temporary, home, repository, integration)
}

#[test]
fn identity_defaults_fill_both_missing_fields_as_literal_data() {
    let (temporary, home, repository, integration) = fixture();
    let output = run(
        &integration,
        &home,
        &repository,
        &[],
        &["config", "--get", "user.name"],
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8(output.stdout).expect("utf8").trim(),
        "Host 'Name' $(not-executed)"
    );
    assert!(!temporary.path().join("not-executed").exists());
}

#[test]
fn local_identity_wins_while_the_other_field_is_filled_independently() {
    let (_temporary, home, repository, integration) = fixture();
    Command::new(which_git())
        .args(["config", "user.name", "Repository Name"])
        .current_dir(&repository)
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("local name");
    let name = run(
        &integration,
        &home,
        &repository,
        &[],
        &["config", "--get", "user.name"],
    );
    let email = run(
        &integration,
        &home,
        &repository,
        &[],
        &["config", "--get", "user.email"],
    );
    assert_eq!(
        String::from_utf8(name.stdout).expect("utf8").trim(),
        "Repository Name"
    );
    assert_eq!(
        String::from_utf8(email.stdout).expect("utf8").trim(),
        "host+quoted@example.test"
    );
}

#[test]
fn command_and_command_environment_overrides_keep_their_precedence() {
    let (_temporary, home, repository, integration) = fixture();
    let direct = run(
        &integration,
        &home,
        &repository,
        &["-c", "user.name=Command Name"],
        &["config", "--get", "user.name"],
    );
    let configured = Command::new(env!("CARGO_BIN_EXE_cdenv-agent"))
        .args([
            "git-with-identity",
            integration.metadata_path().to_str().expect("path"),
            which_git().to_str().expect("git"),
            "--",
            "config",
            "--get",
            "user.email",
        ])
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "user.email")
        .env("GIT_CONFIG_VALUE_0", "Environment Email <env@example.test>")
        .current_dir(&repository)
        .output()
        .expect("configured invocation");
    assert_eq!(
        String::from_utf8(direct.stdout).expect("utf8").trim(),
        "Command Name"
    );
    assert_eq!(
        String::from_utf8(configured.stdout).expect("utf8").trim(),
        "Environment Email <env@example.test>"
    );
}

#[test]
fn conditional_identity_and_signing_configuration_are_preserved() {
    let (temporary, home, repository, integration) = fixture();
    let included = temporary.path().join("conditional.gitconfig");
    fs::write(
        &included,
        "[user]\n\tname = Conditional Name\n\temail = conditional@example.test\n\tsigningKey = CONDITIONAL-KEY\n[commit]\n\tgpgsign = true\n",
    )
    .expect("included config");
    fs::write(
        home.join(".gitconfig"),
        format!(
            "[includeIf \"gitdir:{}/.git\"]\n\tpath = {}\n",
            repository.display(),
            included.display()
        ),
    )
    .expect("global config");
    for (key, expected) in [
        ("user.name", "Conditional Name"),
        ("user.email", "conditional@example.test"),
        ("user.signingKey", "CONDITIONAL-KEY"),
        ("commit.gpgsign", "true"),
    ] {
        let output = run(
            &integration,
            &home,
            &repository,
            &[],
            &["config", "--get", key],
        );
        assert_eq!(
            String::from_utf8(output.stdout).expect("utf8").trim(),
            expected
        );
    }
}

#[test]
fn explicit_author_and_committer_environment_values_remain_effective() {
    let (_temporary, home, repository, integration) = fixture();
    let output = Command::new(env!("CARGO_BIN_EXE_cdenv-agent"))
        .args([
            "git-with-identity",
            integration.metadata_path().to_str().expect("path"),
            which_git().to_str().expect("git"),
            "--",
            "var",
            "GIT_AUTHOR_IDENT",
        ])
        .env("HOME", &home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "Explicit Author")
        .env("GIT_AUTHOR_EMAIL", "explicit-author@example.test")
        .env("GIT_AUTHOR_DATE", "2000-01-01T00:00:00Z")
        .env("GIT_COMMITTER_NAME", "Explicit Committer")
        .env("GIT_COMMITTER_EMAIL", "explicit-committer@example.test")
        .current_dir(repository)
        .output()
        .expect("author identity");
    let identity = String::from_utf8(output.stdout).expect("utf8");
    assert!(identity.starts_with("Explicit Author <explicit-author@example.test>"));
}

#[test]
fn disable_removes_only_owned_metadata_and_restores_underlying_behavior() {
    let (temporary, home, repository, integration) = fixture();
    let global = home.join(".gitconfig");
    fs::write(&global, b"[user]\n\tname = Existing\n").expect("global");
    let before = fs::read(&global).expect("before");
    integration.remove().expect("remove");
    assert_eq!(fs::read(&global).expect("after"), before);
    assert!(!integration.metadata_path().exists());
    assert_eq!(
        fs::metadata(temporary.path().join("integration"))
            .expect("directory")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let output = Command::new(which_git())
        .args(["config", "--get", "user.email"])
        .env("HOME", home)
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .current_dir(repository)
        .output()
        .expect("underlying git");
    assert!(!output.status.success());
}
