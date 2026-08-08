//! Serializable, orthogonal workspace status dimensions.
//!
//! A status is assembled from persisted intent and live observations. Persisted
//! state contributes the selected operation, desired and active configuration,
//! lifecycle intent, and requested/assigned forwards. Docker inspection is the
//! authority for environment state and provision identity. Lock inspection,
//! lifecycle-runner checkpoints, and forwarding-supervisor inspection supply
//! the remaining live facts. No single persisted status value is treated as
//! live truth.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU16;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// The live state of a workspace's managed containers.
///
/// This dimension must be supplied by Docker inspection. Persisted container
/// identity alone cannot distinguish these states.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentStatus {
    /// Every required managed container is running.
    Running,
    /// The recorded managed environment exists but is stopped.
    Stopped,
    /// Only part of the required managed environment is running.
    PartiallyRunning,
    /// No acceptable container exists for the recorded environment.
    Missing,
    /// More than one container is an equally valid current-generation match.
    Ambiguous,
    /// The Docker daemon could not be inspected.
    DockerUnavailable,
}

/// The foreground workspace operation recorded by status correlation.
///
/// Persisted operation intent supplies the kind, while inspection of the
/// workspace lock determines whether that operation is active. If the lock is
/// available despite non-idle persisted intent, retain the operation kind and
/// report [`LocalHealthStatus::Interrupted`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForegroundOperation {
    /// No foreground mutation is recorded.
    Idle,
    /// Initial workspace creation is in progress or was interrupted.
    Creating,
    /// Environment startup is in progress or was interrupted.
    Starting,
    /// Environment replacement is in progress or was interrupted.
    Rebuilding,
    /// Environment shutdown is in progress or was interrupted.
    Stopping,
    /// Feature lockfile generation is in progress or was interrupted.
    Locking,
}

/// Whether one immutable configuration category differs from the active plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationDrift {
    /// Desired and active category fingerprints match.
    Current,
    /// Desired and active category fingerprints differ.
    Drifted,
}

/// The reconciliation state of runtime configuration.
///
/// Runtime drift is distinct from build/create drift because it can be applied
/// to an existing generation without rebuilding it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeConfigurationDrift {
    /// Desired and active runtime configuration match.
    Current,
    /// Runtime drift was applied to the active generation.
    Applied,
    /// Desired runtime drift could not be applied and remains pending.
    Pending,
}

/// Whether the desired configuration can be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationValidity {
    /// The desired configuration conforms to the pinned profile.
    Valid,
    /// The desired configuration is syntactically or semantically invalid.
    Invalid,
    /// The desired configuration requires a profile cdenv does not support.
    ProfileUnsupported,
}

/// Desired-versus-active configuration status.
///
/// Desired selection and fingerprints come from persisted intent and current
/// checkout parsing. Active fingerprints come from the last successfully
/// provisioned generation. Validity does not erase known category drift, so an
/// invalid desired configuration can be reported without hiding active state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigurationStatus {
    validity: ConfigurationValidity,
    build: ConfigurationDrift,
    create: ConfigurationDrift,
    runtime: RuntimeConfigurationDrift,
}

impl ConfigurationStatus {
    /// Creates a configuration status without collapsing its category states.
    #[must_use]
    pub const fn new(
        validity: ConfigurationValidity,
        build: ConfigurationDrift,
        create: ConfigurationDrift,
        runtime: RuntimeConfigurationDrift,
    ) -> Self {
        Self {
            validity,
            build,
            create,
            runtime,
        }
    }

    /// Creates a valid status with no desired-versus-active drift.
    #[must_use]
    pub const fn current() -> Self {
        Self::new(
            ConfigurationValidity::Valid,
            ConfigurationDrift::Current,
            ConfigurationDrift::Current,
            RuntimeConfigurationDrift::Current,
        )
    }

    /// Returns desired configuration validity.
    #[must_use]
    pub const fn validity(self) -> ConfigurationValidity {
        self.validity
    }

    /// Returns build-plan drift.
    #[must_use]
    pub const fn build(self) -> ConfigurationDrift {
        self.build
    }

    /// Returns create-plan drift.
    #[must_use]
    pub const fn create(self) -> ConfigurationDrift {
        self.create
    }

