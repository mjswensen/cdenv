//! Versioned workspace intent and last-successful-generation persistence.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use cdenv_core::{
    AgentBuildId, ContainerArchitecture, ContainerId, ForegroundOperation, ForwardingEndpoint,
    ForwardingEndpointAssignment, GenerationId, InstallationId, ProfileId, ProtocolVersion,
    TcpPort, WorkspaceName,
};
use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;
use url::Url;

use crate::{
    KeyedDigest, ManagedMode, ManagedPathError, ManagedPathKind, ManagedPathState, StorageError,
    atomic_write, inspect_managed_path,
};

/// The workspace-state schema written by this build.
pub const WORKSPACE_STATE_SCHEMA_VERSION: u32 = 1;
const LEGACY_SCHEMA_VERSION: u32 = 0;
const MAX_SUMMARY_BYTES: usize = 1024;

/// Three keyed fingerprints for independently reconcilable plan categories.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PlanFingerprints {
    build: KeyedDigest,
    create: KeyedDigest,
    runtime: KeyedDigest,
}

impl PlanFingerprints {
    /// Groups independently keyed plan-category fingerprints.
    #[must_use]
    pub const fn new(build: KeyedDigest, create: KeyedDigest, runtime: KeyedDigest) -> Self {
        Self {
            build,
            create,
            runtime,
        }
    }

    /// Returns the build-category fingerprint.
    #[must_use]
    pub const fn build(&self) -> &KeyedDigest {
        &self.build
    }

    /// Returns the create-category fingerprint.
    #[must_use]
    pub const fn create(&self) -> &KeyedDigest {
        &self.create
    }

    /// Returns the runtime-category fingerprint.
    #[must_use]
    pub const fn runtime(&self) -> &KeyedDigest {
        &self.runtime
    }
}

/// A repository source safe to persist after credential-bearing URL parts are removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SanitizedRepositorySource(String);

impl SanitizedRepositorySource {
    /// Sanitizes a repository source for persistence.
    ///
    /// HTTP(S) user information, query strings, and fragments are removed.
    /// SSH usernames are retained because they identify transport behavior;
    /// passwords and URL query/fragment data are always removed.
    #[must_use]
    pub fn sanitize(source: &str) -> Self {
        let source = source.replace(char::is_control, "");
        let Ok(mut url) = Url::parse(&source) else {
            return Self(source);
        };
        if matches!(url.scheme(), "http" | "https") {
            let _ = url.set_username("");
        }
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        Self(url.into())
    }

    /// Returns the sanitized source description.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SanitizedRepositorySource {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let sanitized = Self::sanitize(&value);
        if sanitized.0 != value {
            return Err(serde::de::Error::custom(
                "repository source contains credential-bearing or nonessential URL data",
            ));
        }
        Ok(sanitized)
    }
}

/// A concise error summary with recognized secret values removed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct SanitizedSummary(String);

impl SanitizedSummary {
    /// Redacts recognized values and bounds a summary before persistence.
    #[must_use]
    pub fn redact<'a>(summary: &str, secrets: impl IntoIterator<Item = &'a str>) -> Self {
        let mut sanitized = summary.replace(char::is_control, " ");
        for secret in secrets {
            if !secret.is_empty() {
                sanitized = sanitized.replace(secret, "[REDACTED]");
            }
        }
        if sanitized.len() > MAX_SUMMARY_BYTES {
            let mut boundary = MAX_SUMMARY_BYTES;
            while !sanitized.is_char_boundary(boundary) {
                boundary -= 1;
            }
            sanitized.truncate(boundary);
        }
        Self(sanitized)
    }

    /// Returns the bounded sanitized summary.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for SanitizedSummary {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        if value.len() > MAX_SUMMARY_BYTES || value.chars().any(char::is_control) {
            return Err(serde::de::Error::custom(
                "persisted error summary is unbounded or contains control characters",
            ));
        }
        Ok(Self(value))
    }
}

/// An opaque RFC 3339 timestamp supplied by the application clock.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct StateTimestamp(String);

