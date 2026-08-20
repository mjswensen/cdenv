#![cfg(unix)]
//! Fake-executable contracts for Docker endpoint propagation and process safety.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use cdenv_cli::{
    CancellationToken, DockerCommandProbe, DockerEndpoint, DockerEnvironment, DockerSocketProbe,
    ProcessDeadline, ProcessEnvironmentVariable, ProcessError, ProcessRequest, ProcessRunner,
    Version,
};

struct Environment {
    host: OsString,
}

impl DockerEnvironment for Environment {
    fn docker_host(&self) -> Option<OsString> {
        Some(self.host.clone())
    }

    fn docker_context(&self) -> Option<OsString> {
        None
    }

    fn home_dir(&self) -> Option<PathBuf> {
        None
    }

    fn runtime_dir(&self) -> Option<PathBuf> {
        None
    }
}

struct ExactSocket(PathBuf);

impl DockerSocketProbe for ExactSocket {
    fn is_unix_socket(&self, path: &Path) -> bool {
        path == self.0
    }
}

fn endpoint(path: &Path) -> DockerEndpoint {
    let host = OsString::from(format!("unix://{}", path.display()));
    DockerEndpoint::resolve_with_probe(&Environment { host }, &ExactSocket(path.to_path_buf()))
        .expect("fake socket should resolve")
}

fn executable(path: &Path, script: &str) {
    fs::write(path, script).expect("fake executable should be written");
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .expect("fake executable should be executable");
}

#[tokio::test]
async fn runner_uses_exact_argv_cwd_environment_without_a_shell() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let fake = temporary.path().join("fake-process");
    let record = temporary.path().join("record");
    let cwd = temporary.path().join("working directory");
    fs::create_dir(&cwd).expect("working directory should exist");
    executable(
        &fake,
        "#!/bin/sh\nprintf 'cwd=%s\\n' \"$PWD\" > \"$RECORD\"\nprintf 'value=%s\\n' \"$ONLY_VALUE\" >> \"$RECORD\"\nfor arg in \"$@\"; do printf 'arg=%s\\n' \"$arg\" >> \"$RECORD\"; done\n",
    );
    let shell_sentinel = temporary.path().join("shell-was-used");
    let hostile = format!("literal;touch {}", shell_sentinel.display());
    let environment = [
        ProcessEnvironmentVariable {
            name: OsStr::new("RECORD"),
            value: record.as_os_str(),
            sensitive: false,
        },
        ProcessEnvironmentVariable {
            name: OsStr::new("ONLY_VALUE"),
            value: OsStr::new("exact"),
            sensitive: false,
        },
    ];
    let arguments: [&OsStr; 2] = [OsStr::new("first argument"), OsStr::new(&hostile)];
    let request = ProcessRequest {
        operation: "exact-process",
        executable: &fake,
        arguments: &arguments,
        cwd: &cwd,
        environment: &environment,
        redactions: &[],
        deadline: ProcessDeadline::Control(Duration::from_secs(5)),
    };

    let result = ProcessRunner::new(temporary.path().join("logs"))
        .run(&request, &CancellationToken::default())
        .await
        .expect("fake process should run");

    assert!(result.status.success());
    assert_eq!(
        fs::read_to_string(record).expect("record should read"),
        format!(
            "cwd={}\nvalue=exact\narg=first argument\narg={hostile}\n",
            cwd.display()
        )
    );
    assert!(!shell_sentinel.exists());
}

#[tokio::test]
async fn docker_and_compose_probes_share_the_bollard_endpoint_and_exact_docker_host() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let fake = temporary.path().join("fake-docker");
    let socket = temporary.path().join("docker.sock");
    let record = temporary.path().join("record");
    fs::write(&socket, b"").expect("Bollard fixture path should exist");
    executable(
        &fake,
        "#!/bin/sh\nprintf 'host=%s args=' \"$DOCKER_HOST\" >> \"$RECORD\"\nprintf '%s|' \"$@\" >> \"$RECORD\"\nprintf '\\n' >> \"$RECORD\"\nif [ \"$1\" = version ]; then\n  printf '{\"Client\":{\"Version\":\"30.1.0\"},\"Server\":{\"Version\":\"30.2.0\",\"ApiVersion\":\"1.56\"}}\\n'\nelse\n  printf '{\"version\":\"v6.0.0\"}\\n'\nfi\n",
    );
    let endpoint = endpoint(&socket);
    let probe = DockerCommandProbe::new(
        fake,
        endpoint.clone(),
        ProcessRunner::new(temporary.path().join("logs")),
        vec![
            (OsString::from("RECORD"), record.as_os_str().to_owned()),
            (OsString::from("DOCKER_HOST"), OsString::from("ssh://wrong")),
        ],
    );

    let capabilities = probe
        .probe_compose(temporary.path(), &CancellationToken::default())
        .await
        .expect("newer Docker and Compose should pass");
    let connector = endpoint
        .bollard_connector(Duration::from_secs(1))
        .expect("existing fixture path should configure Bollard");
    let record = fs::read_to_string(record).expect("probe record should read");

    assert_eq!(capabilities.compose, Some(Version::new(6, 0, 0)));
    assert_eq!(probe.endpoint(), connector.endpoint());
    assert_eq!(
        record,
        format!(
            "host=unix://{} args=version|--format|{{{{json .}}}}|\nhost=unix://{} args=compose|version|--format|json|\n",
            socket.display(),
            socket.display()
        )
    );
}