    /// Returns runtime-plan reconciliation state.
    #[must_use]
    pub const fn runtime(self) -> RuntimeConfigurationDrift {
        self.runtime
    }
}

/// The state of generation-scoped lifecycle work.
///
/// Persisted lifecycle intent identifies the expected work. Live runner
/// identity and checkpoints determine whether background work is still running,
/// failed, or became indeterminate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    /// Every required generation-scoped lifecycle stage completed.
    Complete,
    /// A later lifecycle stage is healthy and running in the background.
    RunningInBackground,
    /// A lifecycle stage completed with failure.
    Failed,
    /// A one-time lifecycle stage may have run but has no definite outcome.
    Indeterminate,
}

/// The state of configuration-declared forwarding.
///
/// Persisted intent and assignments establish what should be forwarded. The
/// supervisor, listener, and target must be inspected to establish live state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ForwardingStatus {
    /// Every configured forwarding listener and target is healthy.
    Active,
    /// At least one configured forward is unavailable for another reason.
    Degraded,
    /// Forwarding is configured but its scoped supervisor is absent.
    MissingSupervisor,
    /// Listeners are present but at least one container target is unavailable.
    TargetUnavailable,
    /// The desired runtime plan contains no declared forwards.
    NotConfigured,
}

/// Health derived from local state, operation recovery, and provision identity.
///
/// Corrupt and interrupted conditions come from local state and lock
/// inspection. Provision drift requires correlating persisted identity with
/// live container labels and provisioned agent identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalHealthStatus {
    /// Local state and provision identity are internally consistent.
    Valid,
    /// Persisted foreground intent has no corresponding held operation lock.
    Interrupted,
    /// The most recent foreground operation finished with a recorded failure.
    LastOperationFailed,
    /// Managed local state cannot be decoded or validated.
    Corrupt,
    /// Live resources do not match the recorded provision identity.
    ProvisionDrift,
}

/// All independent status dimensions for one workspace.
///
/// The fields deliberately remain orthogonal. For example, an environment can
/// be running while build configuration is drifted, lifecycle work continues,
/// and forwarding is degraded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusDimensions {
    environment: EnvironmentStatus,
    operation: ForegroundOperation,
    configuration: ConfigurationStatus,
    lifecycle: LifecycleStatus,
    forwarding: ForwardingStatus,
    local_health: LocalHealthStatus,
}

impl StatusDimensions {
    /// Creates a set of independent workspace status dimensions.
    #[must_use]
    pub const fn new(
        environment: EnvironmentStatus,
        operation: ForegroundOperation,
        configuration: ConfigurationStatus,
        lifecycle: LifecycleStatus,
        forwarding: ForwardingStatus,
        local_health: LocalHealthStatus,
    ) -> Self {
        Self {
            environment,
            operation,
            configuration,
            lifecycle,
            forwarding,
            local_health,
        }
    }

    /// Returns live environment state.
    #[must_use]
    pub const fn environment(self) -> EnvironmentStatus {
        self.environment
    }

    /// Returns the correlated foreground operation.
    #[must_use]
    pub const fn operation(self) -> ForegroundOperation {
        self.operation
    }

    /// Returns desired-versus-active configuration state.
    #[must_use]
    pub const fn configuration(self) -> ConfigurationStatus {
        self.configuration
    }

    /// Returns lifecycle state.
    #[must_use]
    pub const fn lifecycle(self) -> LifecycleStatus {
        self.lifecycle
    }

    /// Returns declared-forwarding state.
    #[must_use]
    pub const fn forwarding(self) -> ForwardingStatus {
        self.forwarding
    }

    /// Returns local and provision health.
    #[must_use]
    pub const fn local_health(self) -> LocalHealthStatus {
        self.local_health
    }
}

/// A validated nonzero TCP port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TcpPort(NonZeroU16);

/// An error returned when validating a [`TcpPort`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TcpPortError {
    /// Port zero does not identify a connectable endpoint.
    #[error("a TCP port must be greater than zero")]
    Zero,
    /// The value exceeds the largest TCP port.
    #[error("a TCP port cannot exceed 65535; found {value}")]
    TooLarge {
        /// The rejected numeric value.
        value: u32,
    },
}