impl StateTimestamp {
    /// Validates an RFC 3339-shaped timestamp.
    ///
    /// This boundary intentionally preserves the clock's exact representation;
    /// it validates structure rather than normalizing time zones or precision.
    ///
    /// # Errors
    ///
    /// Returns [`StateTimestampError`] for empty, control-bearing, or clearly
    /// non-RFC-3339 values.
    pub fn parse(value: &str) -> Result<Self, StateTimestampError> {
        let has_zone = value.ends_with('Z')
            || value
                .get(10..)
                .is_some_and(|suffix| suffix.contains('+') || suffix.rfind('-').is_some());
        if value.len() < 20
            || value.as_bytes().get(4) != Some(&b'-')
            || value.as_bytes().get(7) != Some(&b'-')
            || value.as_bytes().get(10) != Some(&b'T')
            || value.chars().any(char::is_control)
            || !has_zone
        {
            return Err(StateTimestampError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the original validated timestamp.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A malformed persisted timestamp.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a state timestamp must use RFC 3339 date-time syntax")]
pub struct StateTimestampError;

impl<'de> Deserialize<'de> for StateTimestamp {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// A validated repository-relative selected configuration path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct DesiredConfigPath(String);

impl DesiredConfigPath {
    /// Validates a selected configuration path lexically.
    ///
    /// # Errors
    ///
    /// Returns [`DesiredConfigPathError`] for an empty, absolute,
    /// parent-traversing, non-UTF-8, or control-bearing value.
    pub fn parse(value: &str) -> Result<Self, DesiredConfigPathError> {
        let path = Path::new(value);
        if value.is_empty() {
            return Err(DesiredConfigPathError::Empty);
        }
        if value.chars().any(char::is_control) {
            return Err(DesiredConfigPathError::Control);
        }
        if path.is_absolute() {
            return Err(DesiredConfigPathError::Absolute);
        }
        let mut has_file_component = false;
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(DesiredConfigPathError::ParentTraversal);
            }
            if matches!(component, Component::Prefix(_) | Component::RootDir) {
                return Err(DesiredConfigPathError::Absolute);
            }
            if matches!(component, Component::Normal(_)) {
                has_file_component = true;
            }
        }
        if !has_file_component {
            return Err(DesiredConfigPathError::Empty);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the repository-relative path text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A malformed persisted selected-configuration path.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DesiredConfigPathError {
    /// No file was named.
    #[error("desired configuration path is empty")]
    Empty,
    /// An absolute path was supplied.
    #[error("desired configuration path must be repository-relative")]
    Absolute,
    /// Parent traversal was supplied.
    #[error("desired configuration path must not contain `..`")]
    ParentTraversal,
    /// Control text cannot be represented safely.
    #[error("desired configuration path contains a control character")]
    Control,
}

impl<'de> Deserialize<'de> for DesiredConfigPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

/// Persisted foreground-operation recovery data.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationState {
    kind: ForegroundOperation,
    id: Option<String>,
    started_at: Option<StateTimestamp>,
}

impl OperationState {
    /// Creates an idle operation record.
    #[must_use]
    pub const fn idle() -> Self {
        Self {
            kind: ForegroundOperation::Idle,
            id: None,
            started_at: None,
        }
    }

    /// Creates a non-idle operation recovery record.
    ///
    /// # Errors
    ///
    /// Returns [`OperationStateError`] for an idle kind or an empty/control-bearing ID.
    pub fn active(
        kind: ForegroundOperation,
        id: String,
        started_at: StateTimestamp,
    ) -> Result<Self, OperationStateError> {
        if kind == ForegroundOperation::Idle {
            return Err(OperationStateError::IdleHasDetails);
        }
        if id.is_empty() || id.chars().any(char::is_control) {
            return Err(OperationStateError::InvalidId);
        }
        Ok(Self {
            kind,
            id: Some(id),
            started_at: Some(started_at),
        })
    }

    /// Returns the persisted operation kind, not its live lock status.
    #[must_use]
    pub const fn kind(&self) -> ForegroundOperation {
        self.kind
    }
}

/// An inconsistent foreground-operation recovery record.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum OperationStateError {
    /// Idle records cannot carry operation identity or timing.
    #[error("an idle operation must not carry operation details")]
    IdleHasDetails,
    /// Non-idle records require an identifier and start time.
    #[error("a non-idle operation must carry an identifier and start time")]
    ActiveMissingDetails,
    /// An operation ID must be nonempty and control-free.
    #[error("an operation ID must be nonempty and contain no control characters")]
    InvalidId,
}

impl<'de> Deserialize<'de> for OperationState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Representation {
            kind: ForegroundOperation,
            id: Option<String>,
            started_at: Option<StateTimestamp>,
        }

        let value = Representation::deserialize(deserializer)?;
        match (value.kind, value.id, value.started_at) {
            (ForegroundOperation::Idle, None, None) => Ok(Self::idle()),
            (ForegroundOperation::Idle, _, _) => Err(serde::de::Error::custom(
                OperationStateError::IdleHasDetails,
            )),
            (kind, Some(id), Some(started_at)) => {
                Self::active(kind, id, started_at).map_err(serde::de::Error::custom)
            }
            (_, _, _) => Err(serde::de::Error::custom(
                OperationStateError::ActiveMissingDetails,
            )),
        }
    }
}

/// The container creation scenario of an active generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ActiveScenario {
    /// A direct image-based container.
    Image,
    /// A generated Dockerfile-based container.
    Dockerfile,
    /// An isolated Compose project and its cdenv-managed service set.
    Compose {
        /// The installation/workspace-scoped Compose project name.
        project: String,
        /// Services cdenv starts and reconciles.
        #[serde(rename = "managedServices")]
        managed_services: Vec<String>,
    },
}

impl ActiveScenario {
    /// Returns Compose project and managed services for a Compose scenario.
    #[must_use]
    pub fn compose(&self) -> Option<(&str, &[String])> {
        match self {
            Self::Compose {
                project,
                managed_services,
            } => Some((project, managed_services)),
            Self::Image | Self::Dockerfile => None,
        }
    }
}

/// One persisted lifecycle execution stage.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleStage {
    /// `initializeCommand` on the host.
    InitializeCommand,
    /// `onCreateCommand` in a new generation.
    OnCreateCommand,
    /// `updateContentCommand` in a new generation.
    UpdateContentCommand,
    /// `postCreateCommand` in a new generation.
    PostCreateCommand,
    /// `postStartCommand` for a start.
    PostStartCommand,
    /// `postAttachCommand` for one transport.
    PostAttachCommand,
}

