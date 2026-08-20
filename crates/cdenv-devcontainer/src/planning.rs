//! Pure planning for workspace, identity, environment, and container runtime settings.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use thiserror::Error;

use crate::{
    DeferredString, EffectiveMetadata, HostSubstitutionInputs, MountKind, RawMount, RawProfile,
    RawScenario, ResolvedString, SubstitutionError, SubstitutionProperty, UserEnvProbe,
    substitute_host,
};

/// A normalized absolute path inside a Linux container.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ContainerPath(String);

impl ContainerPath {
    /// Validates and normalizes an absolute container path.
    ///
    /// # Errors
    ///
    /// Returns an error for relative paths, traversal, control characters, or the root path.
    pub fn parse(value: &str) -> Result<Self, PathValidationError> {
        normalize_absolute(value, false).map(Self)
    }

    /// Borrows the normalized path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn overlaps(&self, other: &Self) -> bool {
        path_contains(self.as_str(), other.as_str()) || path_contains(other.as_str(), self.as_str())
    }
}

/// A normalized absolute host bind-mount source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HostMountSource(String);

impl HostMountSource {
    /// Validates and normalizes an absolute host path without accessing the filesystem.
    ///
    /// # Errors
    ///
    /// Returns an error for relative paths, traversal, or control characters.
    pub fn parse(value: &str) -> Result<Self, PathValidationError> {
        normalize_absolute(value, true).map(Self)
    }

    /// Borrows the normalized path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Failure to validate a host or container path.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum PathValidationError {
    /// The path was empty or relative.
    #[error("path must be absolute")]
    NotAbsolute,
    /// The path contained a parent component.
    #[error("path traversal is not allowed")]
    Traversal,
    /// The path contained a control character.
    #[error("path contains a control character")]
    ControlCharacter,
    /// A container target was the root directory.
    #[error("container path must not be the root directory")]
    RootTarget,
}

/// Metadata learned by a host adapter without mutating or starting a container.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScenarioMetadata<'a> {
    /// Image `Config.User` or Compose service `user`, when present.
    pub container_user: Option<&'a str>,
}

/// Inputs injected into pure runtime planning.
pub struct RuntimePlanningInputs<'a> {
    /// Canonical UTF-8 checkout path.
    pub local_workspace_folder: &'a str,
    /// Host environment snapshot used by allowed substitutions.
    pub local_env: &'a BTreeMap<String, String>,
    /// Stable cdenv labels used by `${devcontainerId}`.
    pub identity_labels: &'a crate::StableIdentityLabels,
    /// Image or Compose service defaults obtained by the host adapter.
    pub scenario_metadata: ScenarioMetadata<'a>,
    /// Container targets reserved for cdenv assets.
    pub cdenv_owned_targets: &'a [ContainerPath],
    /// Host UID/GID, retained only in the non-serializable execution plan.
    pub host_user: Option<HostUserIdentity>,
}

/// Host identity used by a later UID/GID image-generation step.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HostUserIdentity {
    uid: u32,
    gid: u32,
}

impl HostUserIdentity {
    /// Creates an injected host identity. This type is deliberately not serializable.
    #[must_use]
    pub const fn new(uid: u32, gid: u32) -> Self {
        Self { uid, gid }
    }

    /// Returns the UID for immediate image generation.
    #[must_use]
    pub const fn uid(self) -> u32 {
        self.uid
    }

    /// Returns the GID for immediate image generation.
    #[must_use]
    pub const fn gid(self) -> u32 {
        self.gid
    }
}

impl std::fmt::Debug for HostUserIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostUserIdentity(<redacted>)")
    }
}

/// A validated container identity accepted by Docker and exec planning.
#[derive(Clone, PartialEq, Eq)]
pub struct ContainerUser(String);

