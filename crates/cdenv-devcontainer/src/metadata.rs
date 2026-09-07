//! Typed image metadata inputs and property-specific effective configuration merging.

use std::collections::{BTreeMap, BTreeSet};

use crate::{
    ConfigPath, ForwardPort, GpuRequirement, HostRequirements, LifecycleCommands, ParseLimits,
    PortAttributes, ProfileError, RawCommand, RawCommon, RawMount, RawProfile, RawScenario,
    ShutdownAction, UserEnvProbe, WaitFor, parse_jsonc, validate_profile,
};
use serde_json::Value;

const METADATA_PROPERTIES: &[&str] = &[
    "id",
    "init",
    "privileged",
    "capAdd",
    "securityOpt",
    "entrypoint",
    "mounts",
    "onCreateCommand",
    "updateContentCommand",
    "postCreateCommand",
    "postStartCommand",
    "postAttachCommand",
    "waitFor",
    "customizations",
    "containerUser",
    "remoteUser",
    "userEnvProbe",
    "remoteEnv",
    "containerEnv",
    "overrideCommand",
    "portsAttributes",
    "otherPortsAttributes",
    "forwardPorts",
    "shutdownAction",
    "updateRemoteUserUID",
    "hostRequirements",
];

/// One inspected `devcontainer.metadata` contribution supplied by the host planner.
#[derive(Clone, PartialEq)]
pub struct ImageMetadata {
    source: String,
    common: RawCommon,
    entrypoint: Option<String>,
    override_command: Option<bool>,
    shutdown_action: Option<ShutdownAction>,
}

impl std::fmt::Debug for ImageMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ImageMetadata")
            .field("source", &self.source)
            .field(
                "container_env_keys",
                &self.common.container_env.keys().collect::<Vec<_>>(),
            )
            .field(
                "remote_env_keys",
                &self.common.remote_env.keys().collect::<Vec<_>>(),
            )
            .finish_non_exhaustive()
    }
}

impl ImageMetadata {
    /// Converts one resolved Feature's effective metadata contributions into an image contribution.
    #[must_use]
    pub fn from_feature(feature: &crate::ResolvedFeature) -> Self {
        let contributions = &feature.metadata.contributions;
        let common = RawCommon {
            container_env: contributions.container_env.clone(),
            mounts: contributions.mounts.clone(),
            cap_add: contributions.cap_add.clone(),
            security_opt: contributions.security_opt.clone(),
            lifecycle: contributions.lifecycle.clone(),
            customizations: contributions.customizations.clone(),
            init: contributions.init,
            privileged: contributions.privileged,
            ..RawCommon::default()
        };
        Self {
            source: feature.reference.as_str().to_owned(),
            common,
            entrypoint: contributions.entrypoint.clone(),
            override_command: None,
            shutdown_action: None,
        }
    }