impl TcpPort {
    /// Creates a TCP port from a 16-bit value.
    ///
    /// # Errors
    ///
    /// Returns [`TcpPortError::Zero`] when `value` is zero.
    pub const fn new(value: u16) -> Result<Self, TcpPortError> {
        match NonZeroU16::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(TcpPortError::Zero),
        }
    }

    /// Validates a wider integer as a TCP port.
    ///
    /// # Errors
    ///
    /// Returns [`TcpPortError::Zero`] for zero and
    /// [`TcpPortError::TooLarge`] for values above 65535.
    pub fn from_u32(value: u32) -> Result<Self, TcpPortError> {
        let Ok(port) = u16::try_from(value) else {
            return Err(TcpPortError::TooLarge { value });
        };

        Self::new(port)
    }

    /// Returns the validated port number.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl fmt::Display for TcpPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

impl TryFrom<u16> for TcpPort {
    type Error = TcpPortError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl TryFrom<u32> for TcpPort {
    type Error = TcpPortError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::from_u32(value)
    }
}

impl From<TcpPort> for u16 {
    fn from(value: TcpPort) -> Self {
        value.get()
    }
}

impl Serialize for TcpPort {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u16(self.get())
    }
}

impl<'de> Deserialize<'de> for TcpPort {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::from_u32(value).map_err(serde::de::Error::custom)
    }
}

/// A loopback listener endpoint for a configuration-declared forward.
///
/// Declared forwarding never exposes a non-loopback listener implicitly. The
/// private representation and validated deserialization preserve that rule.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingEndpoint {
    address: IpAddr,
    port: TcpPort,
}

/// An error returned when validating a [`ForwardingEndpoint`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForwardingEndpointError {
    /// The port is zero or outside the TCP port range.
    #[error("invalid forwarding endpoint port: {0}")]
    InvalidPort(#[from] TcpPortError),
    /// Configuration-declared listeners must remain on a loopback address.
    #[error("a forwarding endpoint must use a loopback address, not `{address}`")]
    NonLoopbackAddress {
        /// The rejected listener address.
        address: IpAddr,
    },
}

impl ForwardingEndpoint {
    /// Creates a validated loopback forwarding endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardingEndpointError::NonLoopbackAddress`] when `address`
    /// is not a loopback address.
    pub fn new(address: IpAddr, port: TcpPort) -> Result<Self, ForwardingEndpointError> {
        if !address.is_loopback() {
            return Err(ForwardingEndpointError::NonLoopbackAddress { address });
        }

        Ok(Self { address, port })
    }

    /// Validates an address and an unbounded numeric port together.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardingEndpointError::InvalidPort`] when `port` is zero or
    /// exceeds 65535, and [`ForwardingEndpointError::NonLoopbackAddress`] when
    /// `address` is not loopback.
    pub fn try_from_parts(address: IpAddr, port: u32) -> Result<Self, ForwardingEndpointError> {
        Self::new(address, TcpPort::from_u32(port)?)
    }

    /// Creates an IPv4 loopback endpoint for a validated port.
    #[must_use]
    pub const fn loopback(port: TcpPort) -> Self {
        Self {
            address: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        }
    }

    /// Returns the loopback listener address.
    #[must_use]
    pub const fn address(self) -> IpAddr {
        self.address
    }

    /// Returns the listener port.
    #[must_use]
    pub const fn port(self) -> TcpPort {
        self.port
    }
}

impl<'de> Deserialize<'de> for ForwardingEndpoint {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase", deny_unknown_fields)]
        struct Representation {
            address: IpAddr,
            port: TcpPort,
        }

        let representation = Representation::deserialize(deserializer)?;
        Self::new(representation.address, representation.port).map_err(serde::de::Error::custom)
    }
}

/// Requested and assigned local endpoints for one declared forward.
///
/// `assigned` is absent until a listener has been allocated or when allocation
/// failed. It remains present even when it equals `requested`, so JSON consumers
/// never have to infer assignment from omission.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingEndpointAssignment {
    requested: ForwardingEndpoint,
    assigned: Option<ForwardingEndpoint>,
}

impl ForwardingEndpointAssignment {
    /// Creates a requested endpoint with its optional live assignment.
    #[must_use]
    pub const fn new(requested: ForwardingEndpoint, assigned: Option<ForwardingEndpoint>) -> Self {
        Self {
            requested,
            assigned,
        }
    }