impl ContainerUser {
    /// Validates a user or `user:group` value.
    ///
    /// # Errors
    ///
    /// Rejects empty components and characters unsafe in Docker argument values.
    pub fn parse(value: &str) -> Result<Self, UserValidationError> {
        if value.is_empty()
            || value.chars().any(|character| {
                character.is_control() || character.is_whitespace() || character == ','
            })
            || value.split(':').any(str::is_empty)
            || value.matches(':').count() > 1
        {
            return Err(UserValidationError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the Docker user expression.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn account(&self) -> &str {
        self.0.split(':').next().unwrap_or_default()
    }

    fn is_root(&self) -> bool {
        matches!(self.account(), "root" | "0")
    }

    fn is_numeric(&self) -> bool {
        self.account().bytes().all(|byte| byte.is_ascii_digit())
    }
}

impl std::fmt::Debug for ContainerUser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContainerUser(<redacted>)")
    }
}

/// An invalid container user expression.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("container user must be a non-empty user or user:group without whitespace or controls")]
pub struct UserValidationError;

/// Why a UID/GID layer is or is not required.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UidUpdateIntent {
    /// No update was requested or a root/numeric identity cannot be safely rewritten.
    None(UidUpdateSkipReason),
    /// Update this named account to an injected host identity in a later image-generation step.
    UpdateNamedUser {
        /// Named account to update.
        user: ContainerUser,
        /// Sensitive host identity, never included in summaries.
        host: HostUserIdentity,
    },
}

/// Stable reason that UID/GID rewriting is omitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UidUpdateSkipReason {
    /// `updateRemoteUserUID` was false.
    Disabled,
    /// Host identity was not measurable.
    HostIdentityUnknown,
    /// Root is never rewritten.
    RootUser,
    /// Numeric users cannot be safely mapped to a named account.
    NumericUser,
}

/// One normalized mount option not owned by source, target, or type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MountOption {
    /// Option name.
    pub name: String,
    /// Optional option value.
    pub value: Option<String>,
}

/// A validated normalized mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedMount {
    /// Bind or named-volume mount.
    pub kind: MountKind,
    /// Source path/name. Anonymous volumes have no source.
    pub source: Option<String>,
    /// Absolute container target.
    pub target: ContainerPath,
    /// Additional Docker mount options in source order.
    pub options: Vec<MountOption>,
}

/// Authoritative workspace location and mount.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspacePlan {
    /// Folder used by lifecycle commands and interactive processes.
    pub folder: ContainerPath,
    /// Workspace mount or deterministic Compose override.
    pub mount: PlannedMount,
}

/// Environment values split by their create-time and runtime-only stages.
pub struct EnvironmentPlan {
    container: BTreeMap<String, ResolvedString>,
    remote: BTreeMap<String, Option<DeferredString>>,
}

impl EnvironmentPlan {
    /// Borrows create-time container environment values for immediate execution.
    #[must_use]
    pub fn container(&self) -> &BTreeMap<String, ResolvedString> {
        &self.container
    }

    /// Borrows remote values, which may require actual container environment capture.
    #[must_use]
    pub fn remote(&self) -> &BTreeMap<String, Option<DeferredString>> {
        &self.remote
    }

    /// Returns a safe summary containing only counts and stage requirements.
    #[must_use]
    pub fn summary(&self) -> EnvironmentSummary {
        EnvironmentSummary {
            container_entries: self.container.len(),
            remote_entries: self.remote.len(),
            runtime_container_environment_required: self
                .remote
                .values()
                .flatten()
                .any(DeferredString::requires_container_environment),
        }
    }
}

impl std::fmt::Debug for EnvironmentPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentPlan")
            .field("summary", &self.summary())
            .finish()
    }
}

/// Serializable environment stage information with no keys or values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct EnvironmentSummary {
    /// Number of create-time entries.
    pub container_entries: usize,
    /// Number of remote runtime entries.
    pub remote_entries: usize,
    /// Whether `${containerEnv:...}` must be reconciled after start.
    pub runtime_container_environment_required: bool,
}

