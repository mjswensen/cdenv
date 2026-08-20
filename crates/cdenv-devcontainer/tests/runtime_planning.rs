//! Docker-free contract tests for runtime planning and host requirements.

use std::collections::BTreeMap;

use cdenv_devcontainer::{
    ConfigPath, ContainerPath, GpuAccessIntent, GpuCapabilities, GpuRequirement, HostCapabilities,
    HostRequirementWarningKind, HostRequirements, HostResource, HostUserIdentity, ImageMetadata,
    Measured, ParseLimits, PlanningErrorKind, RuntimePlanningInputs, ScenarioMetadata,
    StableIdentityLabels, UidUpdateIntent, UidUpdateSkipReason, UnknownMeasurement, UserEnvProbe,
    evaluate_host_requirements, merge_image_metadata, parse_jsonc, plan_runtime, validate_profile,
};
use serde_json::json;

fn repository(source: &str) -> cdenv_devcontainer::RawProfile {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("valid fixture path");
    let document =
        parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("valid fixture JSON");
    validate_profile(&document).expect("valid fixture profile")
}

fn plan(source: &str, metadata: &[ImageMetadata]) -> cdenv_devcontainer::RuntimePlan {
    let profile = repository(source);
    let effective = merge_image_metadata(metadata, &profile).expect("metadata merges");
    let local_env = BTreeMap::from([("TOKEN".to_owned(), "host-secret".to_owned())]);
    let labels = StableIdentityLabels::new("sensitive-installation", "demo");
    let owned = [ContainerPath::parse("/usr/local/share/cdenv").expect("valid owned target")];
    plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: "/checkout/demo",
            local_env: &local_env,
            identity_labels: &labels,
            scenario_metadata: ScenarioMetadata::default(),
            cdenv_owned_targets: &owned,
            host_user: Some(HostUserIdentity::new(1000, 1000)),
        },
    )
    .expect("runtime plan succeeds")
}

