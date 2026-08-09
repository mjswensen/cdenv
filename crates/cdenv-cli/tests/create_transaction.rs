#![cfg(unix)]
//! Local and fake-Git coverage for the durable create transaction.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use cdenv_cli::{
    CancellationToken, CdenvRoot, ConfigContainmentError, CreateWorkspaceError,
    CreateWorkspaceRequest, GitAdapter, RepoRelativeConfigPath, RootEnvironment, create_workspace,
    load_workspace_state, validate_explicit_config,
};
use cdenv_core::{ForegroundOperation, WorkspaceName};

struct NoEnvironment;

impl RootEnvironment for NoEnvironment {
    fn cdenv_home(&self) -> Option<OsString> {
        None
    }

    fn home_dir(&self) -> Option<PathBuf> {
        None
    }
}

fn test_root(path: &Path) -> CdenvRoot {
    CdenvRoot::resolve(Some(path), &NoEnvironment).expect("test root should validate")
}

fn run_git(directory: &Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(directory)
        .status()
        .expect("test Git should start");
    assert!(status.success(), "test Git command should succeed");
}

fn normal_repository(parent: &Path) -> PathBuf {
    let repository = parent.join("source-project");
    fs::create_dir(&repository).expect("repository directory should exist");
    run_git(&repository, &["init", "--quiet"]);
    run_git(&repository, &["config", "user.name", "cdenv tests"]);
    run_git(
        &repository,
        &["config", "user.email", "cdenv@example.invalid"],
    );
    fs::create_dir(repository.join(".devcontainer")).expect("configuration directory should exist");
    fs::write(repository.join(".devcontainer/devcontainer.json"), b"{}\n")
        .expect("configuration should exist");
    run_git(&repository, &["add", "."]);
    run_git(&repository, &["commit", "--quiet", "-m", "fixture"]);
    repository
}

fn fake_git(parent: &Path, clone_exit: i32) -> (PathBuf, PathBuf) {
    let executable = parent.join("fake-git");
    let record = parent.join("arguments.txt");
    let script = format!(
        "#!/bin/sh\n\
         printf 'BEGIN\\n' >> '{}'\n\
         for arg in \"$@\"; do printf 'ARG=%s\\n' \"$arg\" >> '{}'; done\n\
         if [ \"$1\" = --version ]; then printf 'git version 2.47.1\\n'; exit 0; fi\n\
         printf '%s\\n' \"$3\" >&2\n\
         if [ {clone_exit} -ne 0 ]; then exit {clone_exit}; fi\n\
         mkdir -p \"$4\"\n",
        record.display(),
        record.display(),
    );
    fs::write(&executable, script).expect("fake Git should be written");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .expect("fake Git should be executable");
    (executable, record)
}

#[test]
fn local_normal_repository_clones_to_named_checkout_with_sanitized_idle_state() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let source = normal_repository(temporary.path());
    let root = test_root(&temporary.path().join("cdenv"));
    let source_text = source.to_str().expect("fixture path should be UTF-8");

    let created = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: source_text,
            name: None,
            config: None,
        },
        &GitAdapter::system(),
        &CancellationToken::default(),
    )
    .expect("local repository should clone");
    let loaded = load_workspace_state(&root.workspace(created.name()).state_file())
        .expect("created state should load");
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(created.checkout())
        .output()
        .expect("cloned Git state should be inspectable");

    assert_eq!(
        (
            created.name().as_str(),
            created.checkout(),
            loaded.state().repository_source().as_str(),
            loaded.state().operation().kind(),
            loaded.state().active(),
            status.stdout,
        ),
        (
            "source-project",
            root.workspace(created.name()).checkout().as_path(),
            fs::canonicalize(source)
                .expect("source should canonicalize")
                .to_str()
                .expect("source should be UTF-8"),
            ForegroundOperation::Idle,
            None,
            Vec::new(),
        )
    );
}