    /// Creates an endpoint request that has not been assigned a listener.
    #[must_use]
    pub const fn pending(requested: ForwardingEndpoint) -> Self {
        Self::new(requested, None)
    }

    /// Returns the endpoint requested by persisted runtime intent.
    #[must_use]
    pub const fn requested(&self) -> ForwardingEndpoint {
        self.requested
    }

    /// Returns the assigned live listener endpoint, when one exists.
    #[must_use]
    pub const fn assigned(&self) -> Option<ForwardingEndpoint> {
        self.assigned
    }
}

/// Whether a structured status fact is a warning or an error.
///
/// Severity is command-contextual. For example, Docker unavailability can be a
/// per-workspace warning for `list` and an error for a requested `status` query.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFactSeverity {
    /// The condition does not by itself make the surrounding request fail.
    Warning,
    /// The condition makes the surrounding requested state fail.
    Error,
}

/// A presentation-neutral machine code for a concise status fact.
///
/// Renderers map these codes to human text. Keeping that text out of core lets
/// CLI output evolve without changing persisted or machine-oriented values.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusFactCode {
    /// Docker could not be queried.
    DockerUnavailable,
    /// Multiple current-generation containers matched.
    AmbiguousContainers,
    /// A recorded foreground operation no longer holds the lock.
    InterruptedOperation,
    /// Desired build configuration differs from the active generation.
    BuildDrift,
    /// Desired create configuration differs from the active generation.
    CreateDrift,
    /// Runtime drift was successfully applied.
    RuntimeDriftApplied,
    /// Runtime drift remains unapplied.
    RuntimeDriftPending,
    /// Desired configuration is invalid.
    InvalidConfiguration,
    /// Desired configuration requires an unsupported profile.
    ProfileUnsupported,
    /// A lifecycle command failed.
    LifecycleFailed,
    /// A one-time lifecycle command has no definite outcome.
    LifecycleIndeterminate,
    /// Declared forwarding is degraded.
    ForwardingDegraded,
    /// The declared-forwarding supervisor is missing.
    MissingForwardingSupervisor,
    /// A declared-forwarding target is unavailable.
    ForwardingTargetUnavailable,
    /// The most recent foreground operation failed.
    LastOperationFailed,
    /// Local managed state is corrupt.
    CorruptLocalState,
    /// Live provision identity differs from persisted identity.
    ProvisionDrift,
    /// A requested listener received a different available endpoint.
    AlternateForwardingEndpoint,
}

/// A concise structured warning or error attached to workspace status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StatusFact {
    severity: StatusFactSeverity,
    code: StatusFactCode,
}

impl StatusFact {
    /// Creates a warning fact.
    #[must_use]
    pub const fn warning(code: StatusFactCode) -> Self {
        Self {
            severity: StatusFactSeverity::Warning,
            code,
        }
    }

    /// Creates an error fact.
    #[must_use]
    pub const fn error(code: StatusFactCode) -> Self {
        Self {
            severity: StatusFactSeverity::Error,
            code,
        }
    }

    /// Returns whether the fact is a warning or an error.
    #[must_use]
    pub const fn severity(self) -> StatusFactSeverity {
        self.severity
    }

    /// Returns the fact's presentation-neutral machine code.
    #[must_use]
    pub const fn code(self) -> StatusFactCode {
        self.code
    }
}

/// Complete platform-neutral status data for one workspace.
///
/// Every dimension and both sides of every forwarding assignment are serialized
/// independently. The `facts` list adds command-contextual warning/error codes;
/// it does not replace any dimension.
///
/// # Examples
///
/// ```
/// use cdenv_core::{
///     ConfigurationDrift, ConfigurationStatus, ConfigurationValidity,
///     EnvironmentStatus, ForegroundOperation, ForwardingStatus,
///     LifecycleStatus, LocalHealthStatus, RuntimeConfigurationDrift,
///     StatusDimensions, WorkspaceStatus,
/// };
///
/// let dimensions = StatusDimensions::new(
///     EnvironmentStatus::Running,
///     ForegroundOperation::Idle,
///     ConfigurationStatus::new(
///         ConfigurationValidity::Valid,
///         ConfigurationDrift::Drifted,
///         ConfigurationDrift::Current,
///         RuntimeConfigurationDrift::Current,
///     ),
///     LifecycleStatus::RunningInBackground,
///     ForwardingStatus::Degraded,
///     LocalHealthStatus::Valid,
/// );
/// let status = WorkspaceStatus::new(dimensions, Vec::new(), Vec::new());
///
/// assert_eq!(status.dimensions().environment(), EnvironmentStatus::Running);
/// ```
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatus {
    #[serde(flatten)]
    dimensions: StatusDimensions,
    forwarding_endpoints: Vec<ForwardingEndpointAssignment>,
    facts: Vec<StatusFact>,
}

