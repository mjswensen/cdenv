//! Immutable category plans, canonical keyed-fingerprint material, and drift classification.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::{Value, json};

use crate::{
    BuildPlan, CreateOptionsPlan, DeferredString, FeatureInstallIdentity, FeatureValue,
    LifecycleCommand, LifecyclePlan, LifecycleProcess, MountKind, PlannedMount, PortPlan,
    ResolvedFeature, RuntimePlan, UidUpdateIntent,
};

/// One independently reconcilable immutable plan category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanCategory {
    /// Base source, build settings, Features, metadata build, and UID/GID update.
    Build,
    /// Container creation settings.
    Create,
    /// Settings safely applicable to an existing active container.
    Runtime,
    /// Generation-owned lifecycle command lists.
    Lifecycle,
}

/// Borrowed inputs used to separate effective settings into immutable categories.
pub struct ImmutablePlanInputs<'a> {
    /// Docker image/Dockerfile/Compose build source.
    pub build: &'a BuildPlan,
    /// Docker-shaped create options.
    pub create_options: &'a CreateOptionsPlan,
    /// Effective workspace, environment, user, and container settings.
    pub runtime: &'a RuntimePlan,
    /// Create-time publications and runtime forwards.
    pub ports: &'a PortPlan,
    /// Deterministically ordered resolved Features.
    pub features: &'a [ResolvedFeature],
    /// Effective lifecycle plan.
    pub lifecycle: &'a LifecyclePlan,
    /// Ordered generated entrypoints.
    pub entrypoints: &'a [String],
}

/// Persistence-safe structural summary for reviewed snapshots.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImmutablePlanSummary {
    /// Image, Dockerfile, or Compose.
    pub scenario: &'static str,
    /// Number of resolved Feature layers.
    pub feature_layers: usize,
    /// Number of additional mounts.
    pub additional_mounts: usize,
    /// Number of create-time publications.
    pub publications: usize,
    /// Number of runtime forwarding requests.
    pub forwards: usize,
    /// Number of immutable lifecycle command groups.
    pub lifecycle_groups: usize,
    /// Readiness stage.
    pub readiness: String,
    /// Whether runtime container-environment capture is required.
    pub runtime_environment_required: bool,
}

/// Canonical category material. Debug and serde intentionally cannot reveal plan contents.
pub struct ImmutablePlan {
    build: Vec<u8>,
    create: Vec<u8>,
    runtime: Vec<u8>,
    lifecycle: Vec<u8>,
    summary: ImmutablePlanSummary,
}

impl std::fmt::Debug for ImmutablePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImmutablePlan")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl ImmutablePlan {
    /// Returns a secret-free structural summary.
    #[must_use]
    pub const fn summary(&self) -> &ImmutablePlanSummary {
        &self.summary
    }

    /// Applies an installation-owned keyed digest primitive to all canonical categories.
    ///
    /// The callback receives canonical bytes only transiently. `ImmutablePlan` exposes no raw-byte
    /// accessor and implements neither serialization nor cloning.
    pub fn fingerprint_with<T>(
        &self,
        mut digest: impl FnMut(PlanCategory, &[u8]) -> T,
    ) -> CategoryFingerprints<T> {
        CategoryFingerprints {
            build: digest(PlanCategory::Build, &self.build),
            create: digest(PlanCategory::Create, &self.create),
            runtime: digest(PlanCategory::Runtime, &self.runtime),
            lifecycle: digest(PlanCategory::Lifecycle, &self.lifecycle),
        }
    }
}

/// Four opaque category fingerprints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CategoryFingerprints<T> {
    /// Build category.
    pub build: T,
    /// Create category.
    pub create: T,
    /// Runtime category.
    pub runtime: T,
    /// Lifecycle category.
    pub lifecycle: T,
}

