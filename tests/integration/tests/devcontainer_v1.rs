//! Black-box release contract for the `cdenv-devcontainer-v1` profile.

#![cfg(feature = "devcontainer-v1")]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cdenv_devcontainer::{ConfigPath, ParseLimits, parse_jsonc, validate_profile};
use tempfile::TempDir;

const FIXTURE_NAMES: &[&str] = &[
    "basic-debian",
    "basic-alpine",
    "custom-remote-user",
    "compose-primary-and-service",
    "image-metadata-merge",
    "public-oci-features",
    "https-and-local-features",
    "lockfile-stale",
    "forwarding-server",
    "failing-and-background-lifecycle",
    "build-create-runtime-drift",
    "read-only-best-effort",
];

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures")
}

fn command_output(program: &str, arguments: &[&str], cwd: &Path) -> Output {
    let output = Command::new(program)
        .args(arguments)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("cannot start {program}: {error}"));
    assert!(
        output.status.success(),
        "{program} {arguments:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn directory_snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(root: &Path, directory: &Path, files: &mut BTreeMap<PathBuf, Vec<u8>>) {
        let mut entries = fs::read_dir(directory)
            .expect("fixture directory")
            .collect::<Result<Vec<_>, _>>()
            .expect("fixture entries");
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).expect("fixture metadata");
            if metadata.is_dir() {
                visit(root, &path, files);
            } else if metadata.is_file() {
                files.insert(
                    path.strip_prefix(root)
                        .expect("relative fixture")
                        .to_path_buf(),
                    fs::read(path).expect("fixture bytes"),
                );
            }
        }
    }
    let mut files = BTreeMap::new();
    visit(root, root, &mut files);
    files
}

fn version_components(value: &str) -> (u32, u32, u32) {
    let value = value.trim_start_matches(['v', 'V']);
    let mut components = value.split('.').map(|part| {
        part.chars()
            .take_while(char::is_ascii_digit)
            .collect::<String>()
            .parse::<u32>()
            .expect("numeric version component")
    });
    (
        components.next().expect("major version"),
        components.next().expect("minor version"),
        components.next().unwrap_or(0),
    )
}

#[test]
fn dependency_versions_satisfy_profile_baseline() {
    let docker = command_output(
        "docker",
        &[
            "version",
            "--format",
            "{{.Client.Version}} {{.Server.Version}} {{.Server.APIVersion}}",
        ],
        Path::new(env!("CARGO_MANIFEST_DIR")),
    );
    let text = String::from_utf8(docker.stdout).expect("Docker version text");
    let fields = text.split_whitespace().collect::<Vec<_>>();
    assert_eq!(fields.len(), 3);
    assert!(version_components(fields[0]) >= (29, 7, 1));
    assert!(version_components(fields[1]) >= (29, 6, 2));
    assert!(version_components(fields[2]) >= (1, 55, 0));
    let compose = command_output(
        "docker",
        &["compose", "version", "--short"],
        Path::new(env!("CARGO_MANIFEST_DIR")),
    );
    assert!(version_components(String::from_utf8_lossy(&compose.stdout).trim()) >= (5, 3, 1));
}

#[test]
fn support_matrix_has_complete_traceability_and_fixtures() {
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(fixtures().join("devcontainer-v1-coverage.json")).expect("coverage manifest"),
    )
    .expect("coverage JSON");
    let rows = value["matrix"].as_array().expect("matrix rows");
    let security = value["security"].as_array().expect("security rows");
    assert_eq!(
        (rows.len(), security.len(), FIXTURE_NAMES.len()),
        (10, 10, 12)
    );
    for name in FIXTURE_NAMES {
        assert!(fixtures().join(name).is_dir(), "missing fixture {name}");
    }
}

#[test]
fn positive_fixture_profiles_parse_and_validate() {
    for name in FIXTURE_NAMES {
        let bytes = fs::read(fixtures().join(name).join("devcontainer.json"))
            .unwrap_or_else(|error| panic!("cannot read fixture {name}: {error}"));
        let path =
            ConfigPath::parse(&format!("{name}/devcontainer.json")).expect("fixture config path");
        let document = parse_jsonc(&path, &bytes, ParseLimits::default())
            .unwrap_or_else(|error| panic!("fixture {name} did not parse: {error}"));
        validate_profile(&document)
            .unwrap_or_else(|error| panic!("fixture {name} failed profile validation: {error}"));
    }
}

