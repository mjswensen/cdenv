//! Docker-free immutable category and lifecycle snapshot contracts.

use std::collections::BTreeMap;

use cdenv_devcontainer::{
    CategoryFingerprints, ConfigPath, ContainerPath, DockerOptionPlanningInputs,
    HostSubstitutionInputs, ImmutablePlanInputs, ParseLimits, RawProfile, RuntimePlanningInputs,
    ScenarioMetadata, StableIdentityLabels, merge_image_metadata, parse_jsonc, plan_docker_options,
    plan_immutable, plan_lifecycle, plan_ports, plan_runtime, validate_profile,
};
use sha2::{Digest, Sha256};

struct Fixture {
    environment: BTreeMap<String, String>,
    labels: StableIdentityLabels,
    owned: Vec<ContainerPath>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            environment: BTreeMap::from([("SECRET".to_owned(), "secret-marker".to_owned())]),
            labels: StableIdentityLabels::new("installation", "workspace"),
            owned: vec![ContainerPath::parse("/usr/local/share/cdenv").expect("owned target")],
        }
    }

    fn substitutions(&self) -> HostSubstitutionInputs<'_> {
        HostSubstitutionInputs {
            local_workspace_folder: "/checkout/workspace",
            container_workspace_folder: "/workspaces/workspace",
            local_env: &self.environment,
            identity_labels: &self.labels,
        }
    }

    fn fingerprints(&self, source: &str) -> (CategoryFingerprints<Vec<u8>>, String) {
        let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("fixture path");
        let document =
            parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSONC");
        let profile: RawProfile = validate_profile(&document).expect("profile");
        let effective = merge_image_metadata(&[], &profile).expect("metadata");
        let runtime = plan_runtime(
            &profile,
            &effective,
            &RuntimePlanningInputs {
                local_workspace_folder: "/checkout/workspace",
                local_env: &self.environment,
                identity_labels: &self.labels,
                scenario_metadata: ScenarioMetadata::default(),
                cdenv_owned_targets: &self.owned,
                host_user: None,
            },
        )
        .expect("runtime");
        let docker = plan_docker_options(
            &profile,
            &runtime,
            &DockerOptionPlanningInputs {
                config_directory: ".devcontainer",
                substitutions: &self.substitutions(),
                cdenv_owned_targets: &self.owned,
            },
        )
        .expect("Docker options");
        let ports = plan_ports(&profile, &effective).expect("ports");
        let lifecycle = plan_lifecycle(&effective, &self.substitutions()).expect("lifecycle");
        let plan = plan_immutable(&ImmutablePlanInputs {
            build: &docker.build,
            create_options: &docker.create,
            runtime: &runtime,
            ports: &ports,
            features: &[],
            lifecycle: &lifecycle,
            entrypoints: &effective.entrypoints,
        });
        let summary = serde_json::to_string_pretty(plan.summary()).expect("summary");
        let fingerprints = plan.fingerprint_with(|_, bytes| Sha256::digest(bytes).to_vec());
        (fingerprints, summary)
    }
}

#[test]
fn image_dockerfile_and_compose_have_stable_secret_free_summaries() {
    let fixture = Fixture::new();
    let cases = [
        (
            r#"{"image":"example.invalid/base"}"#,
            include_str!("snapshots/immutable-image.json"),
        ),
        (
            r#"{"build":{"dockerfile":"Dockerfile"}}"#,
            include_str!("snapshots/immutable-dockerfile.json"),
        ),
        (
            r#"{"dockerComposeFile":"compose.yml","service":"app","workspaceFolder":"/workspace"}"#,
            include_str!("snapshots/immutable-compose.json"),
        ),
    ];
    for (source, expected) in cases {
        let (_, first) = fixture.fingerprints(source);
        let (_, second) = fixture.fingerprints(source);
        assert_eq!(first, second);
        assert_eq!(first, expected);
        assert!(!first.contains("secret-marker"));
    }
}

#[test]
fn changing_one_owned_property_changes_only_its_category() {
    let fixture = Fixture::new();
    let base = fixture
        .fingerprints(r#"{"image":"example.invalid/base"}"#)
        .0;
    let build = fixture
        .fingerprints(r#"{"image":"example.invalid/changed"}"#)
        .0;
    let create = fixture
        .fingerprints(r#"{"image":"example.invalid/base","containerEnv":{"TOKEN":"changed"}}"#)
        .0;
    let runtime = fixture
        .fingerprints(r#"{"image":"example.invalid/base","forwardPorts":[3000]}"#)
        .0;
    let lifecycle = fixture
        .fingerprints(r#"{"image":"example.invalid/base","postCreateCommand":"changed"}"#)
        .0;

    assert_ne!(base.build, build.build);
    assert_eq!(
        (
            base.create.clone(),
            base.runtime.clone(),
            base.lifecycle.clone()
        ),
        (build.create, build.runtime, build.lifecycle)
    );
    assert_ne!(base.create, create.create);
    assert_eq!(
        (
            base.build.clone(),
            base.runtime.clone(),
            base.lifecycle.clone()
        ),
        (create.build, create.runtime, create.lifecycle)
    );
    assert_ne!(base.runtime, runtime.runtime);
    assert_eq!(
        (
            base.build.clone(),
            base.create.clone(),
            base.lifecycle.clone()
        ),
        (runtime.build, runtime.create, runtime.lifecycle)
    );
    assert_ne!(base.lifecycle, lifecycle.lifecycle);
    assert_eq!(
        (base.build, base.create, base.runtime),
        (lifecycle.build, lifecycle.create, lifecycle.runtime)
    );
}