/// Pure create/runtime settings not delegated to Docker argument parsing.
pub struct RuntimePlan {
    /// Authoritative workspace folder and mount.
    pub workspace: WorkspacePlan,
    /// Additional metadata/repository mounts.
    pub mounts: Vec<PlannedMount>,
    /// User applied to the created container.
    pub container_user: ContainerUser,
    /// User applied to lifecycle and remote processes.
    pub remote_user: ContainerUser,
    /// Explicit decision for a later UID/GID layer.
    pub uid_update: UidUpdateIntent,
    /// Staged environment values.
    pub environment: EnvironmentPlan,
    /// Environment probe, including the profile default.
    pub user_env_probe: UserEnvProbe,
    /// Whether the scenario command is replaced.
    pub override_command: bool,
    /// Whether Docker init is requested.
    pub init: bool,
    /// Whether privileged mode is requested.
    pub privileged: bool,
    /// Stable merged Linux capabilities.
    pub cap_add: Vec<String>,
    /// Stable merged security options.
    pub security_opt: Vec<String>,
}

impl RuntimePlan {
    /// Produces a persistence-safe summary without paths, users, environment keys, or values.
    #[must_use]
    pub fn summary(&self) -> RuntimePlanSummary {
        RuntimePlanSummary {
            additional_mounts: self.mounts.len(),
            environment: self.environment.summary(),
            user_env_probe: self.user_env_probe,
            uid_update: match self.uid_update {
                UidUpdateIntent::None(reason) => UidUpdateSummary::Skipped(reason),
                UidUpdateIntent::UpdateNamedUser { .. } => UidUpdateSummary::Required,
            },
            override_command: self.override_command,
            init: self.init,
            privileged: self.privileged,
            capabilities: self.cap_add.len(),
            security_options: self.security_opt.len(),
        }
    }
}

impl std::fmt::Debug for RuntimePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RuntimePlan")
            .field("summary", &self.summary())
            .finish()
    }
}

/// Persistence-safe runtime summary.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RuntimePlanSummary {
    /// Number of non-workspace mounts.
    pub additional_mounts: usize,
    /// Environment stage information.
    pub environment: EnvironmentSummary,
    /// Probe variant, not captured values.
    pub user_env_probe: UserEnvProbe,
    /// UID-update decision without account or host identity.
    pub uid_update: UidUpdateSummary,
    /// Effective command override.
    pub override_command: bool,
    /// Effective init request.
    pub init: bool,
    /// Effective privileged request.
    pub privileged: bool,
    /// Number of requested capabilities.
    pub capabilities: usize,
    /// Number of requested security options.
    pub security_options: usize,
}

/// Safe UID-update summary.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum UidUpdateSummary {
    /// A named user update is required.
    Required,
    /// No update, with a non-sensitive reason.
    Skipped(UidUpdateSkipReason),
}

