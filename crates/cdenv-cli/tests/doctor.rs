//! Process-level contracts for the read-only doctor command.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

use cdenv_cli::{CdenvRoot, Installation, ProcessEnvironment};

fn snapshot(path: &Path) -> Vec<(PathBuf, Vec<u8>, SystemTime)> {
    let mut entries = fs::read_dir(path)
        .expect("directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("entries");
    entries.sort_by_key(fs::DirEntry::file_name);
    entries
        .into_iter()
        .map(|entry| {
            let path = entry.path();
            (
                path.file_name().expect("name").into(),
                fs::read(&path).expect("file bytes"),
                fs::metadata(&path)
                    .expect("metadata")
                    .modified()
                    .expect("modified time"),
            )
        })
        .collect()
}

#[test]
fn doctor_json_is_one_document_on_required_failure_and_never_creates_the_root() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = temporary.path().join("missing-root");
    let output = Command::new(env!("CARGO_BIN_EXE_cdenv"))
        .arg("--root")
        .arg(&root)
        .args(["doctor", "--json"])
        .output()
        .expect("run doctor");

    assert!(!output.status.success());
    assert!(output.stderr.is_empty());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(document["schemaVersion"], 1);
    assert!(document["checks"].is_array());
    assert!(!root.exists());
}

#[test]
fn doctor_does_not_change_existing_managed_files() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root_path = temporary.path().join("root");
    fs::create_dir(&root_path).expect("root");
    let root = CdenvRoot::resolve(Some(&root_path), &ProcessEnvironment).expect("root path");
    Installation::open_or_create(&root).expect("installation");
    let before = snapshot(&root_path);

    let output = Command::new(env!("CARGO_BIN_EXE_cdenv"))
        .arg("--root")
        .arg(&root_path)
        .arg("doctor")
        .output()
        .expect("run doctor");

    assert!(output.status.success());
    assert_eq!(snapshot(&root_path), before);
    assert!(String::from_utf8_lossy(&output.stdout).contains("installation"));
}
