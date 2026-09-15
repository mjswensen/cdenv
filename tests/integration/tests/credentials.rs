//! Packaged public-command credential workflow release contract.

#![cfg(feature = "credentials")]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SECRET_MARKER: &str = "credential-marker-must-not-persist";

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn run(program: &Path, arguments: &[&str], environment: &[(&str, &Path)]) -> Output {
    let mut command = Command::new(program);
    command
        .args(arguments)
        .stdin(Stdio::null())
        .env("CDENV_SECRET_MARKER", SECRET_MARKER);
    for (name, value) in environment {
        command.env(name, value);
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("cannot start {}: {error}", program.display()))
}

fn require_success(output: &Output, operation: &str) {
    assert!(
        output.status.success(),
        "{operation} failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output
            .stdout
            .windows(SECRET_MARKER.len())
            .any(|value| value == SECRET_MARKER.as_bytes())
            && !output
                .stderr
                .windows(SECRET_MARKER.len())
                .any(|value| value == SECRET_MARKER.as_bytes()),
        "{operation} disclosed the secret marker"
    );
}

fn wait_for_path(path: &Path, operation: &str) {
    for _ in 0..200 {
        if path.exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("{operation} did not create {}", path.display());
}

struct Agent {
    child: Child,
    socket: PathBuf,
}

impl Agent {
    fn start(root: &Path) -> Self {
        let socket = root.join("host-agent.sock");
        let mut child = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(&socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start controlled SSH agent");
        for _ in 0..200 {
            if socket.exists() {
                let key = root.join("fixture-agent-key");
                let generated = Command::new("ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(&key)
                    .status()
                    .expect("start ssh-keygen");
                assert!(generated.success(), "generate controlled agent key");
                let added = Command::new("ssh-add")
                    .arg(&key)
                    .env("SSH_AUTH_SOCK", &socket)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .expect("start ssh-add");
                assert!(added.success(), "load controlled agent key");
                return Self { child, socket };
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("controlled SSH agent did not create its socket");
    }
}

impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn create_repository(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    fs::create_dir_all(repository.join(".devcontainer")).expect("repository directory");
    fs::write(
        repository.join(".devcontainer/devcontainer.json"),
        r#"{
  "name": "credential-release-gate",
  "image": "debian:13-slim",
  "workspaceFolder": "/workspaces/credential-release-gate",
  "onCreateCommand": ["/bin/sh", "-c", "test -S \"$SSH_AUTH_SOCK\" && printf first-hook > .credential-first-hook"],
  "postStartCommand": ["/bin/sh", "-c", "test -S \"$SSH_AUTH_SOCK\" && printf detached > .credential-detached"],
  "postAttachCommand": ["/bin/sh", "-c", "test -S \"$SSH_AUTH_SOCK\" && printf post-attach > .credential-post-attach"]
}
"#,
    )
    .expect("devcontainer config");
    require_success(
        &Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .arg(&repository)
            .output()
            .expect("git init"),
        "git init",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["config", "user.name", "Credential Gate"])
            .output()
            .expect("git config name"),
        "git config name",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["config", "user.email", "gate@example.invalid"])
            .output()
            .expect("git config email"),
        "git config email",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["add", ".devcontainer/devcontainer.json"])
            .output()
            .expect("git add"),
        "git add",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["commit", "-m", "fixture"])
            .output()
            .expect("git commit"),
        "git commit",
    );
    repository
}

fn contains_marker(root: &Path) -> bool {
    fn visit(path: &Path) -> bool {
        let Ok(entries) = fs::read_dir(path) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if visit(&path) {
                    return true;
                }
            } else if metadata.is_file()
                && fs::read(&path).is_ok_and(|bytes| {
                    bytes
                        .windows(SECRET_MARKER.len())
                        .any(|window| window == SECRET_MARKER.as_bytes())
                })
            {
                return true;
            }
        }
        false
    }
    visit(root)
}