/// Builds the effective pure runtime plan after image metadata has been merged.
///
/// # Errors
///
/// Returns a property-path error for invalid substitutions, paths, mounts, users, environment
/// names, or conflicts with cdenv-owned targets.
#[expect(
    clippy::too_many_lines,
    reason = "straight-line assembly keeps property paths and effective defaults auditable"
)]
pub fn plan_runtime(
    profile: &RawProfile,
    effective: &EffectiveMetadata,
    inputs: &RuntimePlanningInputs<'_>,
) -> Result<RuntimePlan, PlanningError> {
    let local_source = HostMountSource::parse(inputs.local_workspace_folder).map_err(|_| {
        PlanningError::new(
            "$",
            PlanningErrorKind::InvalidPath,
            "local workspace path must be absolute and traversal-free",
        )
    })?;
    let default_folder = format!(
        "/workspaces/{}",
        inputs
            .local_workspace_folder
            .trim_end_matches('/')
            .rsplit('/')
            .next()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| PlanningError::new(
                "$.workspaceFolder",
                PlanningErrorKind::InvalidPath,
                "workspace basename must not be empty"
            ))?
    );
    let raw_folder = match &profile.scenario {
        RawScenario::Image(value) => value
            .options
            .workspace_folder
            .as_deref()
            .unwrap_or(&default_folder),
        RawScenario::Dockerfile(value) => value
            .options
            .workspace_folder
            .as_deref()
            .unwrap_or(&default_folder),
        RawScenario::Compose(value) => &value.workspace_folder,
    };
    let provisional_context = HostSubstitutionInputs {
        local_workspace_folder: local_source.as_str(),
        container_workspace_folder: &default_folder,
        local_env: inputs.local_env,
        identity_labels: inputs.identity_labels,
    };
    let folder_value = immediate_substitution(
        "$.workspaceFolder",
        SubstitutionProperty::WorkspaceFolder,
        raw_folder,
        &provisional_context,
    )?;
    let folder = ContainerPath::parse(folder_value.expose()).map_err(|_| {
        PlanningError::new(
            "$.workspaceFolder",
            PlanningErrorKind::InvalidPath,
            "workspace folder must be an absolute traversal-free container path",
        )
    })?;
    let context = HostSubstitutionInputs {
        local_workspace_folder: local_source.as_str(),
        container_workspace_folder: folder.as_str(),
        local_env: inputs.local_env,
        identity_labels: inputs.identity_labels,
    };

    let workspace_mount = match &profile.scenario {
        RawScenario::Image(value) => value.options.workspace_mount.as_deref(),
        RawScenario::Dockerfile(value) => value.options.workspace_mount.as_deref(),
        RawScenario::Compose(_) => None,
    };
    let workspace_mount = if let Some(value) = workspace_mount {
        parse_string_mount(
            value,
            "$.workspaceMount",
            SubstitutionProperty::WorkspaceMount,
            &context,
        )?
    } else {
        PlannedMount {
            kind: MountKind::Bind,
            source: Some(local_source.as_str().to_owned()),
            target: folder.clone(),
            options: Vec::new(),
        }
    };
    validate_workspace_mount(&workspace_mount, &folder, &local_source)?;
    reject_owned_target(
        &workspace_mount.target,
        inputs.cdenv_owned_targets,
        "$.workspaceMount",
    )?;

    let mut mounts = Vec::with_capacity(effective.mounts.len());
    let mut targets = BTreeSet::new();
    for (index, mount) in effective.mounts.iter().enumerate() {
        let property = format!("$.mounts[{index}]");
        let mount = parse_mount(mount, &property, &context)?;
        if mount.target == workspace_mount.target {
            return Err(PlanningError::new(
                property,
                PlanningErrorKind::MountConflict,
                "additional mount conflicts with the authoritative workspace target",
            ));
        }
        reject_owned_target(&mount.target, inputs.cdenv_owned_targets, &property)?;
        if !targets.insert(mount.target.clone()) {
            return Err(PlanningError::new(
                property,
                PlanningErrorKind::MountConflict,
                "additional mount target is duplicated",
            ));
        }
        mounts.push(mount);
    }

    let container_user_raw = effective
        .container_user
        .as_deref()
        .or(inputs.scenario_metadata.container_user)
        .filter(|value| !value.is_empty())
        .unwrap_or("root");
    let container_user_value = immediate_substitution(
        "$.containerUser",
        SubstitutionProperty::ContainerUser,
        container_user_raw,
        &context,
    )?;
    let container_user = ContainerUser::parse(container_user_value.expose()).map_err(|_| {
        PlanningError::new(
            "$.containerUser",
            PlanningErrorKind::InvalidUser,
            "invalid effective container user",
        )
    })?;
    let remote_user_raw = effective
        .remote_user
        .as_deref()
        .unwrap_or(container_user.as_str());
    let remote_user_value = immediate_substitution(
        "$.remoteUser",
        SubstitutionProperty::RemoteUser,
        remote_user_raw,
        &context,
    )?;
    let remote_user = ContainerUser::parse(remote_user_value.expose()).map_err(|_| {
        PlanningError::new(
            "$.remoteUser",
            PlanningErrorKind::InvalidUser,
            "invalid effective remote user",
        )
    })?;
    let uid_update = uid_update_intent(
        effective.update_remote_user_uid.unwrap_or(true),
        &remote_user,
        inputs.host_user,
    );

    let environment = plan_environment(effective, &context)?;
    validate_runtime_strings(&effective.cap_add, "$.capAdd")?;
    validate_runtime_strings(&effective.security_opt, "$.securityOpt")?;
    let override_default = !matches!(profile.scenario, RawScenario::Compose(_));

    Ok(RuntimePlan {
        workspace: WorkspacePlan {
            folder,
            mount: workspace_mount,
        },
        mounts,
        container_user,
        remote_user,
        uid_update,
        environment,
        user_env_probe: effective
            .user_env_probe
            .unwrap_or(UserEnvProbe::LoginInteractiveShell),
        override_command: effective.override_command.unwrap_or(override_default),
        init: effective.init.unwrap_or(false),
        privileged: effective.privileged.unwrap_or(false),
        cap_add: effective.cap_add.clone(),
        security_opt: effective.security_opt.clone(),
    })
}