/// Durable lifecycle checkpoints for the immutable active-generation plan.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleCheckpoint {
    completed_through: Option<LifecycleStage>,
    running: Option<LifecycleStage>,
    indeterminate: bool,
}

impl LifecycleCheckpoint {
    /// Creates lifecycle checkpoint state.
    #[must_use]
    pub const fn new(
        completed_through: Option<LifecycleStage>,
        running: Option<LifecycleStage>,
        indeterminate: bool,
    ) -> Self {
        Self {
            completed_through,
            running,
            indeterminate,
        }
    }

    /// Returns the latest completed lifecycle stage.
    #[must_use]
    pub const fn completed_through(&self) -> Option<LifecycleStage> {
        self.completed_through
    }

    /// Returns the lifecycle stage still running in the background.
    #[must_use]
    pub const fn running(&self) -> Option<LifecycleStage> {
        self.running
    }

    /// Returns whether one-time lifecycle execution has no safe retry outcome.
    #[must_use]
    pub const fn indeterminate(&self) -> bool {
        self.indeterminate
    }

    /// Records that no lifecycle runner remains after a down operation.
    pub(crate) fn record_runner_stopped(&mut self, indeterminate: bool) {
        self.running = None;
        self.indeterminate |= indeterminate;
    }
}

/// Forward target protocol used for URL and display behavior.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardProtocol {
    /// Generic TCP traffic.
    Tcp,
    /// HTTP traffic.
    Http,
    /// HTTPS traffic.
    Https,
}

/// One configuration-declared forwarding request.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DeclaredForward {
    requested: ForwardingEndpoint,
    target_host: String,
    target_port: TcpPort,
    require_local_port: bool,
    label: Option<String>,
    protocol: ForwardProtocol,
}

impl DeclaredForward {
    /// Creates a declarative forwarding request.
    #[must_use]
    pub fn new(
        requested: ForwardingEndpoint,
        target_host: String,
        target_port: TcpPort,
        require_local_port: bool,
        label: Option<String>,
        protocol: ForwardProtocol,
    ) -> Self {
        Self {
            requested,
            target_host,
            target_port,
            require_local_port,
            label,
            protocol,
        }
    }
}

/// Requested and last-committed assignments for an active generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveForwarding {
    supervisor_build_id: Option<AgentBuildId>,
    requested: Vec<DeclaredForward>,
    assigned: Vec<ForwardingEndpointAssignment>,
}

impl ActiveForwarding {
    /// Returns the provisioned supervisor build identity, when configured.
    #[must_use]
    pub const fn supervisor_build_id(&self) -> Option<&AgentBuildId> {
        self.supervisor_build_id.as_ref()
    }

    /// Returns configuration-declared forwarding requests.
    #[must_use]
    pub fn requested(&self) -> &[DeclaredForward] {
        &self.requested
    }

    /// Returns the last committed listener assignments.
    #[must_use]
    pub fn assigned(&self) -> &[ForwardingEndpointAssignment] {
        &self.assigned
    }

    /// Creates persisted forwarding reconciliation state.
    #[must_use]
    pub fn new(
        supervisor_build_id: Option<AgentBuildId>,
        requested: Vec<DeclaredForward>,
        assigned: Vec<ForwardingEndpointAssignment>,
    ) -> Self {
        Self {
            supervisor_build_id,
            requested,
            assigned,
        }
    }
}

