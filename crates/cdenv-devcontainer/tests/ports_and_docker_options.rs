//! Pure contract tests for declared ports and Docker-shaped options.

use std::collections::BTreeMap;

use cdenv_devcontainer::{
    BuildPlan, ConfigPath, ContainerPath, DockerOptionErrorKind, DockerOptionPlanningInputs,
    ForwardTargetHost, HostSubstitutionInputs, ImageMetadata, ParseLimits, PortPlanningErrorKind,
    PortPlanningWarningKind, PublicationBinding, RawProfile, RuntimePlanningInputs,
    ScenarioMetadata, StableIdentityLabels, merge_image_metadata, parse_jsonc, plan_docker_options,
    plan_ports, plan_runtime, validate_profile,
};
use serde_json::json;

fn repository(source: &str) -> RawProfile {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("valid fixture path");
    let document =
        parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("valid fixture JSON");
    validate_profile(&document).expect("valid fixture profile")
}

fn effective(
    profile: &RawProfile,
    metadata: &[ImageMetadata],
) -> cdenv_devcontainer::EffectiveMetadata {
    merge_image_metadata(metadata, profile).expect("metadata merges")
}

struct PlanningFixture {
    local_env: BTreeMap<String, String>,
    labels: StableIdentityLabels,
    owned: Vec<ContainerPath>,
}

impl PlanningFixture {
    fn new() -> Self {
        Self {
            local_env: BTreeMap::from([("BUILD_SECRET".to_owned(), "resolved-secret".to_owned())]),
            labels: StableIdentityLabels::new("installation", "demo"),
            owned: vec![ContainerPath::parse("/usr/local/share/cdenv").expect("valid")],
        }
    }

    fn substitutions(&self) -> HostSubstitutionInputs<'_> {
        HostSubstitutionInputs {
            local_workspace_folder: "/checkout/demo",
            container_workspace_folder: "/workspaces/demo",
            local_env: &self.local_env,
            identity_labels: &self.labels,
        }
    }

    fn runtime(&self, profile: &RawProfile) -> cdenv_devcontainer::RuntimePlan {
        plan_runtime(
            profile,
            &effective(profile, &[]),
            &RuntimePlanningInputs {
                local_workspace_folder: "/checkout/demo",
                local_env: &self.local_env,
                identity_labels: &self.labels,
                scenario_metadata: ScenarioMetadata::default(),
                cdenv_owned_targets: &self.owned,
                host_user: None,
            },
        )
        .expect("runtime plan")
    }
}