fn plan_environment(
    effective: &EffectiveMetadata,
    context: &HostSubstitutionInputs<'_>,
) -> Result<EnvironmentPlan, PlanningError> {
    let mut container = BTreeMap::new();
    for (name, value) in &effective.container_env {
        validate_environment_name(name, &format!("$.containerEnv[\"{name}\"]"))?;
        container.insert(
            name.clone(),
            immediate_substitution(
                &format!("$.containerEnv[\"{name}\"]"),
                SubstitutionProperty::ContainerEnv,
                value,
                context,
            )?,
        );
    }
    let mut remote = BTreeMap::new();
    for (name, value) in &effective.remote_env {
        let property = format!("$.remoteEnv[\"{name}\"]");
        validate_environment_name(name, &property)?;
        let value = value
            .as_deref()
            .map(|value| substitute_host(SubstitutionProperty::RemoteEnv, value, context))
            .transpose()
            .map_err(|error| substitution_error(&property, error))?;
        remote.insert(name.clone(), value);
    }
    Ok(EnvironmentPlan { container, remote })
}

fn validate_environment_name(name: &str, property: &str) -> Result<(), PlanningError> {
    let valid = name.bytes().enumerate().all(|(index, byte)| {
        byte == b'_' || byte.is_ascii_alphabetic() || (index > 0 && byte.is_ascii_digit())
    });
    if valid && !name.is_empty() {
        Ok(())
    } else {
        Err(PlanningError::new(
            property,
            PlanningErrorKind::InvalidEnvironmentName,
            "environment name must match [A-Za-z_][A-Za-z0-9_]*",
        ))
    }
}

fn validate_runtime_strings(values: &[String], parent: &str) -> Result<(), PlanningError> {
    if let Some((index, _)) = values
        .iter()
        .enumerate()
        .find(|(_, value)| value.is_empty() || value.chars().any(char::is_control))
    {
        Err(PlanningError::new(
            format!("{parent}[{index}]"),
            PlanningErrorKind::InvalidRuntimeSetting,
            "runtime setting must not be empty or contain control characters",
        ))
    } else {
        Ok(())
    }
}

fn uid_update_intent(
    enabled: bool,
    remote_user: &ContainerUser,
    host: Option<HostUserIdentity>,
) -> UidUpdateIntent {
    if !enabled {
        UidUpdateIntent::None(UidUpdateSkipReason::Disabled)
    } else if remote_user.is_root() {
        UidUpdateIntent::None(UidUpdateSkipReason::RootUser)
    } else if remote_user.is_numeric() {
        UidUpdateIntent::None(UidUpdateSkipReason::NumericUser)
    } else if let Some(host) = host {
        UidUpdateIntent::UpdateNamedUser {
            user: remote_user.clone(),
            host,
        }
    } else {
        UidUpdateIntent::None(UidUpdateSkipReason::HostIdentityUnknown)
    }
}

fn validate_workspace_mount(
    mount: &PlannedMount,
    folder: &ContainerPath,
    local_workspace: &HostMountSource,
) -> Result<(), PlanningError> {
    if !path_contains(mount.target.as_str(), folder.as_str()) {
        return Err(PlanningError::new(
            "$.workspaceFolder",
            PlanningErrorKind::WorkspaceContainment,
            "workspace folder must be the workspace mount target or a child of it",
        ));
    }
    if mount.kind == MountKind::Bind {
        let source = mount.source.as_deref().ok_or_else(|| {
            PlanningError::new(
                "$.workspaceMount",
                PlanningErrorKind::InvalidMount,
                "workspace bind mount requires a source",
            )
        })?;
        let source = HostMountSource::parse(source).map_err(|_| {
            PlanningError::new(
                "$.workspaceMount",
                PlanningErrorKind::InvalidPath,
                "workspace bind source must be an absolute traversal-free host path",
            )
        })?;
        if !path_contains(local_workspace.as_str(), source.as_str()) {
            return Err(PlanningError::new(
                "$.workspaceMount",
                PlanningErrorKind::WorkspaceContainment,
                "workspace bind source must remain within the canonical local workspace",
            ));
        }
    }
    Ok(())
}