/// Agent and effective remote execution identity verified during provisioning.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionedState {
    remote_user: String,
    remote_workspace_folder: String,
    container_architecture: ContainerArchitecture,
    agent_path: String,
    agent_build_id: AgentBuildId,
    protocol_version: ProtocolVersion,
    environment_path: String,
}

impl ProvisionedState {
    /// Returns the effective remote user.
    #[must_use]
    pub fn remote_user(&self) -> &str {
        &self.remote_user
    }

    /// Returns the effective remote workspace folder.
    #[must_use]
    pub fn remote_workspace_folder(&self) -> &str {
        &self.remote_workspace_folder
    }

    /// Returns the verified container architecture.
    #[must_use]
    pub const fn container_architecture(&self) -> ContainerArchitecture {
        self.container_architecture
    }

    /// Returns the provisioned agent build identity.
    #[must_use]
    pub const fn agent_build_id(&self) -> &AgentBuildId {
        &self.agent_build_id
    }

    /// Returns the provisioned agent protocol version.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }

    /// Creates verified provision identity without persisting environment values.
    #[must_use]
    pub fn new(
        remote_user: String,
        remote_workspace_folder: String,
        container_architecture: ContainerArchitecture,
        agent_path: String,
        agent_build_id: AgentBuildId,
        protocol_version: ProtocolVersion,
        environment_path: String,
    ) -> Self {
        Self {
            remote_user,
            remote_workspace_folder,
            container_architecture,
            agent_path,
            agent_build_id,
            protocol_version,
            environment_path,
        }
    }
}

/// The last generation that completed provisioning successfully.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActiveGeneration {
    generation: GenerationId,
    scenario: ActiveScenario,
    container_id: ContainerId,
    image_id: String,
    fingerprints: PlanFingerprints,
    feature_digests: BTreeMap<String, String>,
    lifecycle: LifecycleCheckpoint,
    forwarding: ActiveForwarding,
    provisioned: ProvisionedState,
}

impl ActiveGeneration {
    /// Returns the immutable generation identity.
    #[must_use]
    pub const fn generation(&self) -> GenerationId {
        self.generation
    }

    /// Returns the active generation's creation scenario.
    #[must_use]
    pub const fn scenario(&self) -> &ActiveScenario {
        &self.scenario
    }

    /// Returns the independently verified primary container ID.
    #[must_use]
    pub const fn container_id(&self) -> &ContainerId {
        &self.container_id
    }

    /// Returns the active generation's exact image identity.
    #[must_use]
    pub fn image_id(&self) -> &str {
        &self.image_id
    }

    /// Returns the active generation's category fingerprints.
    #[must_use]
    pub const fn fingerprints(&self) -> &PlanFingerprints {
        &self.fingerprints
    }

    /// Returns the active generation's resolved Feature digests.
    #[must_use]
    pub const fn feature_digests(&self) -> &BTreeMap<String, String> {
        &self.feature_digests
    }

    /// Returns durable lifecycle retry state.
    #[must_use]
    pub const fn lifecycle(&self) -> &LifecycleCheckpoint {
        &self.lifecycle
    }

    /// Returns persisted declared-forwarding state.
    #[must_use]
    pub const fn forwarding(&self) -> &ActiveForwarding {
        &self.forwarding
    }

    /// Returns verified provision identity.
    #[must_use]
    pub const fn provisioned(&self) -> &ProvisionedState {
        &self.provisioned
    }

    /// Commits a runtime-only fingerprint after its settings were applied.
    pub(crate) fn commit_runtime_fingerprint(&mut self, runtime: KeyedDigest) {
        self.fingerprints.runtime = runtime;
    }

    /// Records a definite or indeterminate lifecycle-runner stop result.
    pub(crate) fn record_lifecycle_stopped(&mut self, indeterminate: bool) {
        self.lifecycle.record_runner_stopped(indeterminate);
    }

    /// Creates a fully provisioned active-generation record.
    #[expect(
        clippy::too_many_arguments,
        reason = "active generation persistence is intentionally explicit"
    )]
    #[must_use]
    pub fn new(
        generation: GenerationId,
        scenario: ActiveScenario,
        container_id: ContainerId,
        image_id: String,
        fingerprints: PlanFingerprints,
        feature_digests: BTreeMap<String, String>,
        lifecycle: LifecycleCheckpoint,
        forwarding: ActiveForwarding,
        provisioned: ProvisionedState,
    ) -> Self {
        Self {
            generation,
            scenario,
            container_id,
            image_id,
            fingerprints,
            feature_digests,
            lifecycle,
            forwarding,
            provisioned,
        }
    }
}

