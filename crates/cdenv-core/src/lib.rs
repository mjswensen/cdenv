//! Platform-neutral domain types and serialization for cdenv.
//!
//! This crate deliberately has no host orchestration or asynchronous runtime
//! dependencies. Its private newtype representations ensure strings and
//! integers are validated before they cross persistence, Docker, or agent
//! protocol boundaries.

pub mod credential_protocol;
pub mod credentials;
pub mod executable;
mod identity;
mod status;
mod workspace;

pub use identity::{
    AgentBuildId, AgentBuildIdError, ContainerArchitecture, ContainerId, ContainerIdError,
    GenerationId, GenerationIdError, IdentityTokenError, InstallationId, InstallationIdError,
    ProfileId, ProfileIdError, ProtocolVersion, ProtocolVersionError,
    UnsupportedContainerArchitecture,
};
pub use status::{
    ConfigurationDrift, ConfigurationStatus, ConfigurationValidity, EnvironmentStatus,
    ForegroundOperation, ForwardingEndpoint, ForwardingEndpointAssignment, ForwardingEndpointError,
    ForwardingStatus, LifecycleStatus, LocalHealthStatus, RuntimeConfigurationDrift,
    StatusDimensions, StatusFact, StatusFactCode, StatusFactSeverity, TcpPort, TcpPortError,
    WorkspaceStatus,
};
pub use workspace::{
    MAX_WORKSPACE_NAME_LENGTH, WORKSPACE_HOST_SUFFIX, WorkspaceHost, WorkspaceHostError,
    WorkspaceName, WorkspaceNameError, WorkspaceNameSelectionError, derive_workspace_name,
    derive_workspace_name_from_path,
};