#[test]
fn local_bare_repository_clones_without_network_access() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let source = normal_repository(temporary.path());
    let bare = temporary.path().join("bare-project.git");
    let status = Command::new("git")
        .args(["clone", "--quiet", "--bare"])
        .arg(&source)
        .arg(&bare)
        .status()
        .expect("bare clone should start");
    assert!(status.success(), "bare fixture should be created");
    let root = test_root(&temporary.path().join("cdenv"));

    let created = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: bare.to_str().expect("fixture path should be UTF-8"),
            name: None,
            config: None,
        },
        &GitAdapter::system(),
        &CancellationToken::default(),
    )
    .expect("bare repository should clone");

    assert_eq!(
        (created.name().as_str(), created.checkout().is_dir()),
        ("bare-project", true)
    );
}

#[test]
fn missing_explicit_config_retains_checkout_state_and_sanitized_error() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let source = normal_repository(temporary.path());
    let root = test_root(&temporary.path().join("cdenv"));
    let config =
        RepoRelativeConfigPath::parse("missing.json").expect("fixture selection should be lexical");

    let error = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: source.to_str().expect("fixture path should be UTF-8"),
            name: None,
            config: Some(&config),
        },
        &GitAdapter::system(),
        &CancellationToken::default(),
    )
    .expect_err("missing explicit config should fail");
    let name = WorkspaceName::parse("source-project").expect("fixture name should validate");
    let paths = root.workspace(&name);
    let state = load_workspace_state(&paths.state_file()).expect("recoverable state should remain");

    assert!(matches!(error, CreateWorkspaceError::Config(_)));
    assert_eq!(
        (
            paths.checkout().is_dir(),
            state.state().operation().kind(),
            state.state().last_error().is_some(),
        ),
        (true, ForegroundOperation::Creating, true)
    );
}

#[test]
fn clone_failure_removes_workspace_and_retains_redacted_global_log() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let source = temporary.path().join("not-a-repository-secret");
    fs::create_dir(&source).expect("non-repository source should exist");
    let root = test_root(&temporary.path().join("cdenv"));
    let name = WorkspaceName::parse("failed").expect("fixture name should validate");
    let source_text = source.to_str().expect("fixture path should be UTF-8");

    let error = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: source_text,
            name: Some(&name),
            config: None,
        },
        &GitAdapter::system(),
        &CancellationToken::default(),
    )
    .expect_err("non-repository clone should fail");
    let logs = fs::read_dir(root.logs_dir())
        .expect("global logs should remain")
        .map(|entry| entry.expect("log entry should read").path())
        .collect::<Vec<_>>();
    let bytes = fs::read(&logs[0]).expect("operation log should read");

    assert!(matches!(error, CreateWorkspaceError::Git(_)));
    assert_eq!(
        (root.workspace(&name).root().exists(), logs.len()),
        (false, 1)
    );
    assert!(
        !bytes
            .windows(source_text.len())
            .any(|part| part == source_text.as_bytes())
    );
}

#[test]
fn fake_git_exit_status_propagates_without_retaining_workspace() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let (executable, _) = fake_git(temporary.path(), 23);
    let root = test_root(&temporary.path().join("cdenv"));
    let name = WorkspaceName::parse("failed-fake").expect("fixture name should validate");

    let error = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: "https://user:secret@example.invalid/repo.git",
            name: Some(&name),
            config: None,
        },
        &GitAdapter::new(executable),
        &CancellationToken::default(),
    )
    .expect_err("fake Git exit should fail create");

    assert!(matches!(
        error,
        CreateWorkspaceError::Git(cdenv_cli::GitError::Exited { code: Some(23), .. })
    ));
    assert!(!root.workspace(&name).root().exists());
}

