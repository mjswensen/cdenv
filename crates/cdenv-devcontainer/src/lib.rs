//! Pure front end and planning types for `cdenv-devcontainer-v1`.
//!
//! Configuration discovery and JSONC parsing operate only on injected paths
//! and bytes. Docker, Compose, filesystem, network, credential-helper, and
//! subprocess adapters belong to the host application rather than this crate.

mod discovery;
mod jsonc;
mod profile;
mod schema;

pub use discovery::{
    ConfigInventory, ConfigPath, ConfigPathError, DiscoveryError, discover_config,
};
pub use jsonc::{
    BoundKind, Diagnostic, JsoncError, MAX_ARRAY_ITEMS, MAX_CONFIG_BYTES,
    MAX_LIFECYCLE_GROUP_ITEMS, MAX_NESTING_DEPTH, MAX_OBJECT_ITEMS, MAX_STRING_BYTES, ParseLimits,
    RawDocument, SourceSpan, parse_jsonc,
};
pub use profile::{
    AppPort, AutoForwardAction, Capability, CommandValue, ComposeScenario, DockerfileScenario,
    FeatureOptionValue, FeatureSource, ForwardPort, GpuRequirement, HostRequirements,
    ImageScenario, LifecycleCommands, MountKind, NonComposeOptions, PortAttributes, PortProtocol,
    ProfileError, ProfileErrorKind, RawBuild, RawCommand, RawCommon, RawFeature, RawMount,
    RawProfile, RawScenario, SecretMetadata, ShutdownAction, UserEnvProbe, WaitFor,
    capability_report, capability_report_json, validate_profile,
};
pub use schema::{BASE_SCHEMA_SHA256, PROFILE_REVISION, SPECIFICATION_COMMIT, base_schema};