#[tokio::test]
async fn runner_redacts_environment_and_header_values_before_bounded_capture_and_logging() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let fake = temporary.path().join("fake-secret-output");
    executable(
        &fake,
        "#!/bin/sh\nprintf '%s %s ' \"$SECRET_ENV\" \"$HEADER\"\nprintf '012345678901234567890123456789'\nprintf '%s %s\\n' \"$SECRET_ENV\" \"$HEADER\" >&2\n",
    );
    let environment = [
        ProcessEnvironmentVariable {
            name: OsStr::new("SECRET_ENV"),
            value: OsStr::new("environment-marker"),
            sensitive: true,
        },
        ProcessEnvironmentVariable {
            name: OsStr::new("HEADER"),
            value: OsStr::new("header-marker"),
            sensitive: false,
        },
    ];
    let request = ProcessRequest {
        operation: "redaction",
        executable: &fake,
        arguments: &[],
        cwd: temporary.path(),
        environment: &environment,
        redactions: &[b"header-marker"],
        deadline: ProcessDeadline::Control(Duration::from_secs(5)),
    };

    let result = ProcessRunner::new(temporary.path().join("logs"))
        .with_bounds(24, 128)
        .run(&request, &CancellationToken::default())
        .await
        .expect("secret output process should run");
    let log = fs::read(&result.log_path).expect("restricted log should read");
    let mode = fs::metadata(&result.log_path)
        .expect("log metadata should read")
        .permissions()
        .mode()
        & 0o777;

    assert_eq!(mode, 0o600);
    assert!(result.stdout.is_truncated());
    for marker in [
        b"environment-marker".as_slice(),
        b"header-marker".as_slice(),
    ] {
        assert!(
            !result
                .stdout
                .as_bytes()
                .windows(marker.len())
                .any(|bytes| bytes == marker)
        );
        assert!(
            !result
                .stderr
                .as_bytes()
                .windows(marker.len())
                .any(|bytes| bytes == marker)
        );
        assert!(!log.windows(marker.len()).any(|bytes| bytes == marker));
    }
}

#[tokio::test]
async fn cancellation_terminates_the_entire_owned_process_group() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let fake = temporary.path().join("fake-process-group");
    let started = temporary.path().join("started");
    let survived = temporary.path().join("grandchild-survived");
    executable(
        &fake,
        &format!(
            "#!/bin/sh\n: > '{}'\n(sleep 1; : > '{}') &\nwait\n",
            started.display(),
            survived.display()
        ),
    );
    let cancellation = CancellationToken::default();
    let child_cancellation = cancellation.clone();
    let log_directory = temporary.path().join("logs");
    let cwd = temporary.path().to_path_buf();
    let child = tokio::spawn(async move {
        let request = ProcessRequest {
            operation: "cancellation",
            executable: &fake,
            arguments: &[],
            cwd: &cwd,
            environment: &[],
            redactions: &[],
            deadline: ProcessDeadline::Unbounded,
        };
        ProcessRunner::new(log_directory)
            .run(&request, &child_cancellation)
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !started.exists() {
        assert!(Instant::now() < deadline, "process marker should appear");
        tokio::time::sleep(Duration::from_millis(2)).await;
    }

    cancellation.cancel();
    let error = child
        .await
        .expect("runner task should not panic")
        .expect_err("cancellation should fail the operation");
    tokio::time::sleep(Duration::from_millis(1100)).await;

    assert!(matches!(error, ProcessError::Cancelled { .. }));
    assert!(!survived.exists());
}

#[tokio::test]
async fn control_probe_deadline_is_bounded_but_unbounded_work_has_no_default_timeout() {
    let temporary = tempfile::tempdir().expect("temporary directory should exist");
    let fake = temporary.path().join("fake-timeout");
    executable(&fake, "#!/bin/sh\nsleep 30\n");
    let request = ProcessRequest {
        operation: "control-timeout",
        executable: &fake,
        arguments: &[],
        cwd: temporary.path(),
        environment: &[],
        redactions: &[],
        deadline: ProcessDeadline::Control(Duration::from_millis(20)),
    };

    let error = ProcessRunner::new(temporary.path().join("logs"))
        .run(&request, &CancellationToken::default())
        .await
        .expect_err("bounded probe should time out");

    assert!(matches!(error, ProcessError::TimedOut { .. }));
}