#[test]
fn fake_git_receives_source_after_delimiter_without_shell_interpretation() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let (executable, record) = fake_git(temporary.path(), 0);
    let root = test_root(&temporary.path().join("cdenv"));
    let name = WorkspaceName::parse("safe-name").expect("fixture name should validate");
    let sentinel = temporary.path().join("shell-was-used");
    let source = format!("host:repo;touch${{IFS}}{}", sentinel.display());

    let created = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: &source,
            name: Some(&name),
            config: None,
        },
        &GitAdapter::new(executable),
        &CancellationToken::default(),
    )
    .expect("fake clone should complete");
    let arguments = fs::read_to_string(record).expect("fake arguments should read");

    assert_eq!(
        arguments,
        format!(
            "BEGIN\nARG=--version\nBEGIN\nARG=clone\nARG=--\nARG={source}\nARG={}\n",
            created.checkout().display()
        )
    );
    assert!(!sentinel.exists());
}

#[test]
fn fake_git_output_and_state_never_retain_raw_http_credentials() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let (executable, _) = fake_git(temporary.path(), 0);
    let root = test_root(&temporary.path().join("cdenv"));
    let name = WorkspaceName::parse("redacted").expect("fixture name should validate");
    let source = "https://user:secret@example.invalid/repo.git?token=x#fragment";

    let created = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source,
            name: Some(&name),
            config: None,
        },
        &GitAdapter::new(executable),
        &CancellationToken::default(),
    )
    .expect("fake clone should complete");
    let state = load_workspace_state(&root.workspace(&name).state_file())
        .expect("fake clone state should load");
    let log = fs::read(created.operation_log()).expect("operation log should read");
    let mode = fs::metadata(created.operation_log())
        .expect("operation log metadata should read")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(
        (
            state.state().repository_source().as_str(),
            mode,
            log.len() <= 1024 * 1024,
        ),
        ("https://example.invalid/repo.git", 0o600, true)
    );
    assert!(!String::from_utf8_lossy(&log).contains("secret"));
    assert!(
        !log.windows(source.len())
            .any(|part| part == source.as_bytes())
    );
}

#[test]
fn cancellation_during_clone_terminates_process_group_and_removes_workspace() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let executable = temporary.path().join("cancellable-git");
    let marker = temporary.path().join("clone-started");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then printf 'git version 2.47.1\\n'; exit 0; fi\n\
         : > '{}'\n\
         sleep 30\n",
        marker.display()
    );
    fs::write(&executable, script).expect("fake Git should be written");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .expect("fake Git should be executable");
    let root = test_root(&temporary.path().join("cdenv"));
    let child_root = root.clone();
    let name = WorkspaceName::parse("cancelled-clone").expect("fixture name should validate");
    let child_name = name.clone();
    let cancellation = CancellationToken::default();
    let child_cancellation = cancellation.clone();

    let child = thread::spawn(move || {
        create_workspace(
            &child_root,
            CreateWorkspaceRequest {
                source: "host:cancelled-clone.git",
                name: Some(&child_name),
                config: None,
            },
            &GitAdapter::new(executable),
            &child_cancellation,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "clone marker should appear");
        thread::sleep(Duration::from_millis(1));
    }
    cancellation.cancel();
    let error = child
        .join()
        .expect("create thread should not panic")
        .expect_err("clone cancellation should fail");

    assert!(matches!(
        error,
        CreateWorkspaceError::Git(cdenv_cli::GitError::Cancelled { .. })
    ));
    assert!(!root.workspace(&name).root().exists());
}

#[test]
fn cancellation_observed_after_successful_clone_retains_recoverable_workspace() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let executable = temporary.path().join("cancel-after-clone-git");
    let marker = temporary.path().join("clone-complete");
    let script = format!(
        "#!/bin/sh\n\
         if [ \"$1\" = --version ]; then printf 'git version 2.47.1\\n'; exit 0; fi\n\
         mkdir -p \"$4\"\n\
         : > '{}'\n\
         sleep 0.002\n",
        marker.display()
    );
    fs::write(&executable, script).expect("fake Git should be written");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .expect("fake Git should be executable");
    let root = test_root(&temporary.path().join("cdenv"));
    let child_root = root.clone();
    let name = WorkspaceName::parse("cancelled").expect("fixture name should validate");
    let child_name = name.clone();
    let cancellation = CancellationToken::default();
    let child_cancellation = cancellation.clone();

    let child = thread::spawn(move || {
        create_workspace(
            &child_root,
            CreateWorkspaceRequest {
                source: "host:cancelled.git",
                name: Some(&child_name),
                config: None,
            },
            &GitAdapter::new(executable),
            &child_cancellation,
        )
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "clone marker should appear");
        thread::sleep(Duration::from_millis(1));
    }
    cancellation.cancel();
    let error = child
        .join()
        .expect("create thread should not panic")
        .expect_err("post-clone cancellation should fail recoverably");
    let paths = root.workspace(&name);
    let state = load_workspace_state(&paths.state_file()).expect("state should be retained");

    assert!(matches!(error, CreateWorkspaceError::CancelledAfterClone));
    assert_eq!(
        (
            paths.checkout().is_dir(),
            state.state().last_error().is_some()
        ),
        (true, true)
    );
}