    /// Parses and validates one already-inspected metadata object.
    ///
    /// The caller owns image inspection and label JSON decoding. This function performs no I/O.
    ///
    /// # Errors
    ///
    /// Returns a source- and property-located error for unknown or invalid metadata.
    pub fn from_value(source: impl Into<String>, value: &Value) -> Result<Self, MetadataError> {
        let source = source.into();
        let object = value
            .as_object()
            .ok_or_else(|| MetadataError::InvalidType {
                source: source.clone(),
                property: "$".to_owned(),
                expected: "metadata contribution must be an object",
            })?;
        if let Some(property) = object
            .keys()
            .find(|key| !METADATA_PROPERTIES.contains(&key.as_str()))
        {
            return Err(MetadataError::UnknownProperty {
                source,
                property: format!("$.{property}"),
            });
        }
        let entrypoint = object
            .get("entrypoint")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| MetadataError::InvalidType {
                        source: source.clone(),
                        property: "$.entrypoint".to_owned(),
                        expected: "entrypoint must be a string",
                    })
            })
            .transpose()?;

        let mut synthetic = object.clone();
        synthetic.remove("id");
        synthetic.remove("entrypoint");
        synthetic.insert(
            "image".to_owned(),
            Value::String("metadata-validation.invalid".to_owned()),
        );
        let bytes = serde_json::to_vec(&Value::Object(synthetic)).map_err(|_| {
            MetadataError::Normalize {
                source: source.clone(),
            }
        })?;
        let path = ConfigPath::parse(".devcontainer/image-metadata.json").map_err(|_| {
            MetadataError::Normalize {
                source: source.clone(),
            }
        })?;
        let document = parse_jsonc(&path, &bytes, ParseLimits::default()).map_err(|error| {
            MetadataError::Parse {
                source: source.clone(),
                detail: error.to_string(),
            }
        })?;
        let profile = validate_profile(&document).map_err(|error| MetadataError::Validation {
            source: source.clone(),
            property: error.diagnostic.property_path.clone(),
            detail: error.diagnostic.message.clone(),
        })?;
        let (override_command, shutdown_action) = scenario_scalars(&profile.scenario);
        Ok(Self {
            source,
            common: profile.common,
            entrypoint,
            override_command,
            shutdown_action,
        })
    }

    /// Identifies the inspected image or Feature contribution in diagnostics.
    #[must_use]
    pub fn source(&self) -> &str {
        &self.source
    }
}

/// Commands that execute sequentially in metadata/repository source order.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EffectiveLifecycle {
    /// Host initialization command. Image metadata cannot contribute this property.
    pub initialize: Vec<RawCommand>,
    /// Ordered create commands.
    pub on_create: Vec<RawCommand>,
    /// Ordered content-update commands.
    pub update_content: Vec<RawCommand>,
    /// Ordered post-create commands.
    pub post_create: Vec<RawCommand>,
    /// Ordered post-start commands.
    pub post_start: Vec<RawCommand>,
    /// Ordered post-attach commands.
    pub post_attach: Vec<RawCommand>,
}

/// Deterministic effective values produced from image metadata and repository configuration.
#[derive(Clone, PartialEq)]
pub struct EffectiveMetadata {
    /// OR-merged init request.
    pub init: Option<bool>,
    /// OR-merged privileged request.
    pub privileged: Option<bool>,
    /// Stable de-duplicating capability union.
    pub cap_add: Vec<String>,
    /// Stable de-duplicating security-option union.
    pub security_opt: Vec<String>,
    /// Feature entrypoints in source order.
    pub entrypoints: Vec<String>,
    /// Mounts in first-target order, with the last source winning conflicts.
    pub mounts: Vec<RawMount>,
    /// Lifecycle contributions in source order.
    pub lifecycle: EffectiveLifecycle,
    /// Last user value.
    pub container_user: Option<String>,
    /// Last remote user value.
    pub remote_user: Option<String>,
    /// Last user environment probe.
    pub user_env_probe: Option<UserEnvProbe>,
    /// Last UID-update choice.
    pub update_remote_user_uid: Option<bool>,
    /// Last command-override choice.
    pub override_command: Option<bool>,
    /// Last wait checkpoint.
    pub wait_for: Option<WaitFor>,
    /// Last shutdown action.
    pub shutdown_action: Option<ShutdownAction>,
    /// Per-key merged container environment. Repository keys are last.
    pub container_env: BTreeMap<String, String>,
    /// Per-key merged remote environment. Repository keys are last.
    pub remote_env: BTreeMap<String, Option<String>>,
    /// Per-port merged attributes. Entire later port entries win.
    pub ports_attributes: BTreeMap<String, PortAttributes>,
    /// Last catch-all port attributes.
    pub other_ports_attributes: Option<PortAttributes>,
    /// Stable port union with later mappings winning.
    pub forward_ports: Vec<ForwardPort>,
    /// Maximum/strongest host requirements.
    pub host_requirements: Option<HostRequirements>,
    /// Tool-owned customization objects retained separately in source order.
    pub customizations: Vec<BTreeMap<String, Value>>,
}