#[test]
fn credential_traceability_manifest_is_nonempty_and_names_public_workflows() {
    let manifest: serde_json::Value =
        serde_json::from_slice(include_bytes!("../fixtures/credential-coverage.json"))
            .expect("credential traceability JSON");
    let scenarios = manifest["scenarios"].as_array().expect("scenario array");
    let criteria = manifest["issue68Acceptance"]
        .as_array()
        .expect("criteria array");

    assert!(scenarios.len() >= 10 && criteria.len() >= 10);
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the packaged public-command sequence remains explicit and auditable"
)]
fn packaged_commands_cover_lifecycle_sessions_recovery_and_revocation() {
    let binary = PathBuf::from(
        std::env::var_os("CDENV_CREDENTIAL_TEST_BINARY")
            .expect("xtask must provide the packaged cdenv binary"),
    );
    require_success(
        &run(&binary, &["__validate-artifacts"], &[]),
        "artifact validation",
    );

    let shared_host_directory = std::env::var_os("CDENV_INTEGRATION_SHARED_TMP")
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let temporary = tempfile::Builder::new()
        .prefix("cdenv-credential-workflow-")
        .tempdir_in(shared_host_directory)
        .expect("temporary workflow");
    let root = temporary.path().join("cdenv-root");
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("host home");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("host home mode");
    let repository = create_repository(temporary.path());
    let workspace = unique_name("credential-gate");
    let agent = Agent::start(temporary.path());
    let root_text = root.to_str().expect("root text");
    let repository_text = repository.to_str().expect("repository text");
    let socket_text = agent.socket.to_str().expect("socket text");
    let environment = [
        ("HOME", home.as_path()),
        ("SSH_AUTH_SOCK", agent.socket.as_path()),
    ];

    let enable = run(
        &binary,
        &[
            "--root",
            root_text,
            "credentials",
            "enable",
            &workspace,
            "ssh-agent",
            "--socket",
            socket_text,
        ],
        &environment,
    );
    require_success(&enable, "staged credential enable");
    let create = run(
        &binary,
        &[
            "--root",
            root_text,
            "--no-modify-ssh-config",
            "create",
            repository_text,
            "--name",
            &workspace,
        ],
        &environment,
    );
    require_success(&create, "credential create");

    let checkout = root
        .join("workspaces")
        .join(&workspace)
        .join("checkout")
        .join(&workspace);
    assert_eq!(
        fs::read(checkout.join(".credential-first-hook")).expect("first hook marker"),
        b"first-hook"
    );
    let detached_marker = checkout.join(".credential-detached");
    wait_for_path(&detached_marker, "detached credential lifecycle");
    assert_eq!(
        fs::read(&detached_marker).expect("detached marker"),
        b"detached"
    );
    let ssh = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "test -S \"$SSH_AUTH_SOCK\" && printf ssh-managed",
        ],
        &environment,
    );
    require_success(&ssh, "managed SSH command");
    assert_eq!(ssh.stdout, b"ssh-managed");
    assert_eq!(
        fs::read(checkout.join(".credential-post-attach")).expect("postAttach marker"),
        b"post-attach"
    );

    let ssh_config = root.join("ssh/config");
    let ssh_config_text = ssh_config.to_str().expect("SSH config text");
    let host = format!("{workspace}.cdenv");
    let non_pty = run(
        Path::new("ssh"),
        &[
            "-F",
            ssh_config_text,
            "-T",
            &host,
            "test ! -t 0 && test ! -t 1 && test -S \"$SSH_AUTH_SOCK\" && printf non-pty",
        ],
        &environment,
    );
    require_success(&non_pty, "non-PTY OpenSSH child");
    assert_eq!(non_pty.stdout, b"non-pty");
    let pty = run(
        Path::new("ssh"),
        &[
            "-F",
            ssh_config_text,
            "-tt",
            &host,
            "test -t 0 && test -t 1 && test -S \"$SSH_AUTH_SOCK\" && printf pty",
        ],
        &environment,
    );
    require_success(&pty, "PTY OpenSSH child");
    assert!(pty.stdout.windows(3).any(|value| value == b"pty"));

    let mut concurrent = Vec::new();
    for _ in 0..4 {
        concurrent.push(
            Command::new(&binary)
                .args([
                    "--root",
                    root_text,
                    "ssh",
                    &workspace,
                    "--",
                    "/bin/sh",
                    "-c",
                    "test -S \"$SSH_AUTH_SOCK\" && sleep 0.2",
                ])
                .env("HOME", &home)
                .env("SSH_AUTH_SOCK", &agent.socket)
                .env("CDENV_SECRET_MARKER", SECRET_MARKER)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("start concurrent managed SSH child"),
        );
    }
    for child in concurrent {
        require_success(
            &child.wait_with_output().expect("concurrent SSH output"),
            "concurrent managed SSH child",
        );
    }

    let status = run(
        &binary,
        &[
            "--root",
            root_text,
            "credentials",
            "status",
            &workspace,
            "--json",
        ],
        &environment,
    );
    require_success(&status, "credential status");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&status.stdout).expect("status JSON")["transport"],
        "healthy"
    );

    require_success(
        &run(
            &binary,
            &["--root", root_text, "down", &workspace],
            &environment,
        ),
        "credential down",
    );
    require_success(
        &run(
            &binary,
            &["--root", root_text, "up", &workspace],
            &environment,
        ),
        "credential up",
    );
    require_success(
        &run(
            &binary,
            &["--root", root_text, "rebuild", &workspace, "--no-cache"],
            &environment,
        ),
        "credential rebuild",
    );

    let supervisor_state = root
        .join("workspaces")
        .join(&workspace)
        .join("runtime/supervisor.json");
    let supervisor: serde_json::Value =
        serde_json::from_slice(&fs::read(&supervisor_state).expect("supervisor state"))
            .expect("supervisor state JSON");
    let supervisor_pid = supervisor["pid"]
        .as_u64()
        .expect("supervisor PID")
        .to_string();
    require_success(
        &Command::new("kill")
            .args(["-KILL", &supervisor_pid])
            .output()
            .expect("kill supervisor"),
        "controlled supervisor loss",
    );
    std::thread::sleep(Duration::from_millis(100));
    require_success(
        &run(
            &binary,
            &["--root", root_text, "up", &workspace],
            &environment,
        ),
        "explicit up after supervisor loss",
    );
    require_success(
        &run(
            &binary,
            &[
                "--root",
                root_text,
                "ssh",
                &workspace,
                "--",
                "/bin/sh",
                "-c",
                "test -S \"$SSH_AUTH_SOCK\"",
            ],
            &environment,
        ),
        "credential recovery after supervisor loss",
    );

    let old_client_ready = checkout.join(".credential-old-client");
    let old_client = Command::new(&binary)
        .args([
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "test -S \"$SSH_AUTH_SOCK\" || exit 20; printf ready > .credential-old-client; sleep 1; printf survived > .credential-old-client-survived",
        ])
        .env("HOME", &home)
        .env("SSH_AUTH_SOCK", &agent.socket)
        .env("CDENV_SECRET_MARKER", SECRET_MARKER)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start old credential client");
    wait_for_path(&old_client_ready, "old credential client readiness");
    require_success(
        &run(
            &binary,
            &[
                "--root",
                root_text,
                "credentials",
                "disable",
                &workspace,
                "ssh-agent",
            ],
            &environment,
        ),
        "credential revocation",
    );
    require_success(
        &old_client.wait_with_output().expect("old client output"),
        "unrelated old SSH client survival",
    );
    assert_eq!(
        fs::read(checkout.join(".credential-old-client-survived"))
            .expect("old client survival marker"),
        b"survived"
    );
    let revoked = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "test -z \"${SSH_AUTH_SOCK+x}\"",
        ],
        &environment,
    );
    require_success(&revoked, "revoked SSH environment");
    assert!(!contains_marker(&root));

    require_success(
        &run(
            &binary,
            &["--root", root_text, "down", &workspace],
            &environment,
        ),
        "final down",
    );
}
