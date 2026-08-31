//! Fake-executable contracts for the specification-facing Docker CLI adapter.

#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cdenv_cli::{
    CancellationToken, DockerBuildContext, DockerBuildRequest, DockerCliAdapter,
    DockerCreateRequest, DockerEndpoint, DockerEnvironment, DockerResourceIdentity,
    DockerSocketProbe, DockerfileInput, ProcessRunner, build_arguments,
};
use cdenv_core::{GenerationId, InstallationId, ProfileId, WorkspaceName};
use cdenv_devcontainer::{
    BuildPlan, ConfigPath, ContainerPath, DockerOptionPlanningInputs, GpuAccessIntent,
    HostSubstitutionInputs, ParseLimits, RawProfile, RuntimePlanningInputs, ScenarioMetadata,
    StableIdentityLabels, merge_image_metadata, parse_jsonc, plan_docker_options, plan_ports,
    plan_runtime, validate_profile,
};
use tempfile::TempDir;

const IMAGE_ID: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const CONTAINER_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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

struct Fixture {
    temporary: TempDir,
    executable: PathBuf,
    record: PathBuf,
    endpoint: DockerEndpoint,
}

impl Fixture {
    fn new() -> Self {
        let temporary = tempfile::tempdir().expect("temporary fixture");
        let executable = temporary.path().join("fake-docker");
        let record = temporary.path().join("record");
        fs::write(
            &executable,
            format!(
                r#"#!/bin/sh
set -eu
printf 'DOCKER_HOST=%s\nDOCKER_BUILDKIT=%s\n' "${{DOCKER_HOST-}}" "${{DOCKER_BUILDKIT-}}" > "${{RECORD}}.env"
printf '%s\n' "$@" > "${{RECORD}}.args"
case "$1" in
  build)
    iid=''
    while [ "$#" -gt 0 ]; do
      if [ "$1" = '--iidfile' ]; then shift; iid=$1; fi
      shift
    done
    printf '%s\n' '{IMAGE_ID}' > "$iid"
    ;;
  create) printf '%s\n' '{CONTAINER_ID}' ;;
esac
"#
            ),
        )
        .expect("fake Docker executable");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
            .expect("executable mode");
        let socket = temporary.path().join("docker.sock");
        let host = OsString::from(format!("unix://{}", socket.display()));
        let endpoint =
            DockerEndpoint::resolve_with_probe(&Environment { host }, &ExactSocket(socket))
                .expect("test endpoint");
        Self {
            temporary,
            executable,
            record,
            endpoint,
        }
    }

    fn adapter(&self) -> DockerCliAdapter {
        DockerCliAdapter::new(
            self.executable.clone(),
            self.endpoint.clone(),
            ProcessRunner::new(self.temporary.path().join("logs")),
            vec![(OsString::from("RECORD"), self.record.as_os_str().to_owned())],
            self.temporary.path().join("managed-tmp"),
        )
    }

    fn arguments(&self) -> Vec<String> {
        fs::read_to_string(self.record.with_extension("args"))
            .expect("recorded arguments")
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn environment(&self) -> String {
        fs::read_to_string(self.record.with_extension("env")).expect("recorded environment")
    }
}

fn profile(source: &str) -> RawProfile {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("config path");
    let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSONC");
    validate_profile(&document).expect("profile")
}