/// Desired workspace intent and its last completely provisioned generation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkspaceState {
    schema_version: u32,
    installation_id: InstallationId,
    name: WorkspaceName,
    repository_source: SanitizedRepositorySource,
    devcontainer_profile: ProfileId,
    desired_devcontainer_config: DesiredConfigPath,
    desired_fingerprints: PlanFingerprints,
    created_at: StateTimestamp,
    last_up_at: Option<StateTimestamp>,
    operation: OperationState,
    last_error: Option<SanitizedSummary>,
    active: Option<ActiveGeneration>,
}

impl WorkspaceState {
    /// Creates workspace intent before any active generation exists.
    #[must_use]
    pub fn new(
        installation_id: InstallationId,
        name: WorkspaceName,
        repository_source: SanitizedRepositorySource,
        devcontainer_profile: ProfileId,
        desired_devcontainer_config: DesiredConfigPath,
        desired_fingerprints: PlanFingerprints,
        created_at: StateTimestamp,
    ) -> Self {
        Self {
            schema_version: WORKSPACE_STATE_SCHEMA_VERSION,
            installation_id,
            name,
            repository_source,
            devcontainer_profile,
            desired_devcontainer_config,
            desired_fingerprints,
            created_at,
            last_up_at: None,
            operation: OperationState::idle(),
            last_error: None,
            active: None,
        }
    }

    /// Returns the current schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the installation namespace recorded with this workspace.
    #[must_use]
    pub const fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    /// Returns the selected compatibility profile.
    #[must_use]
    pub const fn devcontainer_profile(&self) -> &ProfileId {
        &self.devcontainer_profile
    }

    /// Returns desired plan fingerprints.
    #[must_use]
    pub const fn desired_fingerprints(&self) -> &PlanFingerprints {
        &self.desired_fingerprints
    }

    /// Returns the persisted credential-safe repository description.
    #[must_use]
    pub const fn repository_source(&self) -> &SanitizedRepositorySource {
        &self.repository_source
    }

    /// Returns the desired repository-relative configuration selection.
    #[must_use]
    pub const fn desired_devcontainer_config(&self) -> &DesiredConfigPath {
        &self.desired_devcontainer_config
    }

    /// Returns the latest concise failure summary, if any.
    #[must_use]
    pub const fn last_error(&self) -> Option<&SanitizedSummary> {
        self.last_error.as_ref()
    }

    /// Returns persisted foreground-operation recovery intent.
    #[must_use]
    pub const fn operation(&self) -> &OperationState {
        &self.operation
    }

    /// Returns the validated workspace name.
    #[must_use]
    pub const fn name(&self) -> &WorkspaceName {
        &self.name
    }

    /// Returns the last completely provisioned generation, if any.
    #[must_use]
    pub const fn active(&self) -> Option<&ActiveGeneration> {
        self.active.as_ref()
    }

    /// Returns the active generation mutably to transaction coordinators.
    pub(crate) const fn active_mut(&mut self) -> Option<&mut ActiveGeneration> {
        self.active.as_mut()
    }

    /// Changes desired selection without modifying active-generation bytes.
    pub fn update_desired(
        &mut self,
        profile: ProfileId,
        config: DesiredConfigPath,
        fingerprints: PlanFingerprints,
    ) {
        self.devcontainer_profile = profile;
        self.desired_devcontainer_config = config;
        self.desired_fingerprints = fingerprints;
    }

    /// Records foreground operation recovery intent.
    pub fn set_operation(&mut self, operation: OperationState) {
        self.operation = operation;
    }

    /// Records a sanitized concise failure summary.
    pub fn set_last_error(&mut self, error: Option<SanitizedSummary>) {
        self.last_error = error;
    }

    /// Commits a generation only after complete provisioning success.
    pub fn commit_active(&mut self, active: ActiveGeneration, completed_at: StateTimestamp) {
        self.active = Some(active);
        self.last_up_at = Some(completed_at);
        self.operation = OperationState::idle();
        self.last_error = None;
    }
}

/// Whether loading performed an explicit in-memory schema migration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MigrationStatus {
    /// State was already current.
    Current,
    /// State was upgraded in memory from the reported schema.
    Migrated {
        /// The original schema version.
        from: u32,
    },
}

/// Loaded state together with migration persistence guidance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedWorkspaceState {
    state: WorkspaceState,
    migration: MigrationStatus,
}

impl LoadedWorkspaceState {
    /// Returns decoded current-schema state.
    #[must_use]
    pub const fn state(&self) -> &WorkspaceState {
        &self.state
    }

    /// Consumes the wrapper and returns state for a mutating workflow.
    #[must_use]
    pub fn into_state(self) -> WorkspaceState {
        self.state
    }

    /// Returns whether a mutating caller should persist the migrated form.
    #[must_use]
    pub const fn migration_status(&self) -> MigrationStatus {
        self.migration
    }

