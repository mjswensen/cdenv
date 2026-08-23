//! Deterministic Compose project and override planning contracts.

use std::collections::{BTreeMap, BTreeSet};

use cdenv_devcontainer::{
    ComposeIdentity, ComposeModel, ComposePlanningError, ComposePlanningInputs,
    ComposePrimaryOverride, ComposeServiceModel, ConfigPath, ContainerPath, ParseLimits,
    RawProfile, RuntimePlanningInputs, ScenarioMetadata, StableIdentityLabels,
    compose_project_name, merge_image_metadata, parse_jsonc, plan_compose, plan_runtime,
    validate_profile,
};

fn profile(source: &str) -> RawProfile {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("config path");
    let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSONC");
    validate_profile(&document).expect("valid profile")
}

fn compose_model() -> ComposeModel {
    ComposeModel {
        services: BTreeMap::from([
            (
                "app".to_owned(),
                ComposeServiceModel {
                    image: Some("example.invalid/app:latest".to_owned()),
                    has_build: false,
                    dependencies: BTreeSet::from(["db".to_owned()]),
                    user: Some("developer".to_owned()),
                },
            ),
            (
                "db".to_owned(),
                ComposeServiceModel {
                    image: Some("example.invalid/db:latest".to_owned()),
                    ..ComposeServiceModel::default()
                },
            ),
        ]),
    }
}

#[test]
fn compose_project_name_is_stable_and_docker_safe() {
    assert_eq!(
        compose_project_name("Install.ID", "My_Workspace"),
        "cdenv-install-id-my-workspace"
    );
}

#[test]
fn compose_project_name_truncates_with_stable_hash() {
    let installation = "installation".repeat(8);
    let workspace = "workspace".repeat(8);

    let project = compose_project_name(&installation, &workspace);

    assert_eq!(
        project,
        "cdenv-installationinstallationinstallationinstalla-b2c42ed35dd3"
    );
}

#[test]
fn plan_compose_generates_canonical_secret_bearing_final_override() {
    let profile = profile(
        r#"{
          "dockerComposeFile":"compose.yaml",
          "service":"app",
          "runServices":["app"],
          "workspaceFolder":"/workspace/demo",
          "containerEnv":{"TOKEN":"secret-value"},
          "containerUser":"developer",
          "init":true
        }"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("effective metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    let environment = BTreeMap::new();
    let owned = [ContainerPath::parse("/opt/cdenv").expect("owned path")];
    let runtime = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: "/outside/checkout",
            local_env: &environment,
            identity_labels: &labels,
            scenario_metadata: ScenarioMetadata {
                container_user: Some("developer"),
            },
            cdenv_owned_targets: &owned,
            host_user: None,
        },
    )
    .expect("runtime plan");
    let final_entrypoint = ["/opt/cdenv/entrypoint".to_owned()];
    let final_command = ["sleep".to_owned(), "infinity".to_owned()];
    let cdenv_devcontainer::RawScenario::Compose(scenario) = &profile.scenario else {
        panic!("fixture should be Compose");
    };

    let plan = plan_compose(
        scenario,
        &compose_model(),
        &ComposePlanningInputs {
            identity: ComposeIdentity {
                installation: "installation",
                workspace: "workspace",
                generation: "2",
                profile: "cdenv-devcontainer-v1",
            },
            runtime: &runtime,
            primary_override: ComposePrimaryOverride {
                image: Some("cdenv/final@sha256:abc"),
                entrypoint: Some(&final_entrypoint),
                command: Some(&final_command),
            },
        },
    )
    .expect("Compose plan");

    assert_eq!(
        std::str::from_utf8(plan.override_json()).expect("UTF-8 JSON"),
        r#"{"services":{"app":{"labels":{"cdenv.generation":"2","cdenv.installation":"installation","cdenv.profile":"cdenv-devcontainer-v1","cdenv.workspace":"workspace"},"image":"cdenv/final@sha256:abc","build":null,"pull_policy":"never","working_dir":"/workspace/demo","volumes":[{"type":"bind","source":"/outside/checkout","target":"/workspace/demo"}],"environment":{"TOKEN":"secret-value"},"user":"developer","init":true,"entrypoint":["/opt/cdenv/entrypoint"],"command":["sleep","infinity"]},"db":{"labels":{"cdenv.generation":"2","cdenv.installation":"installation","cdenv.profile":"cdenv-devcontainer-v1","cdenv.workspace":"workspace"}}}}"#
    );
}

#[test]
fn plan_compose_includes_transitive_dependencies_in_managed_set() {
    let profile = profile(
        r#"{"dockerComposeFile":"compose.yaml","service":"app","runServices":["app"],"workspaceFolder":"/workspace"}"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("effective metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    let environment = BTreeMap::new();
    let runtime = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: "/outside/checkout",
            local_env: &environment,
            identity_labels: &labels,
            scenario_metadata: ScenarioMetadata::default(),
            cdenv_owned_targets: &[],
            host_user: None,
        },
    )
    .expect("runtime plan");
    let cdenv_devcontainer::RawScenario::Compose(scenario) = &profile.scenario else {
        panic!("fixture should be Compose");
    };

    let plan = plan_compose(
        scenario,
        &compose_model(),
        &ComposePlanningInputs {
            identity: ComposeIdentity {
                installation: "installation",
                workspace: "workspace",
                generation: "1",
                profile: "profile",
            },
            runtime: &runtime,
            primary_override: ComposePrimaryOverride::default(),
        },
    )
    .expect("Compose plan");

    assert_eq!(plan.managed_services(), ["app", "db"]);
}

#[test]
fn plan_compose_rejects_an_unknown_run_service() {
    let profile = profile(
        r#"{"dockerComposeFile":"compose.yaml","service":"app","runServices":["worker"],"workspaceFolder":"/workspace"}"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("effective metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    let environment = BTreeMap::new();
    let runtime = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: "/outside/checkout",
            local_env: &environment,
            identity_labels: &labels,
            scenario_metadata: ScenarioMetadata::default(),
            cdenv_owned_targets: &[],
            host_user: None,
        },
    )
    .expect("runtime plan");
    let cdenv_devcontainer::RawScenario::Compose(scenario) = &profile.scenario else {
        panic!("fixture should be Compose");
    };

    let error = plan_compose(
        scenario,
        &compose_model(),
        &ComposePlanningInputs {
            identity: ComposeIdentity {
                installation: "installation",
                workspace: "workspace",
                generation: "1",
                profile: "profile",
            },
            runtime: &runtime,
            primary_override: ComposePrimaryOverride::default(),
        },
    )
    .expect_err("unknown service should fail");

    assert_eq!(
        error,
        ComposePlanningError::MissingRunService {
            service: "worker".to_owned()
        }
    );
}
