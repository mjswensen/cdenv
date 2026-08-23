#![cfg(target_os = "linux")]
//! Linux executable contracts for environment capture, filtering, and reuse.

use std::ffi::OsString;
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde::Deserialize;
use serde_json::json;
use tempfile::TempDir;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CaptureOutput {
    snapshot_path: PathBuf,
    entries: usize,
}

fn agent() -> &'static str {
    env!("CARGO_BIN_EXE_cdenv-agent")
}

fn capture(probe: &str, directory: &Path, base: &str) -> (CaptureOutput, Output) {
    let request = serde_json::to_vec(&json!({
        "generation": "fixture-generation",
        "stateDirectory": directory,
        "probe": probe,
        "remoteEnvironment": {
            "RESULT": [
                {"kind": "literal", "value": "prefix-"},
                {"kind": "containerEnvironment", "name": "BASE", "default": "fallback"}
            ],
            "REMOVE_ME": null
        }
    }))
    .expect("capture request");
    let mut child = Command::new(agent())
        .arg("capture-environment")
        .env("BASE", base)
        .env("REMOVE_ME", "transient")
        .env("PWD", "transient")
        .env("OLDPWD", "transient")
        .env("SHLVL", "99")
        .env("_", "transient")
        .env("SSH_AUTH_SOCK", "transient")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("capture process");
    std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), &request)
        .expect("request write");
    let output = child.wait_with_output().expect("capture output");
    let parsed = serde_json::from_slice(&output.stdout).expect("machine result");
    (parsed, output)
}

fn assert_probe(probe: &str) {
    let directory = TempDir::new().expect("state directory");
    let (capture, output) = capture(probe, directory.path(), "container");
    assert!(output.status.success(), "capture stderr must stay safe");
    assert!(output.stderr.is_empty());
    assert!(capture.entries > 0);
    let child = Command::new(agent())
        .args([
            "run-environment",
            capture.snapshot_path.to_str().expect("snapshot path"),
            "--",
            "/usr/bin/env",
        ])
        .output()
        .expect("environment child");
    assert!(child.status.success(), "effective environment child");
    let entries = child
        .stdout
        .split(|byte| *byte == b'\n')
        .collect::<Vec<_>>();
    assert!(entries.contains(&b"RESULT=prefix-container".as_slice()));
    assert!(!entries.iter().any(|entry| {
        [
            b"REMOVE_ME=".as_slice(),
            b"PWD=".as_slice(),
            b"OLDPWD=".as_slice(),
            b"SHLVL=".as_slice(),
            b"_=".as_slice(),
            b"SSH_".as_slice(),
        ]
        .iter()
        .any(|prefix| entry.starts_with(prefix))
    }));
}

#[test]
fn none_probe_captures_the_container_environment() {
    assert_probe("none");
}

#[test]
fn login_shell_probe_captures_the_selected_user_environment() {
    assert_probe("loginShell");
}

#[test]
fn login_interactive_shell_probe_captures_the_selected_user_environment() {
    assert_probe("loginInteractiveShell");
}

#[test]
fn interactive_shell_probe_captures_the_selected_user_environment() {
    assert_probe("interactiveShell");
}

#[test]
fn capture_atomically_replaces_the_generation_snapshot() {
    let directory = TempDir::new().expect("state directory");
    let (first, _) = capture("none", directory.path(), "first");
    let (second, _) = capture("none", directory.path(), "second");
    assert_eq!(first.snapshot_path, second.snapshot_path);
    let child = Command::new(agent())
        .args([
            "run-environment",
            second.snapshot_path.to_str().expect("snapshot path"),
            "--",
            "/bin/sh",
            "-c",
            "test \"$RESULT\" = prefix-second",
        ])
        .status()
        .expect("environment child");
    assert!(child.success());
}

#[test]
fn snapshot_is_restricted_and_owned_by_the_selected_user() {
    let directory = TempDir::new().expect("state directory");
    let (capture, _) = capture("none", directory.path(), "value");
    let metadata = fs::metadata(capture.snapshot_path).expect("snapshot metadata");
    assert_eq!(metadata.mode() & 0o777, 0o600);
    assert_eq!(metadata.uid(), nix::unistd::geteuid().as_raw());
}

#[test]
fn non_utf8_unix_environment_entries_round_trip_without_host_decoding() {
    let directory = TempDir::new().expect("state directory");
    let request = serde_json::to_vec(&json!({
        "generation": "binary",
        "stateDirectory": directory.path(),
        "probe": "none",
        "remoteEnvironment": {}
    }))
    .expect("capture request");
    let binary_name = OsString::from_vec(vec![b'B', b'I', b'N', 0x80]);
    let binary_value = OsString::from_vec(vec![b'V', 0xff]);
    let mut child = Command::new(agent())
        .arg("capture-environment")
        .env(&binary_name, &binary_value)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("capture process");
    std::io::Write::write_all(child.stdin.as_mut().expect("stdin"), &request)
        .expect("request write");
    let output = child.wait_with_output().expect("capture output");
    let capture: CaptureOutput = serde_json::from_slice(&output.stdout).expect("machine result");

    let output = Command::new(agent())
        .args([
            "run-environment",
            capture.snapshot_path.to_str().expect("snapshot path"),
            "--",
            "/usr/bin/env",
        ])
        .output()
        .expect("environment listing");
    let mut expected = binary_name.into_vec();
    expected.push(b'=');
    expected.extend(binary_value.into_vec());
    assert!(
        output
            .stdout
            .windows(expected.len())
            .any(|value| value == expected)
    );
}

#[test]
fn capture_machine_output_never_contains_effective_values() {
    let marker = "secret-marker-that-must-not-escape";
    let directory = TempDir::new().expect("state directory");
    let (_, output) = capture("none", directory.path(), marker);
    let combined = [output.stdout, output.stderr].concat();
    assert!(
        !combined
            .windows(marker.len())
            .any(|value| value == marker.as_bytes())
    );
}
