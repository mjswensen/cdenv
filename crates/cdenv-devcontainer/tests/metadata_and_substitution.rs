//! Pure contract tests for metadata merging and staged substitution.

use std::collections::BTreeMap;

use cdenv_devcontainer::{
    ConfigPath, ForwardPort, HostSubstitutionInputs, ImageMetadata, ParseLimits,
    StableIdentityLabels, SubstitutionProperty, merge_image_metadata, parse_jsonc, substitute_host,
    validate_profile,
};
use serde_json::{Value, json};

fn repository(source: &str) -> cdenv_devcontainer::RawProfile {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("fixture path is valid");
    let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default())
        .expect("fixture JSON is valid");
    validate_profile(&document).expect("fixture profile is valid")
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one explicit fixture keeps the complete property merge table reviewable"
)]
fn metadata_merge_applies_every_property_strategy_with_repository_last() {
    let first = ImageMetadata::from_value(
        "base image",
        &json!({
            "init": true,
            "privileged": false,
            "capAdd": ["A", "B"],
            "securityOpt": ["one"],
            "entrypoint": "/first",
            "mounts": ["type=volume,source=first,target=/same"],
            "onCreateCommand": "first",
            "updateContentCommand": ["first-update"],
            "postCreateCommand": "first-post",
            "postStartCommand": "first-start",
            "postAttachCommand": "first-attach",
            "waitFor": "onCreateCommand",
            "containerUser": "first-user",
            "remoteUser": "first-remote",
            "userEnvProbe": "loginShell",
            "containerEnv": {"ORDER": "first", "FIRST": "yes"},
            "remoteEnv": {"ORDER": "first"},
            "overrideCommand": false,
            "portsAttributes": {"3000": {"label": "first"}},
            "otherPortsAttributes": {"label": "first-other"},
            "forwardPorts": [3000, "old:4000"],
            "shutdownAction": "stopContainer",
            "updateRemoteUserUID": false,
            "hostRequirements": {"cpus": 1, "memory": "2gb", "storage": "1gb", "gpu": "optional"},
            "customizations": {"first": {"marker": "one"}}
        }),
    )
    .expect("first metadata is valid");
    let second = ImageMetadata::from_value(
        "final image",
        &json!({
            "init": false,
            "privileged": true,
            "capAdd": ["B", "C"],
            "securityOpt": ["one", "two"],
            "entrypoint": "/second",
            "mounts": [{"type": "volume", "source": "second", "target": "/same"}],
            "onCreateCommand": "second",
            "containerEnv": {"ORDER": "second"},
            "forwardPorts": ["new:4000", 5000],
            "hostRequirements": {"cpus": 4, "memory": "1gb", "storage": "3gb", "gpu": true}
        }),
    )
    .expect("second metadata is valid");
    let repo = repository(
        r#"{
          "image":"example.invalid/image",
          "init":false,"capAdd":["C","D"],"securityOpt":["three"],
          "mounts":[{"type":"bind","source":"repository","target":"/same"}],
          "onCreateCommand":"repository","containerUser":"repository-user",
          "containerEnv":{"ORDER":"repository"},"remoteEnv":{"ORDER":"repository"},
          "overrideCommand":true,"portsAttributes":{"3000":{"label":"repository"}},
          "otherPortsAttributes":{"label":"repository-other"},
          "forwardPorts":[6000],"shutdownAction":"none","updateRemoteUserUID":true,
          "hostRequirements":{"cpus":2,"memory":"3gb","storage":"2gb","gpu":{"cores":2,"memory":"4gb"}},
          "customizations":{"repository":{"marker":"last"}}
        }"#,
    );

    let effective = merge_image_metadata(&[first, second], &repo).expect("merge succeeds");

    assert_eq!(effective.init, Some(true));
    assert_eq!(effective.privileged, Some(true));
    assert_eq!(effective.cap_add, ["A", "B", "C", "D"]);
    assert_eq!(effective.security_opt, ["one", "two", "three"]);
    assert_eq!(effective.entrypoints, ["/first", "/second"]);
    assert!(format!("{:?}", effective.mounts[0]).contains("repository"));
    assert_eq!(effective.lifecycle.on_create.len(), 3);
    assert_eq!(effective.container_user.as_deref(), Some("repository-user"));
    assert_eq!(effective.container_env["ORDER"], "repository");
    assert_eq!(effective.remote_env["ORDER"].as_deref(), Some("repository"));
    assert_eq!(effective.override_command, Some(true));
    assert_eq!(
        effective.ports_attributes["3000"].label.as_deref(),
        Some("repository")
    );
    assert_eq!(
        effective
            .other_ports_attributes
            .as_ref()
            .and_then(|value| value.label.as_deref()),
        Some("repository-other")
    );
    assert_eq!(
        effective.forward_ports,
        [
            ForwardPort::Container(3000),
            ForwardPort::Service {
                service: "new".to_owned(),
                port: 4000
            },
            ForwardPort::Container(5000),
            ForwardPort::Container(6000)
        ]
    );
    assert_eq!(
        effective.shutdown_action,
        Some(cdenv_devcontainer::ShutdownAction::None)
    );
    assert_eq!(effective.update_remote_user_uid, Some(true));
    let requirements = effective
        .host_requirements
        .expect("requirements are present");
    assert_eq!(
        (
            requirements.cpus,
            requirements.memory.as_deref(),
            requirements.storage.as_deref()
        ),
        (Some(4), Some("3gb"), Some("3gb"))
    );
    assert_eq!(effective.customizations.len(), 2);
}