#[test]
fn git_dependency_detection_is_command_scoped() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let (executable, record) = fake_git(temporary.path(), 0);
    let root = test_root(&temporary.path().join("cdenv"));
    let first = WorkspaceName::parse("first").expect("fixture name should validate");
    let second = WorkspaceName::parse("second").expect("fixture name should validate");
    let adapter = GitAdapter::new(executable);

    create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: "host:first.git",
            name: Some(&first),
            config: None,
        },
        &adapter,
        &CancellationToken::default(),
    )
    .expect("first fake clone should complete");
    create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: "host:second.git",
            name: Some(&second),
            config: None,
        },
        &adapter,
        &CancellationToken::default(),
    )
    .expect("second fake clone should complete");
    let arguments = fs::read_to_string(record).expect("fake arguments should read");

    assert_eq!(arguments.matches("ARG=--version").count(), 2);
}

#[test]
fn dependency_failure_occurs_before_workspace_reservation() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let root = test_root(&temporary.path().join("cdenv"));
    let name = WorkspaceName::parse("project").expect("fixture name should validate");

    let error = create_workspace(
        &root,
        CreateWorkspaceRequest {
            source: "host:project.git",
            name: Some(&name),
            config: None,
        },
        &GitAdapter::new(temporary.path().join("missing-git")),
        &CancellationToken::default(),
    )
    .expect_err("missing Git should fail dependency detection");

    assert!(matches!(error, CreateWorkspaceError::Git(_)));
    assert!(!root.workspace(&name).root().exists());
}

#[test]
fn explicit_config_rejects_absolute_parent_missing_and_directory_paths() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let checkout = temporary.path().join("checkout");
    fs::create_dir_all(checkout.join("directory")).expect("checkout fixture should exist");

    let absolute = validate_explicit_config(&checkout, Path::new("/outside.json"));
    let parent = validate_explicit_config(&checkout, Path::new("../outside.json"));
    let missing = validate_explicit_config(&checkout, Path::new("missing.json"));
    let directory = validate_explicit_config(&checkout, Path::new("directory"));

    assert!(matches!(absolute, Err(ConfigContainmentError::Lexical(_))));
    assert!(matches!(parent, Err(ConfigContainmentError::Lexical(_))));
    assert!(matches!(
        missing,
        Err(ConfigContainmentError::Missing { .. })
    ));
    assert!(matches!(
        directory,
        Err(ConfigContainmentError::NotRegular { .. })
    ));
}

#[test]
fn explicit_config_rejects_non_utf8_and_symlink_escape() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let checkout = temporary.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout should exist");
    let outside = temporary.path().join("outside.json");
    fs::write(&outside, b"{}").expect("outside config should exist");
    symlink(&outside, checkout.join("escape.json")).expect("escape symlink should exist");
    let non_utf8 = PathBuf::from(OsString::from_vec(vec![0xff]));

    let encoding = validate_explicit_config(&checkout, &non_utf8);
    let escape = validate_explicit_config(&checkout, Path::new("escape.json"));

    assert!(matches!(encoding, Err(ConfigContainmentError::NonUtf8)));
    assert!(matches!(
        escape,
        Err(ConfigContainmentError::EscapesCheckout { .. })
    ));
}
