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

// OpenSSH joins remote argv with spaces before the remote shell parses it.
// Keep the script a single argument to /bin/sh -c, including embedded quotes.
fn remote_script(script: &str) -> String {
    format!("'{}'", script.replace('\'', "'\\''"))
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

// Uses the production SSH implementation as a controlled Git origin; the client
// remains the container's real Git/OpenSSH using the forwarded host agent.
#[cfg(target_os = "linux")]
struct GitSshServer {
    child: Child,
    port: u16,
    public_key: String,
    repository: PathBuf,
}

#[cfg(target_os = "linux")]
impl GitSshServer {
    fn start(root: &Path, source: &Path, authorized_key: &Path) -> Self {
        let host_key = root.join("git-ssh-host-key");
        require_success(
            &run(
                Path::new("ssh-keygen"),
                &[
                    "-q",
                    "-t",
                    "ed25519",
                    "-N",
                    "",
                    "-f",
                    host_key.to_str().expect("host key"),
                ],
                &[],
            ),
            "generate Git SSH host key",
        );
        let repository = root.join("origin.git");
        require_success(
            &run(
                Path::new("git"),
                &[
                    "clone",
                    "--bare",
                    source.to_str().expect("source"),
                    repository.to_str().expect("origin"),
                ],
                &[],
            ),
            "create Git SSH origin",
        );
        let environment =
            cdenv_agent::capture_environment(&cdenv_agent::EnvironmentCaptureRequest {
                generation: "credential-git-ssh".to_owned(),
                state_directory: root.join("git-ssh-state").display().to_string(),
                probe: cdenv_agent::EnvironmentProbe::None,
                remote_environment: std::collections::BTreeMap::new(),
            })
            .expect("Git SSH environment");
        let key = root.join("git-ssh-authorized-key");
        fs::copy(authorized_key, &key).expect("authorized key");
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).expect("key mode");
        let ready = root.join("git-ssh-ready");
        let child = Command::new(env!("CARGO_BIN_EXE_credential-ssh-server"))
            .arg(&host_key)
            .arg(&key)
            .arg(environment.snapshot_path)
            .arg(root)
            .arg(&ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("Git SSH fixture");
        let mut server = Self {
            child,
            port: 0,
            public_key: fs::read_to_string(host_key.with_extension("pub"))
                .expect("host public key"),
            repository,
        };
        wait_for_path(&ready, "Git SSH fixture readiness");
        server.port = fs::read_to_string(ready)
            .expect("ready port")
            .parse()
            .expect("port");
        server
    }
}

#[cfg(target_os = "linux")]
impl Drop for GitSshServer {
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
  "runArgs": ["--add-host", "credential.test:host-gateway"],
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
            &remote_script(
                "git -C https-checkout config user.name gate && git -C https-checkout config user.email gate@example.invalid && printf first > https-checkout/first && git -C https-checkout add first && git -C https-checkout commit -m first && git -C https-checkout push origin HEAD:main",
            ),
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
            &remote_script("git -C https-checkout fetch origin"),
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
            &remote_script(
                "git -C https-checkout fetch origin && git -C https-checkout/vendor/private fetch origin && printf second > https-checkout/second && git -C https-checkout add second && git -C https-checkout commit -m second && git -C https-checkout push origin HEAD:main",
            ),
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
            &remote_script(&helper_isolation_command),
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
                &remote_script(&denied_command),
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
    #[cfg(target_os = "linux")]
    let git_ssh = GitSshServer::start(temporary.path(), &repository, &agent.public_key);
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
            &remote_script("test -S \"$SSH_AUTH_SOCK\" && printf ssh-managed"),
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
            &remote_script(
                "printf agent-signing > .credential-signing-message && ssh-keygen -Y sign -f .devcontainer/fixture-agent-key.pub -n cdenv .credential-signing-message >/dev/null 2>&1 && test -s .credential-signing-message.sig",
            ),
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

    #[cfg(target_os = "linux")]
    {
        let script = format!(
            r#"set -eu
mkdir -p .credential-git-ssh
printf 'credential-origin %s\n' '{host_key}' > .credential-git-ssh/known_hosts
export GIT_SSH_COMMAND="ssh -o BatchMode=yes -o ConnectTimeout=5 -o StrictHostKeyChecking=yes -o GlobalKnownHostsFile=/dev/null -o UserKnownHostsFile=$PWD/.credential-git-ssh/known_hosts -o HostKeyAlias=credential-origin"
git clone 'ssh://cdenv@credential.test:{port}{repository}' .credential-git-ssh/clone
git -C .credential-git-ssh/clone -c user.name=Gate -c user.email=gate@example.invalid commit --allow-empty -m pushed
git -C .credential-git-ssh/clone push origin HEAD:refs/heads/credential-pushed
git -C .credential-git-ssh/clone fetch origin credential-pushed
: > .credential-git-ssh/known_hosts
if git -C .credential-git-ssh/clone ls-remote origin > .credential-git-ssh/rejected 2>&1; then exit 1; fi
grep -q 'Host key verification failed' .credential-git-ssh/rejected
printf 'credential-origin %s\n' "$(cat .devcontainer/fixture-agent-key.pub)" > .credential-git-ssh/known_hosts
if git -C .credential-git-ssh/clone ls-remote origin > .credential-git-ssh/rejected 2>&1; then exit 1; fi
grep -q 'Host key verification failed' .credential-git-ssh/rejected
"#,
            host_key = git_ssh.public_key.trim(),
            port = git_ssh.port,
            repository = git_ssh.repository.display(),
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
                    &remote_script(&script),
                ],
                &environment,
            ),
            "Git-over-SSH clone/fetch/push and strict unknown/changed host-key rejection",
        );
    }

    // An available agent with no identities is distinct from a missing backend.
    // Exercise the real forwarded protocol, then restore the same agent without
    // restarting the workspace or bridge.
    require_success(
        &run(Path::new("ssh-add"), &["-D"], &environment),
        "remove controlled host-agent identities",
    );
    let empty_agent = run(
        &binary,
        &[
            "--root",
            root_text,
            "ssh",
            &workspace,
            "--",
            "/bin/sh",
            "-c",
            &remote_script("LC_ALL=C ssh-add -L"),
        ],
        &environment,
    );
    assert_eq!(
        empty_agent.status.code(),
        Some(1),
        "empty agent exit status"
    );
    assert!(
        String::from_utf8_lossy(&empty_agent.stdout).contains("The agent has no identities."),
        "empty agent did not return the authenticated empty identity response"
    );
    require_success(
        &run(
            Path::new("ssh-add"),
            &[agent
                .public_key
                .with_extension("")
                .to_str()
                .expect("fixture key path")],
            &environment,
        ),
        "restore controlled host-agent identity",
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
                &remote_script("ssh-add -L | grep -q '^ssh-ed25519 '"),
            ],
            &environment,
        ),
        "host SSH-agent recovery after empty identity response",
    );

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
            &remote_script("ssh-add -L | grep -q '^ssh-ed25519 '"),
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
            &remote_script("ssh-add -L"),
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
                &remote_script("ssh-add -L | grep -q '^ssh-ed25519 '"),
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
                    &remote_script("test -S \"$SSH_AUTH_SOCK\" && sleep 0.2"),
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
                &remote_script("test -S \"$SSH_AUTH_SOCK\""),
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
            &remote_script("test -S \"$SSH_AUTH_SOCK\" || exit 20; printf ready > .credential-old-client; sleep 1; printf survived > .credential-old-client-survived"),
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
            &remote_script("test -z \"${SSH_AUTH_SOCK+x}\""),
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