#[test]
fn metadata_validation_retains_source_and_property_without_rejected_value() {
    let marker = "SECRET-MARKER-never-report";
    let error = ImageMetadata::from_value(
        "base image sha256:abc",
        &json!({"containerEnv": {"TOKEN": [marker]}}),
    )
    .expect_err("invalid environment name is rejected");
    let diagnostic = error.to_string();

    assert!(
        diagnostic.contains("base image sha256:abc")
            && diagnostic.contains("containerEnv")
            && !diagnostic.contains(marker)
    );
}

#[test]
fn metadata_merge_is_independent_of_input_object_key_order() {
    let left: Value = serde_json::from_str(r#"{"containerEnv":{"B":"2","A":"1"},"init":true}"#)
        .expect("valid JSON");
    let right: Value = serde_json::from_str(r#"{"init":true,"containerEnv":{"A":"1","B":"2"}}"#)
        .expect("valid JSON");
    let repo = repository(r#"{"image":"example.invalid/image"}"#);

    let a = merge_image_metadata(
        &[ImageMetadata::from_value("image", &left).expect("valid")],
        &repo,
    )
    .expect("merge");
    let b = merge_image_metadata(
        &[ImageMetadata::from_value("image", &right).expect("valid")],
        &repo,
    )
    .expect("merge");

    assert_eq!(a, b);
}

#[test]
fn metadata_merge_revalidates_normalized_byte_requirements() {
    let metadata = ImageMetadata::from_value(
        "overflow image",
        &json!({"hostRequirements": {"memory": "18446744073709551615tb"}}),
    )
    .expect("shape validation accepts the bounded string");
    let repo = repository(r#"{"image":"example.invalid/image"}"#);

    let error =
        merge_image_metadata(&[metadata], &repo).expect_err("normalization overflow is rejected");

    assert!(
        error.to_string().contains("overflow image")
            && error.to_string().contains("hostRequirements.memory")
    );
}

fn substitution_inputs<'a>(
    local_env: &'a BTreeMap<String, String>,
    labels: &'a StableIdentityLabels,
) -> HostSubstitutionInputs<'a> {
    HostSubstitutionInputs {
        local_workspace_folder: "/checkout/example",
        container_workspace_folder: "/workspaces/example",
        local_env,
        identity_labels: labels,
    }
}

#[test]
fn substitution_respects_host_and_runtime_stage_boundaries() {
    let local_env = BTreeMap::from([("TOKEN".to_owned(), "local-secret".to_owned())]);
    let labels = StableIdentityLabels::new("installation", "workspace");
    let host = substitute_host(
        SubstitutionProperty::RemoteEnv,
        "${localEnv:TOKEN}:${containerEnv:PATH:/bin}",
        &substitution_inputs(&local_env, &labels),
    )
    .expect("host substitution succeeds");

    assert!(host.requires_container_environment());
    assert_eq!(
        host.resolve(&BTreeMap::from([(
            "PATH".to_owned(),
            "/usr/bin".to_owned()
        )]))
        .expose(),
        "local-secret:/usr/bin"
    );
}

#[test]
fn substitution_enforces_properties_defaults_escaping_and_unknown_variables() {
    let local_env = BTreeMap::new();
    let labels = StableIdentityLabels::new("installation", "workspace");
    let inputs = substitution_inputs(&local_env, &labels);
    let escaped = substitute_host(
        SubstitutionProperty::Name,
        "$${localEnv:MISSING}-${localEnv:MISSING:fallback}",
        &inputs,
    )
    .expect("escaping and default are supported")
    .resolve(&BTreeMap::new());
    let disallowed = substitute_host(
        SubstitutionProperty::BuildArgument,
        "${devcontainerId}",
        &inputs,
    )
    .expect_err("ID is forbidden in image build inputs");
    let unsupported = substitute_host(SubstitutionProperty::Name, "${hostEnv:TOKEN}", &inputs)
        .expect_err("unknown syntax fails closed");

    assert_eq!(escaped.expose(), "${localEnv:MISSING}-fallback");
    assert!(
        disallowed.to_string().contains("devcontainerId")
            && unsupported.to_string().contains("hostEnv")
    );
}

#[test]
fn devcontainer_id_is_canonical_and_stable_across_generations() {
    let labels = StableIdentityLabels::new("installation", "workspace");

    let result = labels.devcontainer_id();

    assert_eq!(
        result,
        "1l8p5jj2notvbmhrrdqf7qiob3uma4b7dr1uqf6vs91pujn7vbib"
    );
    assert_eq!(
        result,
        StableIdentityLabels::new("installation", "workspace").devcontainer_id()
    );
}

#[test]
fn sensitive_substitutions_are_absent_from_debug_errors_and_persisted_summaries() {
    let marker = "SECRET-MARKER-never-persist";
    let local_env = BTreeMap::from([("TOKEN".to_owned(), marker.to_owned())]);
    let labels = StableIdentityLabels::new("installation", "workspace");
    let value = substitute_host(
        SubstitutionProperty::RemoteEnv,
        "${localEnv:TOKEN}${containerEnv:MISSING}",
        &substitution_inputs(&local_env, &labels),
    )
    .expect("substitution succeeds");
    let snapshot = format!("{value:?}");
    let summary = serde_json::to_string(&value.summary(SubstitutionProperty::RemoteEnv))
        .expect("summary serializes");

    assert!(
        !snapshot.contains(marker)
            && !summary.contains(marker)
            && !format!("{:?}", value.resolve(&BTreeMap::new())).contains(marker)
    );
}