fn identity<'a>(
    installation: &'a InstallationId,
    workspace: &'a WorkspaceName,
    profile: &'a ProfileId,
) -> DockerResourceIdentity<'a> {
    DockerResourceIdentity {
        installation,
        workspace,
        generation: GenerationId::new(2).expect("generation"),
        profile,
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the exact-argv contract stays self-contained and reviewable"
)]
async fn fake_cli_receives_exact_pull_and_create_arguments_with_owned_settings() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let checkout = fixture.temporary.path().join("checkout/workspace");
    fs::create_dir_all(&checkout).expect("checkout");
    let stable_labels = StableIdentityLabels::new("installation", "workspace");
    let local_env = BTreeMap::new();
    let owned = [ContainerPath::parse("/usr/local/share/cdenv").expect("owned path")];
    let profile = profile(
        r#"{
          "image":"example.invalid/base:latest",
          "runArgs":["--read-only","--device=/dev/kvm"],
          "appPort":3000,
          "containerEnv":{"MODE":"development"},
          "init":true,
          "capAdd":["SYS_PTRACE"],
          "securityOpt":["seccomp=unconfined"]
        }"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("metadata");
    let runtime = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: checkout.to_str().expect("UTF-8 checkout"),
            local_env: &local_env,
            identity_labels: &stable_labels,
            scenario_metadata: ScenarioMetadata::default(),
            cdenv_owned_targets: &owned,
            host_user: None,
        },
    )
    .expect("runtime");
    let substitutions = HostSubstitutionInputs {
        local_workspace_folder: checkout.to_str().expect("UTF-8 checkout"),
        container_workspace_folder: "/workspaces/workspace",
        local_env: &local_env,
        identity_labels: &stable_labels,
    };
    let docker = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: ".devcontainer",
            substitutions: &substitutions,
            cdenv_owned_targets: &owned,
        },
    )
    .expect("Docker options");
    let ports = plan_ports(&profile, &effective).expect("ports");
    let installation = InstallationId::parse("installation").expect("installation");
    let workspace = WorkspaceName::parse("workspace").expect("workspace");
    let profile_id = ProfileId::parse("cdenv-devcontainer-v1").expect("profile ID");
    let command = ["sleep".to_owned(), "infinity".to_owned()];

    adapter
        .pull(
            "example.invalid/base:latest",
            &checkout,
            &CancellationToken::default(),
        )
        .await
        .expect("pull claim");
    assert_eq!(fixture.arguments(), ["pull", "example.invalid/base:latest"]);

    let claim = adapter
        .create(
            &DockerCreateRequest {
                name: "cdenv-workspace-2",
                image: "example.invalid/base:latest",
                identity: identity(&installation, &workspace, &profile_id),
                runtime: &runtime,
                options: &docker.create,
                ports: &ports,
                gpu_access: GpuAccessIntent::Requested,
                command: &command,
                cdenv_owned_targets: &owned,
            },
            &checkout,
            &CancellationToken::default(),
        )
        .await
        .expect("create claim");

    assert_eq!(claim.container_id.as_str(), CONTAINER_ID);
    assert_eq!(
        fixture.arguments(),
        [
            "create".to_owned(),
            "--read-only".to_owned(),
            "--device=/dev/kvm".to_owned(),
            "--name".to_owned(),
            "cdenv-workspace-2".to_owned(),
            "--label".to_owned(),
            "cdenv.installation=installation".to_owned(),
            "--label".to_owned(),
            "cdenv.workspace=workspace".to_owned(),
            "--label".to_owned(),
            "cdenv.generation=2".to_owned(),
            "--label".to_owned(),
            "cdenv.profile=cdenv-devcontainer-v1".to_owned(),
            "--publish".to_owned(),
            "127.0.0.1:3000:3000".to_owned(),
            "--gpus".to_owned(),
            "all".to_owned(),
            "--mount".to_owned(),
            format!(
                "type=bind,source={},target=/workspaces/workspace",
                checkout.display()
            ),
            "--env".to_owned(),
            "MODE=development".to_owned(),
            "--user".to_owned(),
            "root".to_owned(),
            "--workdir".to_owned(),
            "/workspaces/workspace".to_owned(),
            "--init".to_owned(),
            "--cap-add".to_owned(),
            "SYS_PTRACE".to_owned(),
            "--security-opt".to_owned(),
            "seccomp=unconfined".to_owned(),
            "example.invalid/base:latest".to_owned(),
            "sleep".to_owned(),
            "infinity".to_owned(),
        ]
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the BuildKit fixture and exact argument checks form one contract"
)]
async fn fake_cli_build_uses_buildkit_ordered_options_labels_tag_and_typed_iid_claim() {
    let fixture = Fixture::new();
    let adapter = fixture.adapter();
    let checkout = fixture.temporary.path().join("checkout/workspace");
    fs::create_dir_all(checkout.join(".devcontainer")).expect("checkout");
    fs::write(checkout.join("Dockerfile"), "FROM scratch\n").expect("Dockerfile");
    fs::write(checkout.join(".dockerignore"), "ignored\n").expect("dockerignore");
    let stable_labels = StableIdentityLabels::new("installation", "workspace");
    let local_env = BTreeMap::from([("TOKEN".to_owned(), "secret-marker".to_owned())]);
    let owned = [ContainerPath::parse("/usr/local/share/cdenv").expect("owned path")];
    let profile = profile(
        r#"{
          "build":{
            "dockerfile":"../Dockerfile",
            "context":"..",
            "target":"development",
            "args":{"TOKEN":"${localEnv:TOKEN}"},
            "cacheFrom":["cache:first"],
            "options":["--network=host","--progress=plain"]
          }
        }"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("metadata");
    let runtime = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: checkout.to_str().expect("UTF-8 checkout"),
            local_env: &local_env,
            identity_labels: &stable_labels,
            scenario_metadata: ScenarioMetadata::default(),
            cdenv_owned_targets: &owned,
            host_user: None,
        },
    )
    .expect("runtime");
    let substitutions = HostSubstitutionInputs {
        local_workspace_folder: checkout.to_str().expect("UTF-8 checkout"),
        container_workspace_folder: "/workspaces/workspace",
        local_env: &local_env,
        identity_labels: &stable_labels,
    };
    let docker = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: ".devcontainer",
            substitutions: &substitutions,
            cdenv_owned_targets: &owned,
        },
    )
    .expect("Docker options");
    let BuildPlan::Dockerfile(build) = &docker.build else {
        panic!("Dockerfile plan expected")
    };
    let installation = InstallationId::parse("installation").expect("installation");
    let workspace = WorkspaceName::parse("workspace").expect("workspace");
    let profile_id = ProfileId::parse("cdenv-devcontainer-v1").expect("profile ID");
    let dockerfile = checkout.join("Dockerfile");
    let iid_file = fixture.temporary.path().join("claim.id");
    let cached = build_arguments(
        build,
        &dockerfile,
        &checkout,
        "cdenv/workspace:g2",
        &iid_file,
        identity(&installation, &workspace, &profile_id),
        false,
    )
    .expect("cached arguments");
    let uncached = build_arguments(
        build,
        &dockerfile,
        &checkout,
        "cdenv/workspace:g2",
        &iid_file,
        identity(&installation, &workspace, &profile_id),
        true,
    )
    .expect("uncached arguments");
    assert!(!cached.contains(&OsString::from("--no-cache")));
    assert_eq!(uncached.get(3), Some(&OsString::from("--no-cache")));

    let claim = adapter
        .build(
            &DockerBuildRequest {
                plan: build,
                checkout: &checkout,
                tag: "cdenv/workspace:g2",
                identity: identity(&installation, &workspace, &profile_id),
                context: &DockerBuildContext::Repository,
                dockerfile: DockerfileInput::Repository,
                no_cache: false,
            },
            &CancellationToken::default(),
        )
        .await
        .expect("build claim");

    assert_eq!(claim.image_id.as_str(), IMAGE_ID);
    let arguments = fixture.arguments();
    assert_eq!(
        &arguments[..5],
        [
            "build",
            "--network=host",
            "--progress=plain",
            "--file",
            checkout
                .join("Dockerfile")
                .to_str()
                .expect("Dockerfile path"),
        ]
    );
    assert_eq!(arguments.last().map(String::as_str), checkout.to_str());
    assert!(
        arguments
            .windows(2)
            .any(|values| values == ["--build-arg", "TOKEN=secret-marker"])
    );
    assert!(
        arguments
            .windows(2)
            .any(|values| values == ["--cache-from", "cache:first"])
    );
    assert!(
        arguments
            .windows(2)
            .any(|values| values == ["--tag", "cdenv/workspace:g2"])
    );
    assert!(arguments.contains(&"cdenv.profile=cdenv-devcontainer-v1".to_owned()));
    assert!(arguments.contains(&"cdenv.generated=true".to_owned()));
    assert_eq!(
        fixture.environment(),
        format!(
            "DOCKER_HOST={}\nDOCKER_BUILDKIT=1\n",
            fixture.endpoint.docker_host().to_string_lossy()
        )
    );
    let log = fs::read_to_string(claim.log_path).expect("build log");
    assert!(!log.contains("secret-marker"));
}