const ACTIVE_LIFECYCLE_SCRIPT: &str = "test -S \"$SSH_AUTH_SOCK\" || exit 20; trap 'printf stopped >> .active-stops; exit 0' TERM; printf x >> .active-starts; touch .active-ready; while :; do sleep 1; done";

struct LifecycleWorkflow {
    agent: Agent,
    binary: PathBuf,
    root: PathBuf,
    home: PathBuf,
    workspace: String,
    checkout: PathBuf,
    _temporary: tempfile::TempDir,
}

impl LifecycleWorkflow {
    fn create() -> Self {
        let binary = PathBuf::from(
            std::env::var_os("CDENV_CREDENTIAL_TEST_BINARY")
                .expect("xtask must provide the packaged cdenv binary"),
        );
        require_success(
            &run(&binary, &["__validate-artifacts"], &[]),
            "artifact validation",
        );
        let shared = std::env::var_os("CDENV_INTEGRATION_SHARED_TMP")
            .map_or_else(std::env::temp_dir, PathBuf::from);
        let temporary = tempfile::Builder::new()
            .prefix("cdenv-lifecycle-")
            .tempdir_in(shared)
            .expect("temporary workflow");
        let root = temporary.path().join("root");
        let home = temporary.path().join("home");
        fs::create_dir(&home).expect("home");
        fs::set_permissions(&home, fs::Permissions::from_mode(0o700)).expect("home mode");
        let agent = Agent::start(temporary.path());
        let repository = create_repository(temporary.path(), &agent.public_key);
        let config_path = repository.join(".devcontainer/devcontainer.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&fs::read(&config_path).expect("config")).expect("JSON");
        config["postStartCommand"] = serde_json::json!(["/bin/sh", "-c", ACTIVE_LIFECYCLE_SCRIPT]);
        fs::write(
            &config_path,
            serde_json::to_vec(&config).expect("config JSON"),
        )
        .expect("config update");
        require_success(
            &Command::new("git")
                .arg("-C")
                .arg(&repository)
                .args(["commit", "-am", "active lifecycle fixture"])
                .output()
                .expect("commit"),
            "fixture commit",
        );
        let workspace = unique_name("lifecycle");
        let checkout = root
            .join("workspaces")
            .join(&workspace)
            .join("checkout")
            .join(&workspace);
        let workflow = Self {
            agent,
            binary,
            root,
            home,
            workspace,
            checkout,
            _temporary: temporary,
        };
        require_success(
            &workflow.invoke(&[
                "credentials",
                "enable",
                &workflow.workspace,
                "ssh-agent",
                "--socket",
                workflow.agent.socket.to_str().expect("socket"),
            ]),
            "enable credentials",
        );
        require_success(
            &workflow.invoke(&[
                "--no-modify-ssh-config",
                "create",
                repository.to_str().expect("repository"),
                "--name",
                &workflow.workspace,
            ]),
            "create active lifecycle",
        );
        workflow.wait_for_hook();
        workflow
    }

    fn invoke(&self, arguments: &[&str]) -> Output {
        let mut args = vec!["--root", self.root.to_str().expect("root")];
        args.extend_from_slice(arguments);
        run(
            &self.binary,
            &args,
            &[
                ("HOME", self.home.as_path()),
                ("SSH_AUTH_SOCK", self.agent.socket.as_path()),
            ],
        )
    }

    fn command(&self, command: &str) -> Output {
        self.invoke(&[command, &self.workspace])
    }

    fn workspace_root(&self) -> PathBuf {
        self.root.join("workspaces").join(&self.workspace)
    }

    fn record(&self) -> serde_json::Value {
        serde_json::from_slice(
            &fs::read(self.workspace_root().join("runtime/lifecycle-exec.json"))
                .expect("execution record"),
        )
        .expect("record JSON")
    }

    fn wait_for_hook(&self) {
        wait_for_path(&self.checkout.join(".active-ready"), "active hook");
    }

    fn clear_hook_readiness(&self) {
        fs::remove_file(self.checkout.join(".active-ready")).expect("clear readiness");
    }

    fn container_exec(&self, record: &serde_json::Value, arguments: &[&str]) -> Output {
        assert_eq!(
            record["identity"]["workspace"].as_str(),
            Some(self.workspace.as_str())
        );
        let user = format!("{}:{}", record["request"]["uid"], record["request"]["gid"]);
        let mut args = vec![
            "exec",
            "--user",
            &user,
            record["identity"]["container"].as_str().expect("container"),
        ];
        args.extend_from_slice(arguments);
        run(Path::new("docker"), &args, &[])
    }

    fn checkpoint(&self, record: &serde_json::Value) -> serde_json::Value {
        let path = format!(
            "{}/.cdenv-run-{}.json",
            record["request"]["stateDirectory"]
                .as_str()
                .expect("state directory"),
            record["request"]["ownership"]["execution"]
                .as_str()
                .expect("execution")
        );
        let output = self.container_exec(record, &["cat", &path]);
        require_success(&output, "read owned checkpoint");
        let state: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("checkpoint JSON");
        assert_eq!(state["plan"], record["request"]);
        state
    }

    fn assert_recovery_required(&self) {
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(self.workspace_root().join("state.json")).expect("workspace state"),
        )
        .expect("state JSON");
        assert_eq!(state["active"]["lifecycle"]["indeterminate"], true);
        assert!(
            state["lastError"]
                .as_str()
                .is_some_and(|error| !error.is_empty())
        );
        let up = self.command("up");
        assert!(
            !up.status.success(),
            "uncertain work must not retry through up"
        );
        assert!(String::from_utf8_lossy(&up.stderr).contains("rebuild"));
    }