impl WorkspaceStatus {
    /// Creates complete workspace status data.
    #[must_use]
    pub fn new(
        dimensions: StatusDimensions,
        forwarding_endpoints: Vec<ForwardingEndpointAssignment>,
        facts: Vec<StatusFact>,
    ) -> Self {
        Self {
            dimensions,
            forwarding_endpoints,
            facts,
        }
    }

    /// Returns every independent status dimension.
    #[must_use]
    pub const fn dimensions(&self) -> StatusDimensions {
        self.dimensions
    }

    /// Returns requested and assigned declared-forwarding endpoints.
    #[must_use]
    pub fn forwarding_endpoints(&self) -> &[ForwardingEndpointAssignment] {
        &self.forwarding_endpoints
    }

    /// Returns structured warning and error facts.
    #[must_use]
    pub fn facts(&self) -> &[StatusFact] {
        &self.facts
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use serde_json::json;

    use super::{
        ConfigurationDrift, ConfigurationStatus, ConfigurationValidity, EnvironmentStatus,
        ForegroundOperation, ForwardingEndpoint, ForwardingEndpointAssignment,
        ForwardingEndpointError, ForwardingStatus, LifecycleStatus, LocalHealthStatus,
        RuntimeConfigurationDrift, StatusDimensions, StatusFact, StatusFactCode,
        StatusFactSeverity, TcpPort, TcpPortError, WorkspaceStatus,
    };

    fn status_with_dimensions(dimensions: StatusDimensions, fact: StatusFact) -> WorkspaceStatus {
        WorkspaceStatus::new(dimensions, Vec::new(), vec![fact])
    }

    #[test]
    fn combined_status_serialization_retains_every_independent_dimension_and_endpoint() {
        let requested = ForwardingEndpoint::loopback(
            TcpPort::new(3000).expect("the requested test port should be valid"),
        );
        let assigned = ForwardingEndpoint::loopback(
            TcpPort::new(49_152).expect("the assigned test port should be valid"),
        );
        let dimensions = StatusDimensions::new(
            EnvironmentStatus::Running,
            ForegroundOperation::Idle,
            ConfigurationStatus::new(
                ConfigurationValidity::Valid,
                ConfigurationDrift::Drifted,
                ConfigurationDrift::Current,
                RuntimeConfigurationDrift::Current,
            ),
            LifecycleStatus::RunningInBackground,
            ForwardingStatus::Degraded,
            LocalHealthStatus::Valid,
        );
        let status = WorkspaceStatus::new(
            dimensions,
            vec![ForwardingEndpointAssignment::new(requested, Some(assigned))],
            vec![
                StatusFact::warning(StatusFactCode::BuildDrift),
                StatusFact::error(StatusFactCode::ForwardingDegraded),
            ],
        );

        let serialized = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            serialized,
            json!({
                "environment": "running",
                "operation": "idle",
                "configuration": {
                    "validity": "valid",
                    "build": "drifted",
                    "create": "current",
                    "runtime": "current"
                },
                "lifecycle": "running_in_background",
                "forwarding": "degraded",
                "localHealth": "valid",
                "forwardingEndpoints": [{
                    "requested": {"address": "127.0.0.1", "port": 3000},
                    "assigned": {"address": "127.0.0.1", "port": 49152}
                }],
                "facts": [
                    {"severity": "warning", "code": "build_drift"},
                    {"severity": "error", "code": "forwarding_degraded"}
                ]
            })
        );
    }