    /// Returns true only after an older schema was migrated in memory.
    #[must_use]
    pub const fn needs_migration_persistence(&self) -> bool {
        matches!(self.migration, MigrationStatus::Migrated { .. })
    }
}

/// A workspace-state read, compatibility, or persistence failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum WorkspaceStateError {
    /// Secure managed-path inspection failed.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// Reading state failed.
    #[error("cannot read workspace state {path:?}: {source}")]
    Read {
        /// The state path.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// State is absent rather than silently defaulted.
    #[error("workspace state does not exist: {path:?}")]
    Missing {
        /// The absent path.
        path: PathBuf,
    },
    /// JSON is corrupt or violates state invariants.
    #[error("cannot decode workspace state {path:?}: {source}")]
    Corrupt {
        /// The state path.
        path: PathBuf,
        /// The JSON or validation failure.
        #[source]
        source: serde_json::Error,
    },
    /// State comes from a newer unsupported build.
    #[error(
        "workspace state {path:?} uses newer schema {found}; this build supports schema {supported}"
    )]
    NewerSchema {
        /// The state path.
        path: PathBuf,
        /// Encountered schema.
        found: u32,
        /// Current supported schema.
        supported: u32,
    },
    /// No explicit migration exists for an older schema.
    #[error("workspace state {path:?} uses unsupported older schema {found}")]
    UnsupportedOlderSchema {
        /// The state path.
        path: PathBuf,
        /// Encountered schema.
        found: u32,
    },
    /// An in-memory value does not use the schema this build writes.
    #[error("cannot persist in-memory workspace schema {found}; expected {expected}")]
    InvalidInMemorySchema {
        /// The rejected schema.
        found: u32,
        /// The only writable schema.
        expected: u32,
    },
    /// Encoding current state failed.
    #[error("cannot encode workspace state: {source}")]
    Serialize {
        /// The JSON failure.
        #[source]
        source: serde_json::Error,
    },
    /// Atomic managed storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Reads state without modifying bytes, permissions, or migration status on disk.
///
/// # Errors
///
/// Returns [`WorkspaceStateError`] for absence, unsafe filesystem objects,
/// corrupt state, or unsupported schemas. No error path creates defaults.
pub fn load_workspace_state(path: &Path) -> Result<LoadedWorkspaceState, WorkspaceStateError> {
    match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Missing => {
            return Err(WorkspaceStateError::Missing {
                path: path.to_path_buf(),
            });
        }
        ManagedPathState::Valid => {}
    }
    let bytes = fs::read(path).map_err(|source| WorkspaceStateError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    decode_workspace_state(path, &bytes)
}

/// Decodes workspace state while applying only explicit in-memory migrations.
///
/// # Errors
///
/// Returns [`WorkspaceStateError`] for corrupt or unsupported schema data.
pub fn decode_workspace_state(
    path: &Path,
    bytes: &[u8],
) -> Result<LoadedWorkspaceState, WorkspaceStateError> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct SchemaHeader {
        schema_version: u32,
    }

    let header: SchemaHeader =
        serde_json::from_slice(bytes).map_err(|source| WorkspaceStateError::Corrupt {
            path: path.to_path_buf(),
            source,
        })?;
    if header.schema_version > WORKSPACE_STATE_SCHEMA_VERSION {
        return Err(WorkspaceStateError::NewerSchema {
            path: path.to_path_buf(),
            found: header.schema_version,
            supported: WORKSPACE_STATE_SCHEMA_VERSION,
        });
    }

    match header.schema_version {
        WORKSPACE_STATE_SCHEMA_VERSION => {
            let state = decode_json(path, bytes)?;
            Ok(LoadedWorkspaceState {
                state,
                migration: MigrationStatus::Current,
            })
        }
        LEGACY_SCHEMA_VERSION => {
            let legacy: WorkspaceStateV0 = decode_json(path, bytes)?;
            Ok(LoadedWorkspaceState {
                state: legacy.migrate(),
                migration: MigrationStatus::Migrated {
                    from: LEGACY_SCHEMA_VERSION,
                },
            })
        }
        found => Err(WorkspaceStateError::UnsupportedOlderSchema {
            path: path.to_path_buf(),
            found,
        }),
    }
}

/// Atomically persists only the current workspace schema with mode `0600`.
///
/// # Errors
///
/// Returns [`WorkspaceStateError`] if serialization or durable replacement
/// fails.
pub fn persist_workspace_state(
    path: &Path,
    state: &WorkspaceState,
) -> Result<(), WorkspaceStateError> {
    if state.schema_version != WORKSPACE_STATE_SCHEMA_VERSION {
        return Err(WorkspaceStateError::InvalidInMemorySchema {
            found: state.schema_version,
            expected: WORKSPACE_STATE_SCHEMA_VERSION,
        });
    }
    let mut bytes = serde_json::to_vec_pretty(state)
        .map_err(|source| WorkspaceStateError::Serialize { source })?;
    bytes.push(b'\n');
    atomic_write(path, &bytes, ManagedMode::PrivateFile)?;
    Ok(())
}