    fn assert_unverifiable_rebuild_blocked(&self, previous: &serde_json::Value) {
        let rebuild = self.command("rebuild");
        assert!(
            !rebuild.status.success(),
            "unverifiable active work must block replacement"
        );
        assert!(
            String::from_utf8_lossy(&rebuild.stderr)
                .contains("background lifecycle completion or cancellation could not be verified"),
            "unexpected rebuild failure: {}",
            String::from_utf8_lossy(&rebuild.stderr)
        );
        assert_eq!(
            self.record(),
            *previous,
            "failed quiescing must preserve the exact old execution"
        );
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(self.workspace_root().join("state.json")).expect("state"),
        )
        .expect("state JSON");
        assert_eq!(
            state["active"]["containerId"],
            previous["identity"]["container"]
        );
    }

    fn recover_after_uncertainty(&self, previous: &serde_json::Value) {
        self.clear_hook_readiness();
        for _ in 0..2 {
            let down = self.command("down");
            assert!(
                !down.status.success(),
                "uncertain down must report an incomplete shutdown"
            );
            self.assert_recovery_required();
            assert_eq!(
                fs::read(self.checkout.join(".active-starts")).expect("starts"),
                b"x"
            );
        }
        require_success(&self.command("rebuild"), "rebuild uncertain execution");
        self.wait_for_hook();
        let replacement = self.record();
        assert_ne!(
            previous["identity"]["generation"],
            replacement["identity"]["generation"]
        );
        assert_ne!(
            previous["request"]["ownership"]["execution"],
            replacement["request"]["ownership"]["execution"]
        );
        assert_eq!(
            fs::read(self.checkout.join(".active-starts")).expect("starts"),
            b"xx"
        );
        require_success(&self.command("down"), "down recovered generation");
        assert!(!contains_marker(&self.root));
    }
}