#[test]
fn workspace_defaults_are_authoritative_for_image_and_dockerfile_scenarios() {
    let image = plan(r#"{"image":"example.invalid/base"}"#, &[]);
    let dockerfile = plan(r#"{"build":{"dockerfile":"Dockerfile"}}"#, &[]);

    assert_eq!(image.workspace.folder.as_str(), "/workspaces/demo");
    assert_eq!(
        image.workspace.mount.source.as_deref(),
        Some("/checkout/demo")
    );
    assert_eq!(dockerfile.workspace, image.workspace);
}

#[test]
fn compose_workspace_gets_a_deterministic_override_and_compose_command_default() {
    let plan = plan(
        r#"{"dockerComposeFile":"compose.yaml","service":"app","workspaceFolder":"/src/demo"}"#,
        &[],
    );

    assert_eq!(plan.workspace.folder.as_str(), "/src/demo");
    assert_eq!(plan.workspace.mount.target.as_str(), "/src/demo");
    assert!(!plan.override_command);
}

#[test]
fn explicit_workspace_mount_allows_a_contained_source_and_folder() {
    let plan = plan(
        r#"{
          "image":"example.invalid/base",
          "workspaceMount":"type=bind,source=${localWorkspaceFolder}/src,target=/workspace,consistency=cached",
          "workspaceFolder":"/workspace/project"
        }"#,
        &[],
    );

    assert_eq!(
        plan.workspace.mount.source.as_deref(),
        Some("/checkout/demo/src")
    );
    assert_eq!(plan.workspace.mount.options[0].name, "consistency");
}

#[test]
fn workspace_and_owned_mount_containment_errors_name_exact_properties() {
    let profile = repository(
        r#"{
          "image":"example.invalid/base",
          "workspaceMount":"type=bind,source=/outside,target=/workspace",
          "workspaceFolder":"/workspace"
        }"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("merge");
    let labels = StableIdentityLabels::new("installation", "demo");
    let local_env = BTreeMap::new();
    let owned = [ContainerPath::parse("/usr/local/share/cdenv").expect("valid")];
    let inputs = RuntimePlanningInputs {
        local_workspace_folder: "/checkout/demo",
        local_env: &local_env,
        identity_labels: &labels,
        scenario_metadata: ScenarioMetadata::default(),
        cdenv_owned_targets: &owned,
        host_user: None,
    };
    let workspace_error = plan_runtime(&profile, &effective, &inputs)
        .expect_err("outside workspace source is rejected");

    assert_eq!(workspace_error.property_path, "$.workspaceMount");
    assert_eq!(
        workspace_error.kind,
        PlanningErrorKind::WorkspaceContainment
    );

    let profile = repository(
        r#"{"image":"example.invalid/base","mounts":["type=volume,source=x,target=/usr/local/share"]}"#,
    );
    let effective = merge_image_metadata(&[], &profile).expect("merge");
    let owned_error = plan_runtime(&profile, &effective, &inputs)
        .expect_err("ancestor of an owned target is rejected");

    assert_eq!(owned_error.property_path, "$.mounts[0]");
    assert_eq!(owned_error.kind, PlanningErrorKind::OwnedTargetConflict);
}

#[test]
fn repository_users_win_metadata_and_remote_defaults_to_container_user() {
    let metadata = ImageMetadata::from_value(
        "image",
        &json!({"containerUser":"image-user","remoteUser":"image-remote"}),
    )
    .expect("valid metadata");
    let selected = plan(
        r#"{"image":"example.invalid/base","containerUser":"repo-user","remoteUser":"repo-remote"}"#,
        &[metadata],
    );
    let defaulted = plan(
        r#"{"image":"example.invalid/base","containerUser":"repo-user"}"#,
        &[],
    );

    assert_eq!(selected.container_user.as_str(), "repo-user");
    assert_eq!(selected.remote_user.as_str(), "repo-remote");
    assert_eq!(defaulted.remote_user.as_str(), "repo-user");
}

#[test]
fn scenario_user_default_and_uid_update_decisions_are_explicit() {
    let profile = repository(r#"{"image":"example.invalid/base"}"#);
    let effective = merge_image_metadata(&[], &profile).expect("merge");
    let labels = StableIdentityLabels::new("installation", "demo");
    let local_env = BTreeMap::new();
    let planned = plan_runtime(
        &profile,
        &effective,
        &RuntimePlanningInputs {
            local_workspace_folder: "/checkout/demo",
            local_env: &local_env,
            identity_labels: &labels,
            scenario_metadata: ScenarioMetadata {
                container_user: Some("vscode"),
            },
            cdenv_owned_targets: &[],
            host_user: Some(HostUserIdentity::new(501, 20)),
        },
    )
    .expect("plan");

    assert_eq!(planned.container_user.as_str(), "vscode");
    assert!(matches!(
        planned.uid_update,
        UidUpdateIntent::UpdateNamedUser { .. }
    ));
    assert!(matches!(
        plan(
            r#"{"image":"example.invalid/base","remoteUser":"root"}"#,
            &[]
        )
        .uid_update,
        UidUpdateIntent::None(UidUpdateSkipReason::RootUser)
    ));
    assert!(matches!(
        plan(
            r#"{"image":"example.invalid/base","remoteUser":"1000"}"#,
            &[]
        )
        .uid_update,
        UidUpdateIntent::None(UidUpdateSkipReason::NumericUser)
    ));
    assert!(matches!(
        plan(
            r#"{"image":"example.invalid/base","remoteUser":"vscode","updateRemoteUserUID":false}"#,
            &[]
        )
        .uid_update,
        UidUpdateIntent::None(UidUpdateSkipReason::Disabled)
    ));
}

#[test]
fn environment_stages_and_probe_variants_do_not_leak_in_summaries() {
    let planned = plan(
        r#"{
          "image":"example.invalid/base",
          "containerUser":"identity-secret",
          "containerEnv":{"CREATE_SECRET":"${localEnv:TOKEN}"},
          "remoteEnv":{"RUNTIME_SECRET":"${containerEnv:PATH:/bin}","REMOVE_ME":null},
          "userEnvProbe":"interactiveShell"
        }"#,
        &[],
    );
    let runtime = planned.environment.remote()["RUNTIME_SECRET"]
        .as_ref()
        .expect("remote value")
        .resolve(&BTreeMap::from([(
            "PATH".to_owned(),
            "/secret/path".to_owned(),
        )]));
    let serialized = serde_json::to_string(&planned.summary()).expect("summary serializes");

    assert_eq!(
        planned.environment.container()["CREATE_SECRET"].expose(),
        "host-secret"
    );
    assert_eq!(runtime.expose(), "/secret/path");
    assert_eq!(planned.user_env_probe, UserEnvProbe::InteractiveShell);
    assert!(
        planned
            .environment
            .summary()
            .runtime_container_environment_required
    );
    for secret in [
        "host-secret",
        "CREATE_SECRET",
        "RUNTIME_SECRET",
        "identity-secret",
        "/checkout/demo",
    ] {
        assert!(!serialized.contains(secret));
    }
}

#[test]
fn user_environment_probe_uses_the_pinned_default_and_accepts_all_variants() {
    assert_eq!(
        plan(r#"{"image":"example.invalid/base"}"#, &[]).user_env_probe,
        UserEnvProbe::LoginInteractiveShell
    );
    for (value, expected) in [
        ("none", UserEnvProbe::None),
        ("loginShell", UserEnvProbe::LoginShell),
        ("loginInteractiveShell", UserEnvProbe::LoginInteractiveShell),
        ("interactiveShell", UserEnvProbe::InteractiveShell),
    ] {
        let source = format!(r#"{{"image":"example.invalid/base","userEnvProbe":"{value}"}}"#);
        assert_eq!(plan(&source, &[]).user_env_probe, expected);
    }
}

fn capabilities() -> HostCapabilities {
    HostCapabilities {
        cpus: Measured::Reliable(8),
        memory_bytes: Measured::Reliable(16_u64 << 30),
        storage_bytes: Measured::Reliable(100_u64 << 30),
        gpu: Measured::Reliable(Some(GpuCapabilities {
            cores: Some(4),
            memory_bytes: Some(8_u64 << 30),
        })),
    }
}

#[test]
fn host_requirements_distinguish_met_provably_unmet_and_unknown() {
    let requirements = HostRequirements {
        cpus: Some(4),
        memory: Some("8gb".to_owned()),
        storage: Some("32gb".to_owned()),
        gpu: None,
    };
    let met = evaluate_host_requirements(Some(&requirements), &capabilities()).expect("met");
    let error = evaluate_host_requirements(
        Some(&HostRequirements {
            cpus: Some(9),
            ..requirements.clone()
        }),
        &capabilities(),
    )
    .expect_err("reliably unmet CPU fails");
    let unknown = evaluate_host_requirements(
        Some(&requirements),
        &HostCapabilities {
            cpus: Measured::Unknown(UnknownMeasurement::ProbeUnavailable),
            memory_bytes: Measured::Unknown(UnknownMeasurement::Unmeasurable),
            storage_bytes: Measured::Unknown(UnknownMeasurement::Unmeasurable),
            gpu: Measured::Unknown(UnknownMeasurement::Unmeasurable),
        },
    )
    .expect("unknown evidence only warns");

    assert!(met.is_proved_met());
    assert_eq!(error.property_path, "$.hostRequirements.cpus");
    assert_eq!(unknown.warnings.len(), 3);
}

#[test]
fn required_and_optional_gpu_outcomes_preserve_explicit_access_intent() {
    let absent = HostCapabilities {
        gpu: Measured::Reliable(None),
        ..capabilities()
    };
    let required = HostRequirements {
        cpus: None,
        memory: None,
        storage: None,
        gpu: Some(GpuRequirement::Required),
    };
    let optional = HostRequirements {
        gpu: Some(GpuRequirement::Optional),
        ..required.clone()
    };
    let no_request = HostRequirements {
        gpu: Some(GpuRequirement::NotRequired),
        ..required.clone()
    };

    let error = evaluate_host_requirements(Some(&required), &absent)
        .expect_err("required absent GPU fails");
    let optional =
        evaluate_host_requirements(Some(&optional), &absent).expect("optional GPU warns");
    let no_request =
        evaluate_host_requirements(Some(&no_request), &capabilities()).expect("no GPU request");

    assert_eq!(error.resource, HostResource::Gpu);
    assert_eq!(optional.gpu_access, GpuAccessIntent::None);
    assert_eq!(
        optional.warnings[0].kind,
        HostRequirementWarningKind::OptionalGpuUnavailable
    );
    assert_eq!(no_request.gpu_access, GpuAccessIntent::None);
}