fn reject_owned_target(
    target: &ContainerPath,
    owned: &[ContainerPath],
    property: &str,
) -> Result<(), PlanningError> {
    if owned.iter().any(|candidate| target.overlaps(candidate)) {
        Err(PlanningError::new(
            property,
            PlanningErrorKind::OwnedTargetConflict,
            "mount target overlaps a cdenv-owned container target",
        ))
    } else {
        Ok(())
    }
}

fn parse_mount(
    mount: &RawMount,
    property: &str,
    context: &HostSubstitutionInputs<'_>,
) -> Result<PlannedMount, PlanningError> {
    match mount {
        RawMount::String(value) => {
            parse_string_mount(value, property, SubstitutionProperty::Mount, context)
        }
        RawMount::Object {
            kind,
            source,
            target,
        } => {
            let source = source
                .as_deref()
                .map(|value| {
                    immediate_substitution(property, SubstitutionProperty::Mount, value, context)
                })
                .transpose()?
                .map(|value| value.expose().to_owned());
            let target =
                immediate_substitution(property, SubstitutionProperty::Mount, target, context)?;
            normalize_mount(*kind, source, target.expose(), Vec::new(), property)
        }
    }
}

fn parse_string_mount(
    value: &str,
    property: &str,
    substitution_property: SubstitutionProperty,
    context: &HostSubstitutionInputs<'_>,
) -> Result<PlannedMount, PlanningError> {
    let value = immediate_substitution(property, substitution_property, value, context)?;
    let mut kind = None;
    let mut source = None;
    let mut target = None;
    let mut options = Vec::new();
    for field in value.expose().split(',') {
        let (name, option_value) = field
            .split_once('=')
            .map_or((field, None), |(name, value)| (name, Some(value)));
        let name = name.trim();
        let option_value = option_value.map(str::trim);
        if name.is_empty() || option_value.is_some_and(str::is_empty) {
            return Err(PlanningError::new(
                property,
                PlanningErrorKind::InvalidMount,
                "mount contains an empty option",
            ));
        }
        match name {
            "type" => {
                if kind.is_some() {
                    return Err(PlanningError::new(
                        property,
                        PlanningErrorKind::InvalidMount,
                        "mount type is duplicated",
                    ));
                }
                kind = match option_value {
                    Some("bind") => Some(MountKind::Bind),
                    Some("volume") => Some(MountKind::Volume),
                    _ => {
                        return Err(PlanningError::new(
                            property,
                            PlanningErrorKind::InvalidMount,
                            "mount type must be bind or volume",
                        ));
                    }
                };
            }
            "source" | "src" => set_once(
                &mut source,
                option_value,
                property,
                "mount source is duplicated",
            )?,
            "target" | "dst" | "destination" => set_once(
                &mut target,
                option_value,
                property,
                "mount target is duplicated",
            )?,
            _ => options.push(MountOption {
                name: name.to_owned(),
                value: option_value.map(str::to_owned),
            }),
        }
    }
    let kind = kind.ok_or_else(|| {
        PlanningError::new(
            property,
            PlanningErrorKind::InvalidMount,
            "mount type is required",
        )
    })?;
    let target = target.ok_or_else(|| {
        PlanningError::new(
            property,
            PlanningErrorKind::InvalidMount,
            "mount target is required",
        )
    })?;
    normalize_mount(kind, source, &target, options, property)
}

fn set_once(
    output: &mut Option<String>,
    value: Option<&str>,
    property: &str,
    message: &'static str,
) -> Result<(), PlanningError> {
    let value = value.ok_or_else(|| {
        PlanningError::new(
            property,
            PlanningErrorKind::InvalidMount,
            "mount field requires a value",
        )
    })?;
    if output.replace(value.to_owned()).is_some() {
        Err(PlanningError::new(
            property,
            PlanningErrorKind::InvalidMount,
            message,
        ))
    } else {
        Ok(())
    }
}