fn decode_json<T: for<'de> Deserialize<'de>>(
    path: &Path,
    bytes: &[u8],
) -> Result<T, WorkspaceStateError> {
    serde_json::from_slice(bytes).map_err(|source| WorkspaceStateError::Corrupt {
        path: path.to_path_buf(),
        source,
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct WorkspaceStateV0 {
    schema_version: u32,
    installation_id: InstallationId,
    name: WorkspaceName,
    repository_source: SanitizedRepositorySource,
    devcontainer_config: DesiredConfigPath,
    desired_fingerprints: PlanFingerprints,
    created_at: StateTimestamp,
    operation: OperationState,
    last_error: Option<SanitizedSummary>,
    active: Option<ActiveGeneration>,
}

impl WorkspaceStateV0 {
    fn migrate(self) -> WorkspaceState {
        debug_assert_eq!(self.schema_version, LEGACY_SCHEMA_VERSION);
        let Ok(profile) = ProfileId::parse("cdenv-devcontainer-v1") else {
            unreachable!("the pinned profile ID is a valid static token");
        };
        WorkspaceState {
            schema_version: WORKSPACE_STATE_SCHEMA_VERSION,
            installation_id: self.installation_id,
            name: self.name,
            repository_source: self.repository_source,
            devcontainer_profile: profile,
            desired_devcontainer_config: self.devcontainer_config,
            desired_fingerprints: self.desired_fingerprints,
            created_at: self.created_at,
            last_up_at: None,
            operation: self.operation,
            last_error: self.last_error,
            active: self.active,
        }
    }
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the cross-platform ownership API uses None on non-Unix hosts"
)]
fn current_user_id() -> Option<u32> {
    Some(nix::unistd::geteuid().as_raw())
}

#[cfg(not(unix))]
const fn current_user_id() -> Option<u32> {
    None
}

impl fmt::Display for SanitizedRepositorySource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use sha2::Digest;

    use super::*;
    use crate::{FingerprintKeyState, Installation, PlanFingerprintCategory, RootEnvironment};

    struct NoEnvironment;
    impl RootEnvironment for NoEnvironment {
        fn cdenv_home(&self) -> Option<std::ffi::OsString> {
            None
        }
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    #[test]
    fn no_active_fixture_round_trips_every_top_level_distinction() {
        let bytes = include_bytes!("../tests/fixtures/state-no-active.json");
        let loaded = decode_workspace_state(Path::new("state.json"), bytes)
            .expect("reviewed current fixture should decode");
        let encoded = serde_json::to_value(loaded.state()).expect("state should encode");
        let fixture: serde_json::Value =
            serde_json::from_slice(bytes).expect("fixture should be valid JSON");
        assert_eq!(encoded, fixture);
    }

    #[test]
    fn compose_background_degraded_fixture_round_trips() {
        let bytes = include_bytes!("../tests/fixtures/state-compose-background.json");
        let loaded = decode_workspace_state(Path::new("state.json"), bytes)
            .expect("reviewed Compose fixture should decode");
        let encoded = serde_json::to_value(loaded.state()).expect("state should encode");
        let fixture: serde_json::Value =
            serde_json::from_slice(bytes).expect("fixture should be valid JSON");
        assert_eq!(encoded, fixture);
    }

    #[test]
    fn newer_schema_is_rejected_without_modifying_the_file() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = temporary.path().join("state.json");
        let bytes = br#"{"schemaVersion":999}"#;
        fs::write(&path, bytes).expect("newer state should be written");
        let modified = fs::metadata(&path)
            .expect("metadata should exist")
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);

        let result = load_workspace_state(&path);

        assert!(matches!(
            result,
            Err(WorkspaceStateError::NewerSchema { .. })
        ));
        assert_eq!(
            (
                fs::read(&path).expect("state should remain"),
                fs::metadata(&path)
                    .expect("metadata should remain")
                    .modified()
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            ),
            (bytes.to_vec(), modified)
        );
    }

    #[test]
    fn corrupt_state_is_rejected_without_being_replaced_by_defaults() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = temporary.path().join("state.json");
        let bytes = b"{ definitely not JSON";
        fs::write(&path, bytes).expect("corrupt state should be written");

        let result = load_workspace_state(&path);

        assert!(matches!(result, Err(WorkspaceStateError::Corrupt { .. })));
        assert_eq!(fs::read(path).expect("corrupt bytes should remain"), bytes);
    }

    #[test]
    fn legacy_schema_migrates_only_in_memory_until_explicit_persistence() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = temporary.path().join("state.json");
        let bytes = include_bytes!("../tests/fixtures/state-v0.json");
        fs::write(&path, bytes).expect("legacy state should be written");

        let loaded = load_workspace_state(&path).expect("legacy state should migrate");
        assert!(loaded.needs_migration_persistence());
        assert_eq!(fs::read(&path).expect("legacy bytes should remain"), bytes);

        persist_workspace_state(&path, loaded.state()).expect("mutating caller should persist");
        let persisted = load_workspace_state(&path).expect("persisted state should be current");
        assert_eq!(persisted.migration_status(), MigrationStatus::Current);
    }

    #[test]
    fn secret_markers_and_unkeyed_plan_hashes_are_absent_from_state_and_debug_output() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = crate::CdenvRoot::resolve(Some(&temporary.path().join("root")), &NoEnvironment)
            .expect("test root should validate");
        let installation = Installation::open_or_create(&root).expect("installation should exist");
        let key = match installation.fingerprint_key() {
            FingerprintKeyState::Available(key) => key,
            FingerprintKeyState::Unknown(reason) => panic!("new key is unexpectedly {reason:?}"),
        };
        let secret = "state-secret-marker";
        let unkeyed = hex::encode(sha2::Sha256::digest(secret));
        let fingerprints = PlanFingerprints::new(
            key.digest_plan(PlanFingerprintCategory::Build, [secret.as_bytes()]),
            key.digest_plan(PlanFingerprintCategory::Create, [b"create".as_slice()]),
            key.digest_plan(PlanFingerprintCategory::Runtime, [b"runtime".as_slice()]),
        );
        let lifecycle = key.digest_plan(PlanFingerprintCategory::Lifecycle, [secret.as_bytes()]);
        let state = WorkspaceState::new(
            installation.record().installation_id().clone(),
            WorkspaceName::parse("project").expect("workspace name should be valid"),
            SanitizedRepositorySource::sanitize(&format!(
                "https://user:{secret}@example.test/repo.git?token={secret}"
            )),
            ProfileId::parse("cdenv-devcontainer-v1").expect("profile should be valid"),
            DesiredConfigPath::parse(".devcontainer/devcontainer.json")
                .expect("config should be valid"),
            fingerprints,
            StateTimestamp::parse("2025-01-02T03:04:05Z").expect("timestamp should be valid"),
        );
        let output = format!(
            "{}\n{state:?}\n{lifecycle}",
            serde_json::to_string(&state).expect("state should encode")
        );

        assert!(!output.contains(secret));
        assert!(!output.contains(&unkeyed));
    }

    #[test]
    fn desired_selection_changes_without_changing_active_generation_bytes() {
        let bytes = include_bytes!("../tests/fixtures/state-compose-background.json");
        let mut state = decode_workspace_state(Path::new("state.json"), bytes)
            .expect("fixture should decode")
            .into_state();
        let before = serde_json::to_vec(&state.active()).expect("active should encode");
        let replacement: PlanFingerprints = serde_json::from_value(serde_json::json!({
            "build": format!("keyed:{}", "a".repeat(64)),
            "create": format!("keyed:{}", "b".repeat(64)),
            "runtime": format!("keyed:{}", "c".repeat(64))
        }))
        .expect("replacement fingerprints should decode");

        state.update_desired(
            ProfileId::parse("cdenv-devcontainer-v1").expect("profile should be valid"),
            DesiredConfigPath::parse(".devcontainer/alternate.json")
                .expect("config should be valid"),
            replacement,
        );

        assert_eq!(
            serde_json::to_vec(&state.active()).expect("active should still encode"),
            before
        );
    }

    #[test]
    fn keyed_hashing_is_category_and_boundary_separated() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = crate::CdenvRoot::resolve(Some(&temporary.path().join("root")), &NoEnvironment)
            .expect("test root should validate");
        let installation = Installation::open_or_create(&root).expect("installation should exist");
        let key = match installation.fingerprint_key() {
            FingerprintKeyState::Available(key) => key,
            FingerprintKeyState::Unknown(reason) => panic!("new key is unexpectedly {reason:?}"),
        };
        let build = key.digest_plan(PlanFingerprintCategory::Build, [b"ab".as_slice(), b"c"]);
        let create = key.digest_plan(PlanFingerprintCategory::Create, [b"ab".as_slice(), b"c"]);
        let lifecycle =
            key.digest_plan(PlanFingerprintCategory::Lifecycle, [b"ab".as_slice(), b"c"]);
        let regrouped = key.digest_plan(PlanFingerprintCategory::Build, [b"a".as_slice(), b"bc"]);
        assert!(build != create && build != lifecycle && build != regrouped);
    }
}
