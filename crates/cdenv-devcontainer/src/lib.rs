//! Pure front end and planning types for `cdenv-devcontainer-v1`.
//!
//! Configuration discovery and JSONC parsing operate only on injected paths
//! and bytes. Docker, Compose, filesystem, network, credential-helper, and
//! subprocess adapters belong to the host application rather than this crate.

mod discovery;
mod docker_options;
mod feature;
mod host_requirements;
mod immutable_plan;
mod jsonc;
mod lifecycle;
mod metadata;
mod planning;
mod ports;
mod profile;
mod schema;
mod substitution;

pub use discovery::{
    ConfigInventory, ConfigPath, ConfigPathError, DiscoveryError, discover_config,
};
pub use docker_options::{
    BuildPlan, CreateOptionsPlan, DockerOptionError, DockerOptionErrorKind,
    DockerOptionPlanningInputs, DockerOptionsPlan, DockerfileBuildPlan, RepositoryPath,
    RepositoryPathError, plan_docker_options, validate_build_options_at_boundary,
    validate_create_options_at_boundary,
};
pub use feature::{
    FeatureContributions, FeatureError, FeatureInstallIdentity, FeatureLock, FeatureMetadata,
    FeatureOption, FeaturePackage, FeatureReference, FeatureRequest, FeatureValue, LockedFeature,
    ResolvedFeature, ResolvedFeatures, resolve_features,
};
pub use host_requirements::{
    GpuAccessIntent, GpuCapabilities, HostCapabilities, HostRequirementError,
    HostRequirementEvaluation, HostRequirementWarning, HostRequirementWarningKind, HostResource,
    Measured, UnknownMeasurement, evaluate_host_requirements,
};
pub use immutable_plan::{
    CategoryFingerprints, DesiredPlan, DriftClassification, ImmutablePlan, ImmutablePlanInputs,
    ImmutablePlanSummary, PlanCategory, classify_drift, plan_immutable,
};
pub use jsonc::{
    BoundKind, Diagnostic, JsoncError, MAX_ARRAY_ITEMS, MAX_CONFIG_BYTES,
    MAX_LIFECYCLE_GROUP_ITEMS, MAX_NESTING_DEPTH, MAX_OBJECT_ITEMS, MAX_STRING_BYTES, ParseLimits,
    RawDocument, SourceSpan, parse_jsonc,
};
pub use lifecycle::{
    ActiveGenerationLifecycle, LifecycleCheckpoint, LifecycleCommand, LifecyclePlan,
    LifecyclePlanningError, LifecycleProcess, LifecycleStage, LifecycleStagePlan,
    LifecycleTransitionError, LifecycleTrigger, plan_lifecycle,
};
pub use metadata::{
    EffectiveLifecycle, EffectiveMetadata, ImageMetadata, MetadataError, merge_image_metadata,
};
pub use planning::{
    ContainerPath, ContainerUser, EnvironmentPlan, EnvironmentSummary, HostMountSource,
    HostUserIdentity, MountOption, PathValidationError, PlannedMount, PlanningError,
    PlanningErrorKind, RuntimePlan, RuntimePlanSummary, RuntimePlanningInputs, ScenarioMetadata,
    UidUpdateIntent, UidUpdateSkipReason, UidUpdateSummary, UserValidationError, WorkspacePlan,
    plan_runtime,
};
pub use ports::{
    EffectivePortAttributes, ForwardRequest, ForwardTargetHost, ForwardingRenderInput, PortNumber,
    PortNumberError, PortPlan, PortPlanningError, PortPlanningErrorKind, PortPlanningWarning,
    PortPlanningWarningKind, PortRange, PublicationBinding, PublicationProtocol,
    PublicationRequest, plan_ports,
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
pub use substitution::{
    DeferredString, HostSubstitutionInputs, ResolvedString, StableIdentityLabels,
    SubstitutionError, SubstitutionProperty, SubstitutionSummary, substitute_host,
};