/// Produces canonical immutable category plans without I/O.
#[must_use]
pub fn plan_immutable(inputs: &ImmutablePlanInputs<'_>) -> ImmutablePlan {
    let build_value = json!({
        "source": build_value(inputs.build),
        "features": inputs.features.iter().map(feature_value).collect::<Vec<_>>(),
        "uidUpdate": uid_value(&inputs.runtime.uid_update),
    });
    let create_value = json!({
        "workspace": {
            "folder": inputs.runtime.workspace.folder.as_str(),
            "mount": mount_value(&inputs.runtime.workspace.mount),
        },
        "mounts": inputs.runtime.mounts.iter().map(mount_value).collect::<Vec<_>>(),
        "containerUser": inputs.runtime.container_user.as_str(),
        "containerEnvironment": inputs.runtime.environment.container().iter()
            .map(|(name, value)| (name.clone(), Value::String(value.expose().to_owned())))
            .collect::<BTreeMap<_, _>>(),
        "publications": inputs.ports.publications.iter().map(|item| item.argument.as_str()).collect::<Vec<_>>(),
        "overrideCommand": inputs.runtime.override_command,
        "init": inputs.runtime.init,
        "privileged": inputs.runtime.privileged,
        "capAdd": inputs.runtime.cap_add,
        "securityOpt": inputs.runtime.security_opt,
        "runArguments": inputs.create_options.run_arguments,
        "entrypoints": inputs.entrypoints,
    });
    let runtime_value = json!({
        "remoteUser": inputs.runtime.remote_user.as_str(),
        "remoteEnvironment": inputs.runtime.environment.remote().iter().map(|(name, value)| {
            (name.clone(), value.as_ref().map_or(Value::Null, DeferredString::fingerprint_value))
        }).collect::<BTreeMap<_, _>>(),
        "userEnvProbe": format!("{:?}", inputs.runtime.user_env_probe),
        "forwards": inputs.ports.forwards,
    });
    let lifecycle_value = lifecycle_value(inputs.lifecycle);
    let lifecycle_groups = lifecycle_group_count(inputs.lifecycle);
    let runtime_environment_required = inputs
        .runtime
        .environment
        .summary()
        .runtime_container_environment_required
        || lifecycle_requires_container_environment(inputs.lifecycle);
    ImmutablePlan {
        build: canonical(&build_value),
        create: canonical(&create_value),
        runtime: canonical(&runtime_value),
        lifecycle: canonical(&lifecycle_value),
        summary: ImmutablePlanSummary {
            scenario: match inputs.build {
                BuildPlan::Image { .. } => "image",
                BuildPlan::Dockerfile(_) => "dockerfile",
                BuildPlan::Compose => "compose",
            },
            feature_layers: inputs.features.len(),
            additional_mounts: inputs.runtime.mounts.len(),
            publications: inputs.ports.publications.len(),
            forwards: inputs.ports.forwards.len(),
            lifecycle_groups,
            readiness: format!("{:?}", inputs.lifecycle.readiness),
            runtime_environment_required,
        },
    }
}

fn build_value(plan: &BuildPlan) -> Value {
    match plan {
        BuildPlan::Image { image } => json!({"kind": "image", "image": image}),
        BuildPlan::Dockerfile(plan) => json!({
            "kind": "dockerfile", "dockerfile": plan.dockerfile.as_str(),
            "context": plan.context.as_str(), "target": plan.target,
            "arguments": plan.arguments.iter().map(|(name, value)| (name.clone(), value.expose())).collect::<BTreeMap<_, _>>(),
            "cacheFrom": plan.cache_from, "options": plan.options,
        }),
        BuildPlan::Compose => json!({"kind": "compose"}),
    }
}

fn feature_value(feature: &ResolvedFeature) -> Value {
    json!({
        "reference": feature.reference.as_str(),
        "identity": identity_value(&feature.identity),
        "id": feature.metadata.id,
        "version": feature.metadata.version,
        "options": feature.options.iter().map(|(name, value)| (name.clone(), option_value(value))).collect::<BTreeMap<_, _>>(),
    })
}

fn identity_value(identity: &FeatureInstallIdentity) -> &str {
    match identity {
        FeatureInstallIdentity::OciDigest(value)
        | FeatureInstallIdentity::HttpsIntegrity(value)
        | FeatureInstallIdentity::Local(value) => value,
    }
}

fn option_value(value: &FeatureValue) -> Value {
    match value {
        FeatureValue::Boolean(value) => Value::Bool(*value),
        FeatureValue::String(value) => Value::String(value.clone()),
    }
}

fn uid_value(intent: &UidUpdateIntent) -> Value {
    match intent {
        UidUpdateIntent::None(reason) => json!({"kind": "none", "reason": format!("{reason:?}")}),
        UidUpdateIntent::UpdateNamedUser { user, host } => json!({
            "kind": "update", "user": user.as_str(), "uid": host.uid(), "gid": host.gid(),
        }),
    }
}

fn mount_value(mount: &PlannedMount) -> Value {
    json!({
        "type": match mount.kind { MountKind::Bind => "bind", MountKind::Volume => "volume" },
        "source": mount.source,
        "target": mount.target.as_str(),
        "options": mount.options.iter().map(|option| json!({"name": option.name, "value": option.value})).collect::<Vec<_>>(),
    })
}

fn lifecycle_value(plan: &LifecyclePlan) -> Value {
    let stage = |stage: &crate::LifecycleStagePlan| {
        Value::Array(stage.commands.iter().map(command_value).collect())
    };
    json!({
        "initialize": stage(&plan.initialize), "onCreate": stage(&plan.on_create),
        "updateContent": stage(&plan.update_content), "postCreate": stage(&plan.post_create),
        "postStart": stage(&plan.post_start), "postAttach": stage(&plan.post_attach),
        "readiness": format!("{:?}", plan.readiness),
    })
}