    #[test]
    fn combined_status_round_trip_preserves_all_values() {
        let requested = ForwardingEndpoint::loopback(
            TcpPort::new(8080).expect("the requested test port should be valid"),
        );
        let status = WorkspaceStatus::new(
            StatusDimensions::new(
                EnvironmentStatus::Running,
                ForegroundOperation::Starting,
                ConfigurationStatus::new(
                    ConfigurationValidity::Valid,
                    ConfigurationDrift::Current,
                    ConfigurationDrift::Drifted,
                    RuntimeConfigurationDrift::Applied,
                ),
                LifecycleStatus::Complete,
                ForwardingStatus::Active,
                LocalHealthStatus::Valid,
            ),
            vec![ForwardingEndpointAssignment::new(
                requested,
                Some(requested),
            )],
            vec![StatusFact::warning(StatusFactCode::RuntimeDriftApplied)],
        );
        let json = serde_json::to_string(&status).expect("status serialization should succeed");
        let decoded: WorkspaceStatus =
            serde_json::from_str(&json).expect("status deserialization should succeed");

        assert_eq!(decoded, status);
    }

    #[test]
    fn docker_unavailable_serializes_as_live_environment_state_and_warning_fact() {
        let status = status_with_dimensions(
            StatusDimensions::new(
                EnvironmentStatus::DockerUnavailable,
                ForegroundOperation::Idle,
                ConfigurationStatus::current(),
                LifecycleStatus::Complete,
                ForwardingStatus::NotConfigured,
                LocalHealthStatus::Valid,
            ),
            StatusFact::warning(StatusFactCode::DockerUnavailable),
        );
        let value = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            (&value["environment"], &value["facts"][0]),
            (
                &json!("docker_unavailable"),
                &json!({"severity": "warning", "code": "docker_unavailable"})
            )
        );
    }

    #[test]
    fn ambiguous_containers_serialize_without_selecting_one() {
        let status = status_with_dimensions(
            StatusDimensions::new(
                EnvironmentStatus::Ambiguous,
                ForegroundOperation::Idle,
                ConfigurationStatus::current(),
                LifecycleStatus::Complete,
                ForwardingStatus::NotConfigured,
                LocalHealthStatus::ProvisionDrift,
            ),
            StatusFact::error(StatusFactCode::AmbiguousContainers),
        );
        let value = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            (
                &value["environment"],
                &value["localHealth"],
                &value["facts"][0]["code"]
            ),
            (
                &json!("ambiguous"),
                &json!("provision_drift"),
                &json!("ambiguous_containers")
            )
        );
    }

    #[test]
    fn interrupted_operation_retains_its_kind_and_local_health() {
        let status = status_with_dimensions(
            StatusDimensions::new(
                EnvironmentStatus::Stopped,
                ForegroundOperation::Rebuilding,
                ConfigurationStatus::current(),
                LifecycleStatus::Indeterminate,
                ForwardingStatus::NotConfigured,
                LocalHealthStatus::Interrupted,
            ),
            StatusFact::error(StatusFactCode::InterruptedOperation),
        );
        let value = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            (
                &value["operation"],
                &value["localHealth"],
                &value["facts"][0]["code"]
            ),
            (
                &json!("rebuilding"),
                &json!("interrupted"),
                &json!("interrupted_operation")
            )
        );
    }

    #[test]
    fn lifecycle_failure_serializes_independently_of_a_running_environment() {
        let status = status_with_dimensions(
            StatusDimensions::new(
                EnvironmentStatus::Running,
                ForegroundOperation::Idle,
                ConfigurationStatus::current(),
                LifecycleStatus::Failed,
                ForwardingStatus::Active,
                LocalHealthStatus::Valid,
            ),
            StatusFact::error(StatusFactCode::LifecycleFailed),
        );
        let value = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            (
                &value["environment"],
                &value["lifecycle"],
                &value["facts"][0]["code"]
            ),
            (
                &json!("running"),
                &json!("failed"),
                &json!("lifecycle_failed")
            )
        );
    }

    #[test]
    fn missing_supervisor_serializes_with_requested_but_unassigned_endpoint() {
        let requested = ForwardingEndpoint::loopback(
            TcpPort::new(5432).expect("the requested test port should be valid"),
        );
        let status = WorkspaceStatus::new(
            StatusDimensions::new(
                EnvironmentStatus::Running,
                ForegroundOperation::Idle,
                ConfigurationStatus::current(),
                LifecycleStatus::Complete,
                ForwardingStatus::MissingSupervisor,
                LocalHealthStatus::Valid,
            ),
            vec![ForwardingEndpointAssignment::pending(requested)],
            vec![StatusFact::error(
                StatusFactCode::MissingForwardingSupervisor,
            )],
        );
        let value = serde_json::to_value(status).expect("status serialization should succeed");

        assert_eq!(
            (
                &value["forwarding"],
                &value["forwardingEndpoints"][0],
                &value["facts"][0]["code"]
            ),
            (
                &json!("missing_supervisor"),
                &json!({
                    "requested": {"address": "127.0.0.1", "port": 5432},
                    "assigned": null
                }),
                &json!("missing_forwarding_supervisor")
            )
        );
    }

    #[test]
    fn tcp_port_zero_returns_a_typed_error_with_a_stable_message() {
        let error = TcpPort::new(0).expect_err("port zero must fail");
        let message = error.to_string();

        assert_eq!(
            (error, message),
            (
                TcpPortError::Zero,
                "a TCP port must be greater than zero".to_owned()
            )
        );
    }

    #[test]
    fn oversized_tcp_port_returns_a_typed_error_with_a_stable_message() {
        let error = TcpPort::from_u32(65_536).expect_err("an oversized port must fail");
        let message = error.to_string();

        assert_eq!(
            (error, message),
            (
                TcpPortError::TooLarge { value: 65_536 },
                "a TCP port cannot exceed 65535; found 65536".to_owned()
            )
        );
    }

    #[test]
    fn non_loopback_forwarding_endpoint_returns_a_focused_state_error() {
        let address = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
        let port = TcpPort::new(3000).expect("the test port should be valid");
        let error = ForwardingEndpoint::new(address, port)
            .expect_err("a non-loopback declared endpoint must fail");
        let message = error.to_string();

        assert_eq!(
            (error, message),
            (
                ForwardingEndpointError::NonLoopbackAddress { address },
                "a forwarding endpoint must use a loopback address, not `192.0.2.10`".to_owned()
            )
        );
    }

    #[test]
    fn forwarding_endpoint_wraps_port_validation_without_erasing_the_variant() {
        let error = ForwardingEndpoint::try_from_parts(IpAddr::V4(Ipv4Addr::LOCALHOST), 65_536)
            .expect_err("an invalid endpoint port must fail");
        let message = error.to_string();

        assert_eq!(
            (error, message),
            (
                ForwardingEndpointError::InvalidPort(TcpPortError::TooLarge { value: 65_536 }),
                "invalid forwarding endpoint port: a TCP port cannot exceed 65535; found 65536"
                    .to_owned()
            )
        );
    }

    #[test]
    fn forwarding_endpoint_deserialization_rejects_non_loopback_state() {
        let result = serde_json::from_value::<ForwardingEndpoint>(json!({
            "address": "203.0.113.8",
            "port": 3000
        }));

        assert!(result.is_err());
    }

    #[test]
    fn forwarding_endpoint_deserialization_rejects_port_zero() {
        let result = serde_json::from_value::<ForwardingEndpoint>(json!({
            "address": "127.0.0.1",
            "port": 0
        }));

        assert!(result.is_err());
    }

    #[test]
    fn status_values_and_errors_are_send_sync_and_static() {
        fn assert_send_sync_static<T: Send + Sync + 'static>() {}

        assert_send_sync_static::<WorkspaceStatus>();
        assert_send_sync_static::<StatusFact>();
        assert_send_sync_static::<ForwardingEndpointError>();
        assert_send_sync_static::<TcpPortError>();
    }

    #[test]
    fn status_fact_severity_remains_independent_from_its_code() {
        let warning = StatusFact::warning(StatusFactCode::DockerUnavailable);
        let error = StatusFact::error(StatusFactCode::DockerUnavailable);

        assert_eq!(
            (
                warning.severity(),
                warning.code(),
                error.severity(),
                error.code()
            ),
            (
                StatusFactSeverity::Warning,
                StatusFactCode::DockerUnavailable,
                StatusFactSeverity::Error,
                StatusFactCode::DockerUnavailable
            )
        );
    }
}