#[test]
fn packaged_active_lifecycle_cancels_on_down_and_rebuild_and_repeats_post_start() {
    let workflow = LifecycleWorkflow::create();
    let first = workflow.record();
    workflow.clear_hook_readiness();
    require_success(&workflow.command("down"), "cancel active hook on down");
    assert_eq!(
        fs::read(workflow.checkout.join(".active-stops")).expect("TERM acknowledgement"),
        b"stopped"
    );
    require_success(&workflow.command("up"), "up after cancellation");
    workflow.wait_for_hook();
    let second = workflow.record();
    assert_eq!(
        first["identity"]["generation"],
        second["identity"]["generation"]
    );
    assert_ne!(first["execId"], second["execId"]);
    assert_ne!(
        first["request"]["ownership"]["execution"],
        second["request"]["ownership"]["execution"]
    );
    assert_eq!(
        fs::read(workflow.checkout.join(".active-starts")).expect("starts"),
        b"xx"
    );
    workflow.clear_hook_readiness();
    require_success(&workflow.command("rebuild"), "rebuild cancels active hook");
    workflow.wait_for_hook();
    let replacement = workflow.record();
    assert_ne!(
        second["identity"]["generation"],
        replacement["identity"]["generation"]
    );
    assert_eq!(
        fs::read(workflow.checkout.join(".active-stops")).expect("stops"),
        b"stoppedstopped"
    );
    require_success(&workflow.command("down"), "final active hook down");
    assert_eq!(
        fs::read(workflow.checkout.join(".active-starts")).expect("starts"),
        b"xxx"
    );
    assert!(!contains_marker(&workflow.root));
}