fn normalize_mount(
    kind: MountKind,
    source: Option<String>,
    target: &str,
    options: Vec<MountOption>,
    property: &str,
) -> Result<PlannedMount, PlanningError> {
    let source = if kind == MountKind::Bind {
        let value = source.as_deref().ok_or_else(|| {
            PlanningError::new(
                property,
                PlanningErrorKind::InvalidMount,
                "bind mount requires a source",
            )
        })?;
        let normalized = HostMountSource::parse(value).map_err(|_| {
            PlanningError::new(
                property,
                PlanningErrorKind::InvalidPath,
                "bind source must be an absolute traversal-free host path",
            )
        })?;
        Some(normalized.as_str().to_owned())
    } else {
        if source.as_deref().is_some_and(|value| {
            !value
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
                || !value
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-'))
        }) {
            return Err(PlanningError::new(
                property,
                PlanningErrorKind::InvalidMount,
                "volume source must be a safe Docker volume name",
            ));
        }
        source
    };
    let target = ContainerPath::parse(target).map_err(|_| {
        PlanningError::new(
            property,
            PlanningErrorKind::InvalidPath,
            "mount target must be an absolute traversal-free container path",
        )
    })?;
    Ok(PlannedMount {
        kind,
        source,
        target,
        options,
    })
}

fn immediate_substitution(
    property_path: &str,
    property: SubstitutionProperty,
    value: &str,
    context: &HostSubstitutionInputs<'_>,
) -> Result<ResolvedString, PlanningError> {
    substitute_host(property, value, context)
        .map(|value| value.resolve(&BTreeMap::new()))
        .map_err(|error| substitution_error(property_path, error))
}

fn substitution_error(property: &str, _error: SubstitutionError) -> PlanningError {
    PlanningError::new(
        property,
        PlanningErrorKind::Substitution,
        "unsupported or malformed substitution",
    )
}

fn normalize_absolute(value: &str, allow_root: bool) -> Result<String, PathValidationError> {
    if !value.starts_with('/') {
        return Err(PathValidationError::NotAbsolute);
    }
    if value.chars().any(char::is_control) {
        return Err(PathValidationError::ControlCharacter);
    }
    let mut components = Vec::new();
    for component in value.split('/') {
        match component {
            "" | "." => {}
            ".." => return Err(PathValidationError::Traversal),
            value => components.push(value),
        }
    }
    if components.is_empty() && !allow_root {
        return Err(PathValidationError::RootTarget);
    }
    Ok(format!("/{}", components.join("/")))
}

fn path_contains(parent: &str, child: &str) -> bool {
    parent == child
        || (child.starts_with(parent)
            && (parent == "/" || child.as_bytes().get(parent.len()) == Some(&b'/')))
}

/// Stable category of pure planning failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanningErrorKind {
    /// A path was unsafe or invalid.
    InvalidPath,
    /// A mount was malformed.
    InvalidMount,
    /// Two mounts use the same target.
    MountConflict,
    /// A workspace source/folder escaped its authoritative mount.
    WorkspaceContainment,
    /// A target overlaps cdenv-owned container state.
    OwnedTargetConflict,
    /// A user expression was invalid.
    InvalidUser,
    /// An environment key was invalid.
    InvalidEnvironmentName,
    /// A substitution was invalid at this stage.
    Substitution,
    /// A capability or security option was invalid.
    InvalidRuntimeSetting,
}

/// Property-located planning failure that never includes rejected values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{property_path}: {message}")]
pub struct PlanningError {
    /// Exact Dev Container property path.
    pub property_path: String,
    /// Stable machine-readable category.
    pub kind: PlanningErrorKind,
    message: &'static str,
}

impl PlanningError {
    fn new(
        property_path: impl Into<String>,
        kind: PlanningErrorKind,
        message: &'static str,
    ) -> Self {
        Self {
            property_path: property_path.into(),
            kind,
            message,
        }
    }
}
