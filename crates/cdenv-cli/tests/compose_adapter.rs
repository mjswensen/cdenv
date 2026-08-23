#![cfg(unix)]
//! Fake-executable contracts for the Docker Compose V2 adapter.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cdenv_cli::{
    CancellationToken, ComposeAdapter, ComposeProject, ComposeStopRequest, DockerEndpoint,
    DockerEnvironment, DockerSocketProbe, ProcessRunner,
};

struct Environment(OsString);

impl DockerEnvironment for Environment {
    fn docker_host(&self) -> Option<OsString> {
        Some(self.0.clone())
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

#[tokio::test]
async fn resolve_model_uses_exact_files_project_cwd_endpoint_and_secret_safe_stdout() {
    let temporary = tempfile::tempdir().expect("temporary fixture");
    let checkout = temporary.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    let compose_file = checkout.join("compose file.yaml");
    fs::write(&compose_file, "services: {}").expect("Compose file");
    let executable = temporary.path().join("fake-docker");
    let record = temporary.path().join("record");
    fs::write(
        &executable,
        r#"#!/bin/sh
set -eu
printf 'host=%s\ncwd=%s\n' "$DOCKER_HOST" "$PWD" > "$RECORD"
printf 'arg=%s\n' "$@" >> "$RECORD"
printf '%s\n' '{"services":{"app":{"image":"example.invalid/app:latest","depends_on":{"db":{"condition":"service_started"}},"user":"secret-user"},"db":{"image":"example.invalid/db:latest"}}}'
"#,
    )
    .expect("fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("executable mode");
    let socket = temporary.path().join("docker.sock");
    let host = OsString::from(format!("unix://{}", socket.display()));
    let endpoint =
        DockerEndpoint::resolve_with_probe(&Environment(host), &ExactSocket(socket.clone()))
            .expect("endpoint");
    let logs = temporary.path().join("logs");
    let adapter = ComposeAdapter::new(
        executable,
        endpoint,
        ProcessRunner::new(logs.clone()),
        vec![(OsString::from("RECORD"), record.as_os_str().to_owned())],
        temporary.path().join("managed-tmp"),
    );

    let model = adapter
        .resolve_model(
            ComposeProject {
                files: std::slice::from_ref(&compose_file),
                project_name: "cdenv-installation-workspace",
                working_directory: &checkout,
            },
            &CancellationToken::default(),
        )
        .await
        .expect("resolved model");

    assert_eq!(
        fs::read_to_string(record).expect("record"),
        format!(
            "host=unix://{}\ncwd={}\narg=compose\narg=--project-directory\narg={}\narg=--project-name\narg=cdenv-installation-workspace\narg=--file\narg={}\narg=config\narg=--format\narg=json\n",
            socket.display(),
            checkout.display(),
            checkout.display(),
            compose_file.display()
        )
    );
    assert_eq!(
        model.services["app"].dependencies,
        ["db".to_owned()].into_iter().collect()
    );
    let log = fs::read(
        fs::read_dir(logs)
            .expect("logs")
            .next()
            .expect("one log")
            .expect("log entry")
            .path(),
    )
    .expect("operation log");
    assert!(
        !log.windows(b"secret-user".len())
            .any(|value| value == b"secret-user")
    );
}

#[tokio::test]
async fn stop_targets_only_the_persisted_managed_services_without_down_or_override() {
    let temporary = tempfile::tempdir().expect("temporary fixture");
    let checkout = temporary.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    let compose_file = checkout.join("compose.yaml");
    fs::write(&compose_file, "services: {}").expect("Compose file");
    let executable = temporary.path().join("fake-docker");
    let record = temporary.path().join("record");
    fs::write(
        &executable,
        "#!/bin/sh\nset -eu\nprintf '%s\\n' \"$@\" > \"$RECORD\"\n",
    )
    .expect("fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("executable mode");
    let socket = temporary.path().join("docker.sock");
    let endpoint = DockerEndpoint::resolve_with_probe(
        &Environment(OsString::from(format!("unix://{}", socket.display()))),
        &ExactSocket(socket),
    )
    .expect("endpoint");
    let adapter = ComposeAdapter::new(
        executable,
        endpoint,
        ProcessRunner::new(temporary.path().join("logs")),
        vec![(OsString::from("RECORD"), record.as_os_str().to_owned())],
        temporary.path().join("managed-tmp"),
    );
    let managed = vec!["app".to_owned(), "db".to_owned()];

    adapter
        .stop(
            &ComposeStopRequest {
                project: ComposeProject {
                    files: std::slice::from_ref(&compose_file),
                    project_name: "cdenv-installation-workspace",
                    working_directory: &checkout,
                },
                managed_services: &managed,
            },
            &CancellationToken::default(),
        )
        .await
        .expect("managed stop");

    let arguments = fs::read_to_string(record).expect("record");
    assert!(arguments.ends_with("stop\napp\ndb\n"), "{arguments}");
    assert!(!arguments.contains("down"), "{arguments}");
}

#[tokio::test]
async fn resolve_model_rejects_malformed_typed_output_without_echoing_it() {
    let temporary = tempfile::tempdir().expect("temporary fixture");
    let checkout = temporary.path().join("checkout");
    fs::create_dir(&checkout).expect("checkout");
    let compose_file = checkout.join("compose.yaml");
    fs::write(&compose_file, "services: {}").expect("Compose file");
    let executable = temporary.path().join("fake-docker");
    fs::write(
        &executable,
        "#!/bin/sh\nprintf '%s' 'top-secret malformed output'\n",
    )
    .expect("fake executable");
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("executable mode");
    let socket = temporary.path().join("docker.sock");
    let endpoint = DockerEndpoint::resolve_with_probe(
        &Environment(OsString::from(format!("unix://{}", socket.display()))),
        &ExactSocket(socket),
    )
    .expect("endpoint");
    let adapter = ComposeAdapter::new(
        executable,
        endpoint,
        ProcessRunner::new(temporary.path().join("logs")),
        Vec::new(),
        temporary.path().join("managed-tmp"),
    );

    let error = adapter
        .resolve_model(
            ComposeProject {
                files: std::slice::from_ref(&compose_file),
                project_name: "cdenv-installation-workspace",
                working_directory: &checkout,
            },
            &CancellationToken::default(),
        )
        .await
        .expect_err("malformed output should fail");

    assert!(!error.to_string().contains("top-secret"));
}