#[test]
fn packaged_lifecycle_unconfirmed_start_remains_indeterminate_until_rebuild() {
    let workflow = LifecycleWorkflow::create();
    let mut record = workflow.record();
    assert_eq!(record["started"], true);
    // Simulate the real crash window after Docker starts the runner but before
    // the host durably confirms that start. The live work must remain unverified.
    record["started"] = serde_json::json!(false);
    fs::write(
        workflow
            .workspace_root()
            .join("runtime/lifecycle-exec.json"),
        serde_json::to_vec(&record).expect("record JSON"),
    )
    .expect("unconfirmed start");
    workflow.assert_unverifiable_rebuild_blocked(&record);
    workflow.recover_after_uncertainty(&record);
}

#[test]
fn packaged_lifecycle_runner_crash_preserves_unrelated_process_and_requires_rebuild() {
    let workflow = LifecycleWorkflow::create();
    let record = workflow.record();
    let state = workflow.checkpoint(&record);
    assert_eq!(state["phase"], "running");
    // This process is created by a separate Docker Exec, outside the hook's
    // owned process group. Cancellation must never use it as a fallback target.
    let witness = workflow.checkout.join(".unrelated-pid");
    let remote_witness = "/workspaces/credential-release-gate/.unrelated-pid";
    let container = record["identity"]["container"].as_str().expect("container");
    require_success(
        &run(
            Path::new("docker"),
            &[
                "exec",
                "--detach",
                container,
                "/bin/sh",
                "-c",
                "echo $$ > /workspaces/credential-release-gate/.unrelated-pid.tmp; mv /workspaces/credential-release-gate/.unrelated-pid.tmp /workspaces/credential-release-gate/.unrelated-pid; exec sleep 600",
            ],
            &[],
        ),
        "start unrelated process",
    );
    wait_for_path(&witness, "unrelated process");
    let unrelated_pid = fs::read_to_string(&witness).expect("witness PID");
    let unrelated_pid = unrelated_pid.trim();
    let runner_pid = state["runnerPid"]
        .as_u64()
        .expect("live fixture runner PID")
        .to_string();
    require_success(
        &workflow.container_exec(&record, &["/bin/kill", "-KILL", &runner_pid]),
        "crash owned runner",
    );
    let request = serde_json::to_string(&record["request"]).expect("request");
    // The provisioned executable path is authoritative, not guessed from an image layout.
    let workspace_state: serde_json::Value = serde_json::from_slice(
        &fs::read(workflow.workspace_root().join("state.json")).expect("state"),
    )
    .expect("state JSON");
    let agent = workspace_state["active"]["provisioned"]["agentPath"]
        .as_str()
        .expect("provisioned agent path");
    let acknowledgement =
        workflow.container_exec(&record, &[agent, "lifecycle-cancel-owned", &request, "0"]);
    require_success(&acknowledgement, "inspect crashed cancellation");
    let inspection: serde_json::Value =
        serde_json::from_slice(&acknowledgement.stdout).expect("inspection");
    assert_eq!(inspection["runnerActive"], false);
    assert_eq!(inspection["state"]["phase"], "failed");
    workflow.assert_unverifiable_rebuild_blocked(&record);
    require_success(
        &workflow.container_exec(&record, &["/bin/kill", "-0", unrelated_pid]),
        "unrelated process survived",
    );
    require_success(
        &workflow.container_exec(&record, &["test", "-f", remote_witness]),
        "unrelated marker survived",
    );
    workflow.recover_after_uncertainty(&record);
}
