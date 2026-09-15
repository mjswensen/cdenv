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

struct HttpsServer {
    child: Child,
    port: u16,
}

impl HttpsServer {
    fn start(root: &Path, project_root: &Path, credential: &Path) -> Self {
        let ready = root.join("https-server.ready");
        let mut child = Command::new(env!("CARGO_BIN_EXE_credential-https-server"))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/credential-tls/server.pem"
            ))
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/credential-tls/server-key.pem"
            ))
            .arg(project_root)
            .arg(credential)
            .arg(&ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("start controlled HTTPS server");
        for _ in 0..200 {
            if let Ok(value) = fs::read_to_string(&ready) {
                let port = value.parse().expect("HTTPS fixture port");
                return Self { child, port };
            }
            assert!(
                child.try_wait().expect("inspect HTTPS fixture").is_none(),
                "controlled HTTPS server exited before readiness"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("controlled HTTPS server did not become ready");
    }
}

impl Drop for HttpsServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Agent {
    child: Child,
    socket: PathBuf,
    public_key: PathBuf,
}

impl Agent {
    fn start(root: &Path) -> Self {
        let socket = root.join("host-agent.sock");
        let key = root.join("fixture-agent-key");
        let generated = Command::new("ssh-keygen")
            .args(["-q", "-t", "ed25519", "-N", "", "-f"])
            .arg(&key)
            .status()
            .expect("start ssh-keygen");
        assert!(generated.success(), "generate controlled agent key");
        let child = Self::start_process(&socket, &key);
        Self {
            child,
            socket,
            public_key: key.with_extension("pub"),
        }
    }

    fn restart_same_path(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.socket);
        self.child = Self::start_process(&self.socket, &self.public_key.with_extension(""));
    }

    fn start_process(socket: &Path, key: &Path) -> Child {
        let mut child = Command::new("ssh-agent")
            .args(["-D", "-a"])
            .arg(socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("start controlled SSH agent");
        for _ in 0..200 {
            if socket.exists() {
                let added = Command::new("ssh-add")
                    .arg(key)
                    .env("SSH_AUTH_SOCK", socket)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .expect("start ssh-add");
                assert!(added.success(), "load controlled agent key");
                return child;
            }
            std::thread::sleep(Duration::from_millis(10));
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

fn create_repository(root: &Path, agent_public_key: &Path) -> PathBuf {
    let repository = root.join("repository");
    fs::create_dir_all(repository.join(".devcontainer")).expect("repository directory");
    fs::copy(
        agent_public_key,
        repository.join(".devcontainer/fixture-agent-key.pub"),
    )
    .expect("copy fixture agent public key");
    fs::write(
        repository.join(".devcontainer/devcontainer.json"),
        r#"{
  "name": "credential-release-gate",
  "image": "buildpack-deps:trixie",
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
            .args(["add", ".devcontainer"])
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

fn contains_bytes(root: &Path, marker: &[u8]) -> bool {
    fn visit(path: &Path, marker: &[u8]) -> bool {
        let Ok(entries) = fs::read_dir(path) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(metadata) = fs::symlink_metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if visit(&path, marker) {
                    return true;
                }
            } else if metadata.is_file()
                && fs::read(&path)
                    .is_ok_and(|bytes| bytes.windows(marker.len()).any(|window| window == marker))
            {
                return true;
            }
        }
        false
    }
    visit(root, marker)
}

fn contains_marker(root: &Path) -> bool {
    contains_bytes(root, SECRET_MARKER.as_bytes())
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
    reason = "the authenticated smart-HTTP sequence remains explicit and auditable"
)]
fn packaged_git_https_paths_accounts_submodule_rotation_and_helper_isolation() {
    const FIRST_TOKEN: &str = "credential-https-secret-one";
    const SECOND_TOKEN: &str = "credential-https-secret-two";
    const DEPENDENCY_TOKEN: &str = "credential-https-dependency-secret";

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
        .prefix("cdenv-credential-https-")
        .tempdir_in(shared_host_directory)
        .expect("temporary HTTPS workflow");
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("host home");
    fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("host home mode");
    let host_credentials = temporary.path().join("host-credentials");
    let server_credentials = temporary.path().join("server-credentials");
    let initial_credentials = format!(
        "team/repo.git\talice\t{FIRST_TOKEN}\nteam/dependency.git\tbob\t{DEPENDENCY_TOKEN}\n"
    );
    fs::write(&host_credentials, &initial_credentials).expect("initial host credentials");
    fs::write(&server_credentials, &initial_credentials).expect("initial server credentials");
    for path in [&host_credentials, &server_credentials] {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).expect("credential mode");
    }
    let helper = temporary.path().join("host-helper");
    fs::write(
        &helper,
        format!(
            "#!/bin/sh\n[ \"$1\" = get ] || exit 0\npath=\nwhile IFS='=' read -r key value; do [ \"$key\" = path ] && path=$value; done\nawk -F '\\t' -v path=\"$path\" '$1 == path {{ printf \"username=%s\\npassword=%s\\n\", $2, $3; found=1 }} END {{ exit !found }}' '{}'\n",
            host_credentials.display()
        ),
    )
    .expect("host helper");
    fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper mode");
    fs::write(
        home.join(".gitconfig"),
        format!(
            "[credential]\n\thelper = !{}\n\tuseHttpPath = true\n",
            helper.display()
        ),
    )
    .expect("host Git config");

    let project_root = temporary.path().join("smart-http");
    let bare = project_root.join("team/repo.git");
    fs::create_dir_all(bare.parent().expect("bare parent")).expect("smart HTTP root");
    require_success(
        &Command::new("git")
            .args(["init", "--bare", "--initial-branch", "main"])
            .arg(&bare)
            .output()
            .expect("initialize bare repository"),
        "initialize smart-HTTP bare repository",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&bare)
            .args(["config", "http.receivepack", "true"])
            .output()
            .expect("enable authenticated push"),
        "enable authenticated smart-HTTP push",
    );
    let dependency_bare = project_root.join("team/dependency.git");
    require_success(
        &Command::new("git")
            .args(["init", "--bare", "--initial-branch", "main"])
            .arg(&dependency_bare)
            .output()
            .expect("initialize dependency repository"),
        "initialize smart-HTTP dependency repository",
    );
    let dependency_seed = temporary.path().join("dependency-seed");
    require_success(
        &Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .arg(&dependency_seed)
            .output()
            .expect("initialize dependency seed"),
        "initialize dependency seed",
    );
    fs::write(
        dependency_seed.join("dependency.txt"),
        "private submodule\n",
    )
    .expect("dependency file");
    for (key, value) in [
        ("user.name", "Credential Dependency Gate"),
        ("user.email", "dependency-gate@example.invalid"),
    ] {
        require_success(
            &Command::new("git")
                .args(["-C"])
                .arg(&dependency_seed)
                .args(["config", key, value])
                .output()
                .expect("configure dependency seed"),
            "configure dependency seed",
        );
    }
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&dependency_seed)
            .args(["add", "dependency.txt"])
            .output()
            .expect("add dependency seed"),
        "add dependency seed",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&dependency_seed)
            .args(["commit", "-m", "dependency seed"])
            .output()
            .expect("commit dependency seed"),
        "commit dependency seed",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&dependency_seed)
            .args(["push"])
            .arg(&dependency_bare)
            .arg("main")
            .output()
            .expect("push dependency seed"),
        "push dependency seed",
    );

    let server = HttpsServer::start(temporary.path(), &project_root, &server_credentials);
    let origin = format!("https://credential.test:{}", server.port);
    let seed = temporary.path().join("seed");
    require_success(
        &Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .arg(&seed)
            .output()
            .expect("initialize seed"),
        "initialize HTTPS seed",
    );
    fs::write(seed.join("private.txt"), "controlled private repository\n").expect("seed file");
    for (key, value) in [
        ("user.name", "Credential HTTPS Gate"),
        ("user.email", "https-gate@example.invalid"),
    ] {
        require_success(
            &Command::new("git")
                .args(["-C"])
                .arg(&seed)
                .args(["config", key, value])
                .output()
                .expect("configure seed"),
            "configure HTTPS seed",
        );
    }
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args(["add", "private.txt"])
            .output()
            .expect("add seed"),
        "add HTTPS seed",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args(["-c", "protocol.file.allow=always", "submodule", "add"])
            .arg(&dependency_bare)
            .arg("vendor/private")
            .output()
            .expect("add private submodule"),
        "add private submodule",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args([
                "config",
                "-f",
                ".gitmodules",
                "submodule.vendor/private.url",
                &format!("{origin}/team/dependency.git"),
            ])
            .output()
            .expect("configure private submodule URL"),
        "configure private submodule URL",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args(["add", ".gitmodules", "vendor/private"])
            .output()
            .expect("add private submodule metadata"),
        "add private submodule metadata",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args(["commit", "-m", "seed"])
            .output()
            .expect("commit seed"),
        "commit HTTPS seed",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&seed)
            .args(["push"])
            .arg(&bare)
            .arg("main")
            .output()
            .expect("push seed"),
        "push HTTPS seed",
    );

    let repository = temporary.path().join("repository");
    fs::create_dir_all(repository.join(".devcontainer")).expect("HTTPS source repository");
    fs::copy(
        concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/fixtures/credential-tls/ca.pem"
        ),
        repository.join(".devcontainer/ca.pem"),
    )
    .expect("copy controlled CA");
    fs::write(
        repository.join(".devcontainer/devcontainer.json"),
        format!(
            r#"{{
  "name": "credential-https-gate",
  "image": "buildpack-deps:trixie",
  "workspaceFolder": "/workspaces/credential-https-gate",
  "runArgs": ["--add-host", "credential.test:host-gateway", "--add-host", "denied.test:host-gateway"],
  "remoteEnv": {{"GIT_SSL_CAINFO": "/workspaces/credential-https-gate/.devcontainer/ca.pem"}},
  "onCreateCommand": ["/bin/sh", "-c", "git clone --recurse-submodules '{origin}/team/repo.git' https-checkout && test -f https-checkout/private.txt && test -f https-checkout/vendor/private/dependency.txt"]
}}
"#
        ),
    )
    .expect("HTTPS devcontainer config");
    require_success(
        &Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .arg(&repository)
            .output()
            .expect("initialize HTTPS source"),
        "initialize HTTPS source",
    );
    for (key, value) in [
        ("user.name", "Credential HTTPS Gate"),
        ("user.email", "https-gate@example.invalid"),
    ] {
        require_success(
            &Command::new("git")
                .args(["-C"])
                .arg(&repository)
                .args(["config", key, value])
                .output()
                .expect("configure HTTPS source"),
            "configure HTTPS source",
        );
    }
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["add", ".devcontainer"])
            .output()
            .expect("add HTTPS source"),
        "add HTTPS source",
    );
    require_success(
        &Command::new("git")
            .args(["-C"])
            .arg(&repository)
            .args(["commit", "-m", "HTTPS fixture"])
            .output()
            .expect("commit HTTPS source"),
        "commit HTTPS source",
    );

    let root = temporary.path().join("cdenv-root");
    let workspace = unique_name("credential-https");
    let root_text = root.to_str().expect("root text");
    let repository_text = repository.to_str().expect("repository text");
    let environment = [("HOME", home.as_path())];
    require_success(
        &run(
            &binary,
            &[
                "--root",
                root_text,
                "credentials",
                "enable",
                &workspace,
                "git-https",
                "--host",
                &origin,
            ],
            &environment,
        ),
        "stage HTTPS permission",
    );
    require_success(
        &run(
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
        ),
        "create with authenticated first-hook clone",
    );

    let first_push = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "git -C https-checkout config user.name gate && git -C https-checkout config user.email gate@example.invalid && printf first > https-checkout/first && git -C https-checkout add first && git -C https-checkout commit -m first && git -C https-checkout push origin HEAD:main",
        ],
        &environment,
    );
    require_success(&first_push, "authenticated HTTPS push");
    let rotated_credentials = format!(
        "team/repo.git\talice\t{SECOND_TOKEN}\nteam/dependency.git\tbob\t{DEPENDENCY_TOKEN}\n"
    );
    fs::write(&server_credentials, &rotated_credentials).expect("rotate server credential");
    let stale = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "git -C https-checkout fetch origin",
        ],
        &environment,
    );
    assert!(
        !stale.status.success(),
        "stale token unexpectedly authenticated"
    );
    assert!(
        !stale
            .stdout
            .windows(FIRST_TOKEN.len())
            .any(|value| value == FIRST_TOKEN.as_bytes())
    );
    assert!(
        !stale
            .stderr
            .windows(FIRST_TOKEN.len())
            .any(|value| value == FIRST_TOKEN.as_bytes())
    );
    fs::write(&host_credentials, &rotated_credentials).expect("refresh host credential");
    let rotated = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "git -C https-checkout fetch origin && git -C https-checkout/vendor/private fetch origin && printf second > https-checkout/second && git -C https-checkout add second && git -C https-checkout commit -m second && git -C https-checkout push origin HEAD:main",
        ],
        &environment,
    );
    require_success(
        &rotated,
        "stale-token recovery and rotated HTTPS fetch and push",
    );

    let helper_isolation_command = format!(
        r#"cd https-checkout && printf '%s\n' '#!/bin/sh' 'printf "native-%s\n" "$1" >> ../native-helper.log' 'cat >/dev/null' '[ "$1" = get ] && printf "username=native\npassword=native-token\n"' > .native-helper && chmod 700 .native-helper && git config --add credential.helper '!./.native-helper' && cp .git/config ../native-config.before && result=$(printf 'protocol=https\nhost=credential.test:{}\npath=team/repo.git\n\n' | git credential fill) && printf '%s\n' "$result" | grep -q '^username=alice$' && printf '%s\n\n' "$result" | git credential approve && printf '%s\n\n' "$result" | git credential reject && cmp .git/config ../native-config.before && test ! -e ../native-helper.log"#,
        server.port
    );
    let helper_isolation = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            &helper_isolation_command,
        ],
        &environment,
    );
    require_success(
        &helper_isolation,
        "lookup/approve/store native-helper isolation and configuration preservation",
    );
    let denied_url = format!("https://denied.test:{}/team/repo.git", server.port);
    let denied_command =
        format!("if git ls-remote '{denied_url}' >/dev/null 2>&1; then exit 31; fi");
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
                &denied_command,
            ],
            &environment,
        ),
        "denied additional HTTPS origin",
    );
    assert!(!contains_bytes(&root, FIRST_TOKEN.as_bytes()));
    assert!(!contains_bytes(&root, SECOND_TOKEN.as_bytes()));
    assert!(!contains_bytes(&root, DEPENDENCY_TOKEN.as_bytes()));
    assert!(!contains_bytes(&repository, FIRST_TOKEN.as_bytes()));
    assert!(!contains_bytes(&repository, SECOND_TOKEN.as_bytes()));
    assert!(!contains_bytes(&repository, DEPENDENCY_TOKEN.as_bytes()));
    require_success(
        &run(
            &binary,
            &["--root", root_text, "down", &workspace],
            &environment,
        ),
        "HTTPS fixture down",
    );
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
    let mut agent = Agent::start(temporary.path());
    let agent_socket = agent.socket.clone();
    let repository = create_repository(temporary.path(), &agent.public_key);
    let workspace = unique_name("credential-gate");
    let root_text = root.to_str().expect("root text");
    let repository_text = repository.to_str().expect("repository text");
    let socket_text = agent.socket.to_str().expect("socket text");
    let environment = [
        ("HOME", home.as_path()),
        ("SSH_AUTH_SOCK", agent_socket.as_path()),
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
    let signing = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "printf agent-signing > .credential-signing-message && ssh-keygen -Y sign -f .devcontainer/fixture-agent-key.pub -n cdenv .credential-signing-message >/dev/null 2>&1 && test -s .credential-signing-message.sig",
        ],
        &environment,
    );
    require_success(&signing, "managed agent signing");
    let allowed_signers = temporary.path().join("allowed-signers");
    fs::write(
        &allowed_signers,
        format!(
            "fixture {}",
            fs::read_to_string(&agent.public_key).expect("agent public key")
        ),
    )
    .expect("allowed signers");
    let message =
        fs::File::open(checkout.join(".credential-signing-message")).expect("signed message");
    let verified = Command::new("ssh-keygen")
        .args(["-Y", "verify", "-f"])
        .arg(&allowed_signers)
        .args(["-I", "fixture", "-n", "cdenv", "-s"])
        .arg(checkout.join(".credential-signing-message.sig"))
        .stdin(message)
        .output()
        .expect("verify managed agent signature");
    require_success(&verified, "managed agent signature verification");

    agent.restart_same_path();
    let refreshed_agent = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "ssh-add -L | grep -q '^ssh-ed25519 '",
        ],
        &environment,
    );
    require_success(
        &refreshed_agent,
        "same-path host SSH-agent restart without workspace restart",
    );
    let _ = agent.child.kill();
    let _ = agent.child.wait();
    let unavailable_agent = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            "ssh-add -L",
        ],
        &environment,
    );
    assert!(
        !unavailable_agent.status.success(),
        "unavailable host agent unexpectedly served an identity"
    );
    assert!(
        !unavailable_agent
            .stderr
            .windows(SECRET_MARKER.len())
            .any(|value| value == SECRET_MARKER.as_bytes())
    );
    agent.restart_same_path();
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
                "ssh-add -L | grep -q '^ssh-ed25519 '",
            ],
            &environment,
        ),
        "host SSH-agent recovery after backend unavailability",
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