fn command_value(command: &LifecycleCommand) -> Value {
    match command {
        LifecycleCommand::Process(process) => process_value(process),
        LifecycleCommand::Parallel(processes) => {
            json!({"parallel": processes.iter().map(|(key, process)| (key.clone(), process_value(process))).collect::<BTreeMap<_, _>>() })
        }
    }
}

fn process_value(process: &LifecycleProcess) -> Value {
    match process {
        LifecycleProcess::Shell(value) => json!({"shell": value.fingerprint_value()}),
        LifecycleProcess::Exec(values) => {
            json!({"exec": values.iter().map(DeferredString::fingerprint_value).collect::<Vec<_>>() })
        }
    }
}

fn lifecycle_group_count(plan: &LifecyclePlan) -> usize {
    [
        &plan.initialize,
        &plan.on_create,
        &plan.update_content,
        &plan.post_create,
        &plan.post_start,
        &plan.post_attach,
    ]
    .iter()
    .map(|stage| stage.commands.len())
    .sum()
}

fn lifecycle_requires_container_environment(plan: &LifecyclePlan) -> bool {
    [
        &plan.initialize,
        &plan.on_create,
        &plan.update_content,
        &plan.post_create,
        &plan.post_start,
        &plan.post_attach,
    ]
    .iter()
    .flat_map(|stage| &stage.commands)
    .any(LifecycleCommand::requires_container_environment)
}

fn canonical(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap_or_default()
}

/// Desired-plan validity supplied by pure parsing/planning.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DesiredPlan<F> {
    /// Desired configuration produced all category fingerprints.
    Valid(CategoryFingerprints<F>),
    /// Desired configuration is invalid; active operation remains available.
    Invalid,
}

/// Highest-priority reconciliation action for desired-versus-active plan drift.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriftClassification {
    /// Every desired category matches the active generation.
    Current,
    /// Runtime drift can be applied independently; booleans retain concurrent warnings.
    RuntimeApplicable {
        /// Build drift also requires a user-chosen rebuild.
        build_warning: bool,
        /// Create drift also requires a user-chosen rebuild.
        create_warning: bool,
        /// Lifecycle changes are retained for the next generation.
        lifecycle_next_generation: bool,
    },
    /// Build and/or create settings differ and should warn without automatic rebuild.
    BuildCreateWarning {
        /// Build category differs.
        build: bool,
        /// Create category differs.
        create: bool,
        /// Lifecycle changes are also pending for the next generation.
        lifecycle_next_generation: bool,
    },
    /// Only lifecycle commands changed; active generation commands remain immutable.
    LifecycleNextGeneration,
    /// Desired parsing/planning failed. Existing active operations remain usable.
    InvalidDesired,
}

/// Classifies category drift without mutating the active generation.
#[must_use]
pub fn classify_drift<F: PartialEq>(
    desired: &DesiredPlan<F>,
    active: &CategoryFingerprints<F>,
) -> DriftClassification {
    let DesiredPlan::Valid(desired) = desired else {
        return DriftClassification::InvalidDesired;
    };
    let build = desired.build != active.build;
    let create = desired.create != active.create;
    let runtime = desired.runtime != active.runtime;
    let lifecycle = desired.lifecycle != active.lifecycle;
    if runtime {
        DriftClassification::RuntimeApplicable {
            build_warning: build,
            create_warning: create,
            lifecycle_next_generation: lifecycle,
        }
    } else if build || create {
        DriftClassification::BuildCreateWarning {
            build,
            create,
            lifecycle_next_generation: lifecycle,
        }
    } else if lifecycle {
        DriftClassification::LifecycleNextGeneration
    } else {
        DriftClassification::Current
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fingerprints(values: [u8; 4]) -> CategoryFingerprints<u8> {
        CategoryFingerprints {
            build: values[0],
            create: values[1],
            runtime: values[2],
            lifecycle: values[3],
        }
    }

    #[test]
    fn drift_classification_preserves_runtime_applicability_and_other_warnings() {
        let active = fingerprints([1, 1, 1, 1]);
        let desired = DesiredPlan::Valid(fingerprints([2, 1, 2, 2]));
        assert_eq!(
            classify_drift(&desired, &active),
            DriftClassification::RuntimeApplicable {
                build_warning: true,
                create_warning: false,
                lifecycle_next_generation: true,
            }
        );
        assert_eq!(
            classify_drift(&DesiredPlan::Invalid, &active),
            DriftClassification::InvalidDesired
        );
    }
}