impl std::fmt::Debug for EffectiveMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EffectiveMetadata")
            .field(
                "container_env_keys",
                &self.container_env.keys().collect::<Vec<_>>(),
            )
            .field(
                "remote_env_keys",
                &self.remote_env.keys().collect::<Vec<_>>(),
            )
            .field("mounts", &self.mounts)
            .field("forward_ports", &self.forward_ports)
            .field("host_requirements", &self.host_requirements)
            .finish_non_exhaustive()
    }
}

/// Merges inspected image metadata in supplied order and applies repository configuration last.
///
/// # Errors
///
/// Returns a property-located error if a mount target or byte requirement cannot be normalized.
#[expect(
    clippy::too_many_lines,
    reason = "the straight-line orchestration makes the pinned merge order auditable"
)]
pub fn merge_image_metadata(
    metadata: &[ImageMetadata],
    repository: &RawProfile,
) -> Result<EffectiveMetadata, MetadataError> {
    let mut init = None;
    let mut privileged = None;
    let mut cap_add = Vec::new();
    let mut security_opt = Vec::new();
    let mut entrypoints = Vec::new();
    let mut mounts = Vec::<(String, RawMount)>::new();
    let mut lifecycle = EffectiveLifecycle::default();
    let mut container_user = None;
    let mut remote_user = None;
    let mut user_env_probe = None;
    let mut update_remote_user_uid = None;
    let mut override_command = None;
    let mut wait_for = None;
    let mut shutdown_action = None;
    let mut container_env = BTreeMap::new();
    let mut remote_env = BTreeMap::new();
    let mut ports_attributes = BTreeMap::new();
    let mut other_ports_attributes = None;
    let mut forward_ports = Vec::<(String, ForwardPort)>::new();
    let mut host_requirements = None;
    let mut customizations = Vec::new();

    for contribution in metadata {
        merge_common(
            &contribution.source,
            &contribution.common,
            &mut init,
            &mut privileged,
            &mut cap_add,
            &mut security_opt,
            &mut mounts,
            &mut lifecycle,
            &mut container_user,
            &mut remote_user,
            &mut user_env_probe,
            &mut update_remote_user_uid,
            &mut wait_for,
            &mut container_env,
            &mut remote_env,
            &mut ports_attributes,
            &mut other_ports_attributes,
            &mut forward_ports,
            &mut host_requirements,
            &mut customizations,
        )?;
        if let Some(value) = &contribution.entrypoint {
            entrypoints.push(value.clone());
        }
        if contribution.override_command.is_some() {
            override_command = contribution.override_command;
        }
        if contribution.shutdown_action.is_some() {
            shutdown_action = contribution.shutdown_action;
        }
    }
    merge_common(
        "repository",
        &repository.common,
        &mut init,
        &mut privileged,
        &mut cap_add,
        &mut security_opt,
        &mut mounts,
        &mut lifecycle,
        &mut container_user,
        &mut remote_user,
        &mut user_env_probe,
        &mut update_remote_user_uid,
        &mut wait_for,
        &mut container_env,
        &mut remote_env,
        &mut ports_attributes,
        &mut other_ports_attributes,
        &mut forward_ports,
        &mut host_requirements,
        &mut customizations,
    )?;
    let (repository_override, repository_shutdown) = scenario_scalars(&repository.scenario);
    if repository_override.is_some() {
        override_command = repository_override;
    }
    if repository_shutdown.is_some() {
        shutdown_action = repository_shutdown;
    }

    Ok(EffectiveMetadata {
        init,
        privileged,
        cap_add,
        security_opt,
        entrypoints,
        mounts: mounts.into_iter().map(|(_, mount)| mount).collect(),
        lifecycle,
        container_user,
        remote_user,
        user_env_probe,
        update_remote_user_uid,
        override_command,
        wait_for,
        shutdown_action,
        container_env,
        remote_env,
        ports_attributes,
        other_ports_attributes,
        forward_ports: forward_ports.into_iter().map(|(_, port)| port).collect(),
        host_requirements,
        customizations,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "the explicit arguments mirror the pinned property-specific merge table"
)]
fn merge_common(
    source: &str,
    common: &RawCommon,
    init: &mut Option<bool>,
    privileged: &mut Option<bool>,
    cap_add: &mut Vec<String>,
    security_opt: &mut Vec<String>,
    mounts: &mut Vec<(String, RawMount)>,
    lifecycle: &mut EffectiveLifecycle,
    container_user: &mut Option<String>,
    remote_user: &mut Option<String>,
    user_env_probe: &mut Option<UserEnvProbe>,
    update_remote_user_uid: &mut Option<bool>,
    wait_for: &mut Option<WaitFor>,
    container_env: &mut BTreeMap<String, String>,
    remote_env: &mut BTreeMap<String, Option<String>>,
    ports_attributes: &mut BTreeMap<String, PortAttributes>,
    other_ports_attributes: &mut Option<PortAttributes>,
    forward_ports: &mut Vec<(String, ForwardPort)>,
    host_requirements: &mut Option<HostRequirements>,
    customizations: &mut Vec<BTreeMap<String, Value>>,
) -> Result<(), MetadataError> {
    if let Some(value) = common.init {
        *init = Some(init.unwrap_or(false) || value);
    }
    if let Some(value) = common.privileged {
        *privileged = Some(privileged.unwrap_or(false) || value);
    }
    stable_extend(cap_add, &common.cap_add);
    stable_extend(security_opt, &common.security_opt);
    for mount in &common.mounts {
        let target = mount_target(mount).ok_or_else(|| MetadataError::InvalidMount {
            source: source.to_owned(),
            property: "$.mounts".to_owned(),
        })?;
        if let Some((_, existing)) = mounts.iter_mut().find(|(key, _)| key == &target) {
            *existing = mount.clone();
        } else {
            mounts.push((target, mount.clone()));
        }
    }
    append_lifecycle(lifecycle, &common.lifecycle);
    replace_if_some(container_user, common.container_user.as_ref());
    replace_if_some(remote_user, common.remote_user.as_ref());
    if common.user_env_probe.is_some() {
        *user_env_probe = common.user_env_probe;
    }
    if common.update_remote_user_uid.is_some() {
        *update_remote_user_uid = common.update_remote_user_uid;
    }
    if common.wait_for.is_some() {
        *wait_for = common.wait_for;
    }
    container_env.extend(
        common
            .container_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    remote_env.extend(
        common
            .remote_env
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    ports_attributes.extend(
        common
            .ports_attributes
            .iter()
            .map(|(k, v)| (k.clone(), v.clone())),
    );
    if common.other_ports_attributes.is_some() {
        other_ports_attributes.clone_from(&common.other_ports_attributes);
    }
    for port in &common.forward_ports {
        let key = forward_port_key(port);
        if let Some((_, existing)) = forward_ports
            .iter_mut()
            .find(|(candidate, _)| candidate == &key)
        {
            *existing = port.clone();
        } else {
            forward_ports.push((key, port.clone()));
        }
    }
    *host_requirements = merge_requirements(
        host_requirements.as_ref(),
        common.host_requirements.as_ref(),
        source,
    )?;
    if !common.customizations.is_empty() {
        customizations.push(common.customizations.clone());
    }
    Ok(())
}

fn stable_extend(output: &mut Vec<String>, values: &[String]) {
    let mut seen = output.iter().cloned().collect::<BTreeSet<_>>();
    output.extend(
        values
            .iter()
            .filter(|value| seen.insert((*value).clone()))
            .cloned(),
    );
}

fn replace_if_some(output: &mut Option<String>, value: Option<&String>) {
    if let Some(value) = value {
        *output = Some(value.clone());
    }
}

fn append_lifecycle(output: &mut EffectiveLifecycle, value: &LifecycleCommands) {
    if let Some(command) = &value.initialize {
        output.initialize.push(command.clone());
    }
    if let Some(command) = &value.on_create {
        output.on_create.push(command.clone());
    }
    if let Some(command) = &value.update_content {
        output.update_content.push(command.clone());
    }
    if let Some(command) = &value.post_create {
        output.post_create.push(command.clone());
    }
    if let Some(command) = &value.post_start {
        output.post_start.push(command.clone());
    }
    if let Some(command) = &value.post_attach {
        output.post_attach.push(command.clone());
    }
}

fn mount_target(mount: &RawMount) -> Option<String> {
    match mount {
        RawMount::Object { target, .. } => Some(target.clone()),
        RawMount::String(value) => value.split(',').find_map(|field| {
            let (key, target) = field.split_once('=')?;
            matches!(key.trim(), "target" | "dst" | "destination").then(|| target.trim().to_owned())
        }),
    }
}

fn forward_port_key(port: &ForwardPort) -> String {
    match port {
        ForwardPort::Container(port) | ForwardPort::Service { port, .. } => port.to_string(),
    }
}

fn merge_requirements(
    current: Option<&HostRequirements>,
    next: Option<&HostRequirements>,
    source: &str,
) -> Result<Option<HostRequirements>, MetadataError> {
    let (Some(current), Some(next)) = (current, next) else {
        let value = current.or(next);
        if let Some(requirements) = value {
            validate_requirement_sizes(requirements, source)?;
        }
        return Ok(value.cloned());
    };
    Ok(Some(HostRequirements {
        cpus: current.cpus.max(next.cpus),
        memory: max_size(
            current.memory.as_deref(),
            next.memory.as_deref(),
            source,
            "$.hostRequirements.memory",
        )?,
        storage: max_size(
            current.storage.as_deref(),
            next.storage.as_deref(),
            source,
            "$.hostRequirements.storage",
        )?,
        gpu: merge_gpu(current.gpu.as_ref(), next.gpu.as_ref(), source)?,
    }))
}

fn validate_requirement_sizes(
    requirements: &HostRequirements,
    source: &str,
) -> Result<(), MetadataError> {
    for (property, value) in [
        ("$.hostRequirements.memory", requirements.memory.as_deref()),
        (
            "$.hostRequirements.storage",
            requirements.storage.as_deref(),
        ),
    ] {
        if value.is_some_and(|value| size_bytes(value).is_none()) {
            return Err(MetadataError::InvalidRequirement {
                source: source.to_owned(),
                property: property.to_owned(),
            });
        }
    }
    if let Some(GpuRequirement::Detailed {
        memory: Some(value),
        ..
    }) = &requirements.gpu
        && size_bytes(value).is_none()
    {
        return Err(MetadataError::InvalidRequirement {
            source: source.to_owned(),
            property: "$.hostRequirements.gpu.memory".to_owned(),
        });
    }
    Ok(())
}

fn max_size(
    current: Option<&str>,
    next: Option<&str>,
    source: &str,
    property: &str,
) -> Result<Option<String>, MetadataError> {
    let Some(next) = next else {
        return Ok(current.map(str::to_owned));
    };
    let next_bytes = size_bytes(next).ok_or_else(|| MetadataError::InvalidRequirement {
        source: source.to_owned(),
        property: property.to_owned(),
    })?;
    let Some(current) = current else {
        return Ok(Some(next.to_owned()));
    };
    let current_bytes = size_bytes(current).ok_or_else(|| MetadataError::InvalidRequirement {
        source: "earlier metadata".to_owned(),
        property: property.to_owned(),
    })?;
    Ok(Some(
        if next_bytes >= current_bytes {
            next
        } else {
            current
        }
        .to_owned(),
    ))
}

fn size_bytes(value: &str) -> Option<u64> {
    let digits = value.bytes().take_while(u8::is_ascii_digit).count();
    let number = value[..digits].parse::<u64>().ok()?;
    let multiplier = match &value[digits..] {
        "" => 1,
        "kb" => 1 << 10,
        "mb" => 1 << 20,
        "gb" => 1 << 30,
        "tb" => 1_u64 << 40,
        _ => return None,
    };
    number.checked_mul(multiplier)
}

fn merge_gpu(
    current: Option<&GpuRequirement>,
    next: Option<&GpuRequirement>,
    source: &str,
) -> Result<Option<GpuRequirement>, MetadataError> {
    let (Some(current), Some(next)) = (current, next) else {
        return Ok(current.cloned().or_else(|| next.cloned()));
    };
    match (current, next) {
        (
            GpuRequirement::Detailed {
                cores: a_cores,
                memory: a_memory,
            },
            GpuRequirement::Detailed {
                cores: b_cores,
                memory: b_memory,
            },
        ) => Ok(Some(GpuRequirement::Detailed {
            cores: (*a_cores).max(*b_cores),
            memory: max_size(
                a_memory.as_deref(),
                b_memory.as_deref(),
                source,
                "$.hostRequirements.gpu.memory",
            )?,
        })),
        (GpuRequirement::Detailed { .. }, _) => Ok(Some(current.clone())),
        (_, GpuRequirement::Detailed { .. }) => Ok(Some(next.clone())),
        _ => {
            let rank = |value: &GpuRequirement| match value {
                GpuRequirement::NotRequired => 0,
                GpuRequirement::Optional => 1,
                GpuRequirement::Required => 2,
                GpuRequirement::Detailed { .. } => 3,
            };
            Ok(Some(
                if rank(next) >= rank(current) {
                    next
                } else {
                    current
                }
                .clone(),
            ))
        }
    }
}

fn scenario_scalars(scenario: &RawScenario) -> (Option<bool>, Option<ShutdownAction>) {
    match scenario {
        RawScenario::Image(value) => (
            value.options.override_command,
            value.options.shutdown_action,
        ),
        RawScenario::Dockerfile(value) => (
            value.options.override_command,
            value.options.shutdown_action,
        ),
        RawScenario::Compose(value) => (value.override_command, value.shutdown_action),
    }
}

/// A metadata parsing or merge failure that never embeds property values.
#[expect(
    missing_docs,
    reason = "variant fields repeat the documented source/property error contract"
)]
#[derive(Debug, PartialEq, Eq)]
pub enum MetadataError {
    /// The metadata contribution was not shaped as required.
    InvalidType {
        source: String,
        property: String,
        expected: &'static str,
    },
    /// A behavioral property is not part of the pinned metadata table.
    UnknownProperty { source: String, property: String },
    /// Internal normalization failed.
    Normalize { source: String },
    /// Synthetic bounded parsing failed.
    Parse { source: String, detail: String },
    /// Profile validation failed.
    Validation {
        source: String,
        property: String,
        detail: String,
    },
    /// A string mount omitted its conflict target.
    InvalidMount { source: String, property: String },
    /// A byte requirement overflowed while being normalized.
    InvalidRequirement { source: String, property: String },
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidType {
                source,
                property,
                expected,
            } => write!(
                formatter,
                "image metadata `{source}` at {property}: {expected}"
            ),
            Self::UnknownProperty { source, property } => write!(
                formatter,
                "image metadata `{source}` at {property}: unknown metadata property"
            ),
            Self::Normalize { source } => write!(
                formatter,
                "image metadata `{source}` could not be normalized"
            ),
            Self::Parse { source, detail } => write!(
                formatter,
                "image metadata `{source}` could not be parsed: {detail}"
            ),
            Self::Validation {
                source,
                property,
                detail,
            } => write!(
                formatter,
                "image metadata `{source}` at {property}: {detail}"
            ),
            Self::InvalidMount { source, property } => write!(
                formatter,
                "image metadata `{source}` at {property}: mount has no target"
            ),
            Self::InvalidRequirement { source, property } => write!(
                formatter,
                "image metadata `{source}` at {property}: byte requirement exceeds supported range"
            ),
        }
    }
}

impl std::error::Error for MetadataError {}

impl From<ProfileError> for MetadataError {
    fn from(error: ProfileError) -> Self {
        Self::Validation {
            source: "repository".to_owned(),
            property: error.diagnostic.property_path.clone(),
            detail: error.diagnostic.message.clone(),
        }
    }
}