#[test]
fn hostile_and_unsupported_profiles_fail_closed() {
    let path = ConfigPath::parse("hostile/devcontainer.json").expect("config path");
    let malformed = parse_jsonc(
        &path,
        br#"{"image":"debian:13-slim",/* unterminated"#,
        ParseLimits::default(),
    );
    assert!(malformed.is_err());
    let unknown = parse_jsonc(
        &path,
        br#"{"image":"debian:13-slim","privilegedEditorBackdoor":true}"#,
        ParseLimits::default(),
    )
    .expect("JSON syntax");
    assert!(validate_profile(&unknown).is_err());
}

#[test]
fn feature_security_contract_is_repository_owned() {
    let value: serde_json::Value = serde_json::from_slice(
        &fs::read(fixtures().join("devcontainer-v1-coverage.json")).expect("coverage manifest"),
    )
    .expect("coverage JSON");
    let controls = value["security"]
        .as_array()
        .expect("security controls")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<BTreeSet<_>>();
    for required in [
        "oci-bearer-token-on-blob",
        "cross-origin-redirect-strips-credentials",
        "exactly-one-feature-layer",
        "descriptor-size-and-digest-verification",
        "published-feature-resource-limits",
        "cache-reuse-and-retention",
        "frozen-offline-lock",
        "dockerignore-context-semantics",
    ] {
        assert!(
            controls.contains(required),
            "missing security control {required}"
        );
    }
}

#[test]
fn pure_profile_gate_does_not_mutate_fixture_checkout() {
    let root = fixtures();
    let before = directory_snapshot(&root);
    for name in FIXTURE_NAMES {
        let bytes = fs::read(root.join(name).join("devcontainer.json")).expect("fixture config");
        let path =
            ConfigPath::parse(&format!("{name}/devcontainer.json")).expect("fixture config path");
        let document = parse_jsonc(&path, &bytes, ParseLimits::default()).expect("fixture parse");
        let _profile = validate_profile(&document).expect("fixture validation");
    }
    assert_eq!(directory_snapshot(&root), before);
}

#[test]
fn static_editor_server_simulator_is_binary_clean_and_editor_independent() {
    let executable = env!("CARGO_BIN_EXE_editor-server-simulator");
    let mut child = Command::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start static simulator");
    let stdout = child.stdout.take().expect("simulator stdout");
    let mut reader = BufReader::new(stdout);
    let mut address = String::new();
    reader.read_line(&mut address).expect("simulator address");
    let mut stream = TcpStream::connect(address.trim()).expect("simulator connection");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("read timeout");
    let mut greeting = [0_u8; 25];
    stream
        .read_exact(&mut greeting)
        .expect("simulator greeting");
    assert_eq!(&greeting, b"CDENV_EDITOR_SIMULATOR/1\n");
    stream.write_all(b"PING\n").expect("simulator request");
    let mut response = [0_u8; 5];
    stream
        .read_exact(&mut response)
        .expect("simulator response");
    assert_eq!(&response, b"PONG\n");
    assert!(child.wait().expect("simulator exit").success());
}

struct ComposeCleanup {
    project: String,
    directory: PathBuf,
    unrelated: String,
}