#[test]
fn numeric_publication_is_fixed_same_port_loopback() {
    let profile = repository(r#"{"image":"example.invalid/base","appPort":3000}"#);
    let plan = plan_ports(&profile, &effective(&profile, &[])).expect("port plan");

    assert_eq!(plan.publications[0].argument, "127.0.0.1:3000:3000");
    assert!(matches!(
        plan.publications[0].binding,
        PublicationBinding::Loopback(_)
    ));
    assert!(plan.warnings.is_empty());
}

#[test]
fn explicit_ipv4_ipv6_and_protocol_publications_are_retained_exactly() {
    let profile = repository(
        r#"{"image":"example.invalid/base","appPort":["127.0.0.1:8000:80/udp","127.0.0.1:81","[::1]:8443:443","[::1]:444","0.0.0.0:9000:90"]}"#,
    );
    let plan = plan_ports(&profile, &effective(&profile, &[])).expect("port plan");

    assert_eq!(
        plan.publications
            .iter()
            .map(|publication| publication.argument.as_str())
            .collect::<Vec<_>>(),
        [
            "127.0.0.1:8000:80/udp",
            "127.0.0.1:81",
            "[::1]:8443:443",
            "[::1]:444",
            "0.0.0.0:9000:90"
        ]
    );
    assert_eq!(plan.warnings.len(), 1);
    assert_eq!(
        plan.warnings[0].kind,
        PortPlanningWarningKind::NonLoopbackPublication
    );
}

#[test]
fn implicit_all_interface_publication_warns_and_invalid_ports_fail_at_the_property() {
    let profile = repository(r#"{"image":"example.invalid/base","appPort":"8080:80"}"#);
    let plan = plan_ports(&profile, &effective(&profile, &[])).expect("valid publication");
    assert_eq!(
        plan.warnings[0].kind,
        PortPlanningWarningKind::NonLoopbackPublication
    );

    for argument in [
        "127.0.0.1:0:80",
        "127.0.0.1:70000:80",
        "::1:80:80",
        "8000-8002:80-81",
    ] {
        let source = format!(r#"{{"image":"example.invalid/base","appPort":"{argument}"}}"#);
        let profile = repository(&source);
        let error = plan_ports(&profile, &effective(&profile, &[]))
            .expect_err("invalid publication is rejected");
        assert_eq!(error.property_path, "$.appPort");
        assert_eq!(error.kind, PortPlanningErrorKind::InvalidPublication);
    }
}

#[test]
fn numeric_zero_and_out_of_range_ports_are_rejected_during_profile_validation() {
    for (source, property) in [
        (r#"{"image":"x","appPort":0}"#, "$.appPort"),
        (r#"{"image":"x","appPort":70000}"#, "$.appPort"),
        (r#"{"image":"x","forwardPorts":[0]}"#, "$.forwardPorts[0]"),
        (
            r#"{"dockerComposeFile":"compose.yaml","service":"app","workspaceFolder":"/w","forwardPorts":["db:70000"]}"#,
            "$.forwardPorts[0]",
        ),
    ] {
        let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
        let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSON");
        let error = validate_profile(&document).expect_err("invalid numeric port is rejected");
        assert_eq!(error.diagnostic.property_path, property);
    }
}

#[test]
fn fixed_publication_duplicates_and_forward_duplicates_are_rejected() {
    let profile =
        repository(r#"{"image":"example.invalid/base","appPort":[3000,"127.0.0.1:3000:80"]}"#);
    let error = plan_ports(&profile, &effective(&profile, &[]))
        .expect_err("fixed host port conflict is rejected");
    assert_eq!(error.property_path, "$.appPort[1]");
    assert_eq!(error.kind, PortPlanningErrorKind::PublicationConflict);

    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
    let document = parse_jsonc(
        &path,
        br#"{"image":"x","forwardPorts":[3000,"api:3000"]}"#,
        ParseLimits::default(),
    )
    .expect("JSON");
    let error = validate_profile(&document).expect_err("duplicate local request is rejected");
    assert_eq!(error.diagnostic.property_path, "$.forwardPorts[1]");
}

#[test]
fn explicit_forwards_apply_attributes_and_override_ignore_without_discovery() {
    let profile = repository(
        r#"{
          "dockerComposeFile":"compose.yaml","service":"app","workspaceFolder":"/workspace",
          "forwardPorts":[3000,"db:5432"],
          "portsAttributes":{
            "3000":{"label":"Web","protocol":"http","requireLocalPort":true,"onAutoForward":"ignore"},
            "db:5432":{"label":"Database"},
            "3000-4000":{"onAutoForward":"silent"}
          },
          "otherPortsAttributes":{"onAutoForward":"ignore"}
        }"#,
    );
    let plan = plan_ports(&profile, &effective(&profile, &[])).expect("port plan");

    assert_eq!(plan.forwards[0].attributes.label.as_deref(), Some("Web"));
    assert!(plan.forwards[0].attributes.require_local_port);
    assert_eq!(
        plan.forwards[1].target_host,
        ForwardTargetHost::ComposeService("db".to_owned())
    );
    assert_eq!(
        plan.forwards[1].attributes.label.as_deref(),
        Some("Database")
    );
    assert!(
        plan.forwards
            .iter()
            .all(|forward| forward.assigned_local.is_none())
    );
    assert!(plan.warnings.iter().any(|warning| {
        warning.kind == PortPlanningWarningKind::ExplicitForwardOverridesIgnore
    }));
    assert_eq!(
        plan.warnings
            .iter()
            .filter(|warning| warning.kind == PortPlanningWarningKind::DeferredDiscovery)
            .count(),
        2
    );
}

#[test]
fn requested_versus_assigned_rendering_snapshot_never_binds_a_listener() {
    let profile = repository(
        r#"{"image":"example.invalid/base","forwardPorts":[3000],"portsAttributes":{"3000":{"label":"Web","protocol":"https"}}}"#,
    );
    let plan = plan_ports(&profile, &effective(&profile, &[])).expect("port plan");
    let rendered = serde_json::to_value(plan.rendering_inputs()).expect("serializes");

    assert_eq!(
        rendered,
        json!([{
            "requestedLocal": 3000,
            "assignedLocal": null,
            "targetHost": {"kind":"containerLoopback"},
            "targetPort": 3000,
            "label": "Web",
            "protocol": "https"
        }])
    );
}

#[test]
fn service_forward_target_is_rejected_outside_compose() {
    let profile = repository(r#"{"image":"example.invalid/base","forwardPorts":["db:5432"]}"#);
    let error = plan_ports(&profile, &effective(&profile, &[]))
        .expect_err("service target requires Compose");

    assert_eq!(error.property_path, "$.forwardPorts[0]");
    assert_eq!(
        error.kind,
        PortPlanningErrorKind::ServiceTargetOutsideCompose
    );
}

#[test]
fn dockerfile_paths_defaults_build_arguments_and_options_are_planned_purely() {
    let profile = repository(
        r#"{
          "build":{
            "dockerfile":"../Dockerfile",
            "context":"..",
            "target":"development",
            "args":{"TOKEN":"${localEnv:BUILD_SECRET}"},
            "cacheFrom":["cache:one","cache:two"],
            "options":["--network=host","--no-cache-filter","install"]
          },
          "runArgs":["--device","/dev/kvm"]
        }"#,
    );
    let fixture = PlanningFixture::new();
    let runtime = fixture.runtime(&profile);
    let substitutions = fixture.substitutions();
    let plan = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: ".devcontainer",
            substitutions: &substitutions,
            cdenv_owned_targets: &fixture.owned,
        },
    )
    .expect("Docker options plan");
    let BuildPlan::Dockerfile(build) = plan.build else {
        panic!("expected Dockerfile build")
    };

    assert_eq!(build.dockerfile.as_str(), "Dockerfile");
    assert_eq!(build.context.as_str(), ".");
    assert_eq!(build.arguments["TOKEN"].expose(), "resolved-secret");
    assert_eq!(
        build.options,
        ["--network=host", "--no-cache-filter", "install"]
    );
    assert_eq!(plan.create.run_arguments, ["--device", "/dev/kvm"]);
    assert!(!format!("{build:?}").contains("resolved-secret"));
}

#[test]
fn build_reserved_split_equal_and_short_forms_report_the_exact_token() {
    let cases = [
        (vec!["--file", "Dockerfile"], "--file"),
        (vec!["--tag=result"], "--tag=result"),
        (vec!["--target", "other"], "--target"),
        (vec!["."], "."),
        (vec!["-o", "type=local,dest=out"], "-o"),
        (vec!["--iidfile=id"], "--iidfile=id"),
        (vec!["--metadata-file", "metadata.json"], "--metadata-file"),
        (vec!["--label", "cdenv.workspace=other"], "--label"),
    ];
    for (options, expected) in cases {
        let options = serde_json::to_string(&options).expect("options JSON");
        let profile = repository(&format!(
            r#"{{"build":{{"dockerfile":"Dockerfile","options":{options}}}}}"#
        ));
        let fixture = PlanningFixture::new();
        let runtime = fixture.runtime(&profile);
        let substitutions = fixture.substitutions();
        let error = plan_docker_options(
            &profile,
            &runtime,
            &DockerOptionPlanningInputs {
                config_directory: ".devcontainer",
                substitutions: &substitutions,
                cdenv_owned_targets: &fixture.owned,
            },
        )
        .expect_err("reserved build option is rejected");

        assert_eq!(error.argument, expected);
    }
}

#[test]
fn run_reserved_options_and_owned_mounts_report_exact_tokens() {
    let cases = [
        (vec!["--name=other"], DockerOptionErrorKind::ContainerName),
        (vec!["--rm"], DockerOptionErrorKind::AutoRemove),
        (vec!["-u1000"], DockerOptionErrorKind::ContainerUser),
        (vec!["-it"], DockerOptionErrorKind::AttachmentMode),
        (
            vec!["--label", "cdenv.generation=9"],
            DockerOptionErrorKind::IdentityLabel,
        ),
        (
            vec!["--mount", "type=bind,source=/tmp,target=/workspaces/demo"],
            DockerOptionErrorKind::MountTarget,
        ),
        (
            vec!["-v/tmp:/usr/local/share/cdenv/cache"],
            DockerOptionErrorKind::MountTarget,
        ),
        (
            vec!["--tmpfs=/workspaces/demo:rw"],
            DockerOptionErrorKind::MountTarget,
        ),
    ];
    for (arguments, expected_kind) in cases {
        let expected_argument = arguments[0];
        let arguments = serde_json::to_string(&arguments).expect("arguments JSON");
        let profile = repository(&format!(
            r#"{{"image":"example.invalid/base","runArgs":{arguments}}}"#
        ));
        let fixture = PlanningFixture::new();
        let runtime = fixture.runtime(&profile);
        let substitutions = fixture.substitutions();
        let error = plan_docker_options(
            &profile,
            &runtime,
            &DockerOptionPlanningInputs {
                config_directory: ".devcontainer",
                substitutions: &substitutions,
                cdenv_owned_targets: &fixture.owned,
            },
        )
        .expect_err("reserved run option is rejected");

        assert_eq!(error.argument, expected_argument);
        assert_eq!(error.kind, expected_kind);
    }
}

#[test]
fn non_reserved_options_round_trip_byte_for_byte_and_in_order() {
    let profile = repository(
        r#"{
          "image":"example.invalid/base",
          "runArgs":["--device=/dev/kvm","--label=com.example.kind=test","--read-only","--cap-add","SYS_ADMIN"]
        }"#,
    );
    let fixture = PlanningFixture::new();
    let runtime = fixture.runtime(&profile);
    let substitutions = fixture.substitutions();
    let plan = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: "",
            substitutions: &substitutions,
            cdenv_owned_targets: &fixture.owned,
        },
    )
    .expect("non-reserved arguments are retained");

    assert_eq!(
        plan.create.run_arguments,
        [
            "--device=/dev/kvm",
            "--label=com.example.kind=test",
            "--read-only",
            "--cap-add",
            "SYS_ADMIN"
        ]
    );
}

#[test]
fn checkout_escape_and_runtime_substitution_fail_with_focused_kinds() {
    let profile = repository(r#"{"build":{"dockerfile":"../../Dockerfile"}}"#);
    let fixture = PlanningFixture::new();
    let runtime = fixture.runtime(&profile);
    let substitutions = fixture.substitutions();
    let error = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: ".devcontainer",
            substitutions: &substitutions,
            cdenv_owned_targets: &fixture.owned,
        },
    )
    .expect_err("checkout escape fails");

    assert_eq!(error.property_path, "$.build.dockerfile");
    assert_eq!(error.kind, DockerOptionErrorKind::PathOutsideCheckout);

    let profile =
        repository(r#"{"image":"example.invalid/base","runArgs":["${containerEnv:SECRET}"]}"#);
    let runtime = fixture.runtime(&profile);
    let error = plan_docker_options(
        &profile,
        &runtime,
        &DockerOptionPlanningInputs {
            config_directory: ".devcontainer",
            substitutions: &substitutions,
            cdenv_owned_targets: &fixture.owned,
        },
    )
    .expect_err("runtime-only substitution is rejected in runArgs");
    assert_eq!(error.kind, DockerOptionErrorKind::Substitution);
}
