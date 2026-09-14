//! Git helper-chain isolation through the real Git credential plumbing.
#![cfg(target_os = "linux")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fmt::Write as _;
use std::fs;
use std::io::{Read as _, Write as _};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};

use cdenv_agent::ManagedGitCredentialIntegration;
use cdenv_core::credentials::HttpsOrigin;

const REQUEST: &str =
    "protocol=https\nhost=granted.example\npath=team/repo.git\nusername=alice\n\n";
const CREDENTIAL: &str = "username=alice\npassword=SECRET-FORWARDED-TOKEN\n\n";

#[test]
#[expect(clippy::too_many_lines, reason = "one ordered end-to-end Git sequence")]
fn git_fill_approve_and_reject_isolate_granted_origin_native_helpers() {
    let temporary = tempfile::tempdir().expect("temporary");
    let native_log = temporary.path().join("native.log");
    let native = temporary.path().join("native-helper");
    fs::write(
        &native,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{}'\ncat >> '{}'\n[ \"$1\" = get ] && printf 'username=native\\npassword=NATIVE-TOKEN\\n'\n",
            native_log.display(),
            native_log.display()
        ),
    )
    .expect("native helper");
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).expect("native mode");

    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home");
    let global = home.join(".gitconfig");
    fs::write(
        &global,
        format!(
            "[credential]\n\thelper = !{}\n[credential \"https://granted.example/team/repo.git\"]\n\thelper = !{}\n",
            native.display(),
            native.display()
        ),
    )
    .expect("global");

    let repository = temporary.path().join("repository");
    fs::create_dir(&repository).expect("repository");
    run_git(
        Command::new("git").arg("init").arg("-q").arg(&repository),
        "init",
    );
    let local = repository.join(".git/config");
    let mut local_contents = fs::read_to_string(&local).expect("local");
    writeln!(
        &mut local_contents,
        "[credential]\n\thelper = !{}",
        native.display()
    )
    .expect("local config formatting");
    fs::write(&local, &local_contents).expect("local helper");
    let global_before = fs::read(&global).expect("global snapshot");
    let local_before = fs::read(&local).expect("local snapshot");

    let socket = temporary.path().join("credential.sock");
    let listener = UnixListener::bind(&socket).expect("listener");
    let recipient = temporary.path().join("private-recipient");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut input = Vec::new();
        stream.read_to_end(&mut input).expect("request");
        fs::write(recipient, &input).expect("recipient");
        stream.write_all(CREDENTIAL.as_bytes()).expect("response");
    });

    let integration = ManagedGitCredentialIntegration::refresh(
        &temporary.path().join("integration"),
        Path::new(env!("CARGO_BIN_EXE_cdenv-agent")),
        &socket,
        &[HttpsOrigin::parse("https://granted.example").expect("origin")],
    )
    .expect("integration");
    let mut environment = BTreeMap::from([
        (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
        (
            OsString::from("GIT_CONFIG_KEY_0"),
            OsString::from("core.askPass"),
        ),
        (OsString::from("GIT_CONFIG_VALUE_0"), OsString::new()),
    ]);
    integration
        .enroll_environment(&mut environment)
        .expect("enroll");

    let fill = credential_command(&repository, &home, &environment, "fill", REQUEST);
    assert!(fill.status.success(), "fill failed");
    assert_eq!(
        String::from_utf8(fill.stdout).expect("utf8"),
        format!(
            "protocol=https\nhost=granted.example\npath=team/repo.git\n{}",
            CREDENTIAL.trim_end_matches('\n')
        ) + "\n"
    );
    server.join().expect("server");

    for operation in ["approve", "reject"] {
        let input = REQUEST.replace("\n\n", "\npassword=SECRET-FORWARDED-TOKEN\n\n");
        let result = credential_command(&repository, &home, &environment, operation, &input);
        assert!(result.status.success(), "{operation} failed");
    }
    assert!(
        !native_log.exists(),
        "native helper received granted-origin traffic"
    );
    assert_eq!(fs::read(&global).expect("global after"), global_before);
    assert_eq!(fs::read(&local).expect("local after"), local_before);

    let ungranted = credential_command(
        &repository,
        &home,
        &environment,
        "fill",
        "protocol=https\nhost=ungranted.example\npath=other.git\n\n",
    );
    assert!(ungranted.status.success(), "ungranted fill failed");
    assert!(
        String::from_utf8(ungranted.stdout)
            .expect("utf8")
            .contains("password=NATIVE-TOKEN")
    );
    assert!(
        fs::read_to_string(native_log)
            .expect("native log")
            .contains("get")
    );

    integration.remove().expect("remove");
    assert!(!integration.config_path().exists());
}

fn credential_command(
    repository: &Path,
    home: &Path,
    environment: &BTreeMap<OsString, OsString>,
    operation: &str,
    input: &str,
) -> std::process::Output {
    let mut child = Command::new("git");
    child
        .args(["credential", operation])
        .current_dir(repository)
        .env_clear()
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .envs(environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = child.spawn().expect("git credential");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input.as_bytes())
        .expect("input");
    child.wait_with_output().expect("output")
}

fn run_git(command: &mut Command, context: &str) {
    assert!(
        command
            .status()
            .unwrap_or_else(|error| panic!("{context}: {error}"))
            .success()
    );
}