impl Drop for ComposeCleanup {
    fn drop(&mut self) {
        let _ = Command::new("docker")
            .args([
                "compose",
                "--project-name",
                &self.project,
                "--file",
                "compose.yaml",
                "down",
                "--volumes",
                "--remove-orphans",
            ])
            .current_dir(&self.directory)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = Command::new("docker")
            .args(["rm", "--force", &self.unrelated])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn unique_name(prefix: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock")
        .as_nanos();
    format!("{prefix}-{}-{nanos}", std::process::id())
}

fn compose_primary_id(directory: &Path, project: &str) -> String {
    let output = command_output(
        "docker",
        &[
            "compose",
            "--project-name",
            project,
            "--file",
            "compose.yaml",
            "ps",
            "--quiet",
            "app",
        ],
        directory,
    );
    String::from_utf8(output.stdout)
        .expect("container ID text")
        .trim()
        .to_owned()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "the black-box replacement sequence stays explicit and auditable"
)]
fn docker_and_compose_replacement_preserve_checkout_and_named_volume() {
    let temporary = TempDir::new().expect("temporary fixture");
    let project = unique_name("cdenv-profile");
    let unrelated = unique_name("cdenv-unrelated");
    let _cleanup = ComposeCleanup {
        project: project.clone(),
        directory: temporary.path().to_path_buf(),
        unrelated: unrelated.clone(),
    };
    command_output("docker", &["pull", "debian:13-slim"], temporary.path());
    command_output("docker", &["pull", "alpine:3.22"], temporary.path());
    fs::write(temporary.path().join("tracked.txt"), "tracked-change\n").expect("tracked fixture");
    fs::write(temporary.path().join("untracked.txt"), "untracked-change\n")
        .expect("untracked fixture");
    let checkout_before = directory_snapshot(temporary.path());
    fs::write(
        temporary.path().join("compose.yaml"),
        "services:\n  app:\n    image: debian:13-slim\n    command: [\"sleep\", \"infinity\"]\n    volumes: [\"profile-data:/data\"]\n  db:\n    image: alpine:3.22\n    command: [\"sleep\", \"infinity\"]\n  orphan:\n    image: alpine:3.22\n    command: [\"sleep\", \"infinity\"]\nvolumes:\n  profile-data:\n",
    )
    .expect("initial Compose fixture");
    command_output(
        "docker",
        &[
            "compose",
            "--project-name",
            &project,
            "--file",
            "compose.yaml",
            "up",
            "--detach",
        ],
        temporary.path(),
    );
    let old_primary = compose_primary_id(temporary.path(), &project);
    command_output(
        "docker",
        &[
            "exec",
            &old_primary,
            "sh",
            "-c",
            "printf stable > /data/value",
        ],
        temporary.path(),
    );
    command_output(
        "docker",
        &[
            "run",
            "--detach",
            "--name",
            &unrelated,
            "alpine:3.22",
            "sleep",
            "infinity",
        ],
        temporary.path(),
    );
    fs::write(
        temporary.path().join("compose.yaml"),
        "services:\n  app:\n    image: debian:13-slim\n    command: [\"sleep\", \"infinity\"]\n    volumes: [\"profile-data:/data\"]\n  db:\n    image: alpine:3.22\n    command: [\"sleep\", \"infinity\"]\nvolumes:\n  profile-data:\n",
    )
    .expect("desired Compose fixture");
    command_output(
        "docker",
        &[
            "compose",
            "--project-name",
            &project,
            "--file",
            "compose.yaml",
            "up",
            "--detach",
            "--force-recreate",
            "--no-build",
            "--pull",
            "never",
        ],
        temporary.path(),
    );
    let new_primary = compose_primary_id(temporary.path(), &project);
    assert_ne!(new_primary, old_primary);
    let value = command_output(
        "docker",
        &["exec", &new_primary, "cat", "/data/value"],
        temporary.path(),
    );
    assert_eq!(value.stdout, b"stable");
    command_output(
        "docker",
        &[
            "compose",
            "--project-name",
            &project,
            "--file",
            "compose.yaml",
            "up",
            "--detach",
            "--no-build",
            "--pull",
            "never",
            "--remove-orphans",
        ],
        temporary.path(),
    );
    let unrelated_state = command_output(
        "docker",
        &["inspect", "--format", "{{.State.Running}}", &unrelated],
        temporary.path(),
    );
    assert_eq!(unrelated_state.stdout, b"true\n");
    assert_eq!(
        fs::read(temporary.path().join("tracked.txt")).expect("tracked bytes"),
        b"tracked-change\n"
    );
    assert_eq!(
        fs::read(temporary.path().join("untracked.txt")).expect("untracked bytes"),
        b"untracked-change\n"
    );
    assert!(checkout_before.contains_key(Path::new("tracked.txt")));
}
