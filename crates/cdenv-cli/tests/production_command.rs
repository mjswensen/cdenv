//! Installed-binary coverage for the real create/up dispatch boundary.

use std::fs;
use std::process::{Command, Stdio};

#[test]
fn create_reaches_production_planning_and_retains_checkout_on_failure() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let repository = temporary.path().join("repository");
    fs::create_dir_all(repository.join(".devcontainer")).expect("configuration directory");
    fs::write(
        repository.join(".devcontainer/devcontainer.json"),
        b"{\"image\":",
    )
    .expect("malformed configuration fixture");
    run_git(&repository, &["init", "--quiet"]);
    run_git(&repository, &["add", "."]);
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=cdenv test",
            "-c",
            "user.email=cdenv@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ])
        .current_dir(&repository)
        .stdin(Stdio::null())
        .status()
        .expect("Git commit");
    assert!(status.success());

    let root = temporary.path().join("root");
    let output = Command::new(env!("CARGO_BIN_EXE_cdenv"))
        .arg("--root")
        .arg(&root)
        .args(["--no-modify-ssh-config", "create", "--name", "production"])
        .arg(&repository)
        .stdin(Stdio::null())
        .output()
        .expect("installed cdenv invocation");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("environment failed:"), "{stderr}");
    assert!(
        !stderr.contains("not implemented in this build"),
        "{stderr}"
    );
    assert!(
        root.join("workspaces/production/checkout/production")
            .is_dir()
    );
}

fn run_git(repository: &std::path::Path, arguments: &[&str]) {
    let status = Command::new("git")
        .args(arguments)
        .current_dir(repository)
        .stdin(Stdio::null())
        .status()
        .expect("Git fixture command");
    assert!(status.success());
}
