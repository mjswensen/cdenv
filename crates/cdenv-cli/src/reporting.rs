//! Read-only live workspace correlation and deterministic report rendering.

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use cdenv_core::{
    ConfigurationDrift, ConfigurationStatus, ConfigurationValidity, EnvironmentStatus,
    ForegroundOperation, ForwardingEndpointAssignment, ForwardingStatus, LifecycleStatus,
    LocalHealthStatus, RuntimeConfigurationDrift, StatusDimensions, StatusFact, StatusFactCode,
    StatusFactSeverity, WorkspaceName, WorkspaceStatus,
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    ActiveScenario, BollardAdapter, CdenvRoot, ContainerDiscoveryScope, CorrelatedContainers,
    DiscoveredContainer, EnumeratedWorkspace, EnumerationError, Installation,
    PersistedOperationStatus, PlanFingerprints, ProcessDockerEnvironment,
    SupervisorRuntimeInspection, WorkspaceEntryStatus, WorkspacePaths, correlate_containers,
    enumerate_workspaces, inspect_supervisor_runtime,
};

const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// Docker observation supplied to one read-only correlation pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerSnapshot {
    /// One successful installation-wide `all=true` list response.
    Available(Vec<DiscoveredContainer>),
    /// Docker endpoint selection or the single list request failed.
    Unavailable(String),
}

/// A global failure before per-workspace reporting can begin.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReportingError {
    /// Local workspace enumeration failed.
    #[error(transparent)]
    Enumeration(#[from] EnumerationError),
    /// The async Docker query runtime could not be constructed.
    #[error("cannot initialize the Docker status query: {0}")]
    Runtime(#[source] io::Error),
}

/// Enumerates locally, then performs at most one installation-wide Docker list query.
///
/// Docker endpoint, connector, installation-record, and list failures become a
/// snapshot warning rather than a global failure. No daemon repair or probe is
/// attempted before the single `all=true` list request.
///
/// # Errors
///
/// Returns [`ReportingError`] only when local enumeration or runtime setup fails.
pub fn collect_live_workspace_reports(
    root: &CdenvRoot,
) -> Result<(Vec<EnumeratedWorkspace>, DockerSnapshot), ReportingError> {
    let entries = enumerate_workspaces(root)?;
    let record = match Installation::load_record_read_only(root) {
        Ok(record) => record,
        Err(error) => return Ok((entries, DockerSnapshot::Unavailable(error.to_string()))),
    };
    let endpoint = match crate::DockerEndpoint::resolve(&ProcessDockerEnvironment) {
        Ok(endpoint) => endpoint,
        Err(error) => return Ok((entries, DockerSnapshot::Unavailable(error.to_string()))),
    };
    let connector = match endpoint.bollard_connector(crate::BOLLARD_CONTROL_TIMEOUT) {
        Ok(connector) => connector,
        Err(error) => return Ok((entries, DockerSnapshot::Unavailable(error.to_string()))),
    };
    let adapter = BollardAdapter::from_connector(&connector);
    let runtime = tokio::runtime::Runtime::new().map_err(ReportingError::Runtime)?;
    let result = runtime.block_on(adapter.discover(ContainerDiscoveryScope {
        installation: record.installation_id(),
        workspace: None,
        generation: None,
    }));
    let docker = match result {
        Ok(containers) => DockerSnapshot::Available(containers),
        Err(error) => DockerSnapshot::Unavailable(error.to_string()),
    };
    Ok((entries, docker))
}

/// Concise status data used by `list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListItem {
    name: String,
    status: WorkspaceStatus,
    source: Option<String>,
    config: Option<String>,
    profile: Option<String>,
    credentials: Option<crate::CredentialStatusReport>,
    message: Option<String>,
}

impl WorkspaceListItem {
    /// Returns the deterministically sorted workspace name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the complete orthogonal status dimensions.
    #[must_use]
    pub const fn status(&self) -> &WorkspaceStatus {
        &self.status
    }
}

/// Active scenario details retained by status JSON.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ScenarioReport {
    /// Image-based active generation.
    Image,
    /// Dockerfile-based active generation.
    Dockerfile,
    /// Compose project and managed service set.
    Compose {
        /// Isolated Compose project name.
        project: String,
        /// Services managed by cdenv.
        #[serde(rename = "managedServices")]
        managed_services: Vec<String>,
    },
}

/// Desired and active category fingerprints.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FingerprintReport {
    desired: FingerprintValues,
    active: Option<FingerprintValues>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
struct FingerprintValues {
    build: String,
    create: String,
    runtime: String,
}

/// Detailed, credential-safe status for one valid local workspace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatusReport {
    name: String,
    source: String,
    checkout_path: String,
    selected_config: String,
    profile: String,
    current_git_branch: Option<String>,
    status: WorkspaceStatus,
    fingerprints: FingerprintReport,
    scenario: Option<ScenarioReport>,
    lifecycle_completed_through: Option<crate::LifecycleStage>,
    lifecycle_running: Option<crate::LifecycleStage>,
    forwarding_endpoints: Vec<ForwardingEndpointAssignment>,
    container_id: Option<String>,
    container_architecture: Option<String>,
    remote_user: Option<String>,
    remote_workspace_folder: Option<String>,
    feature_digests: BTreeMap<String, String>,
    agent_build_id: Option<String>,
    agent_protocol_version: Option<u32>,
    credentials: crate::CredentialStatusReport,
}

impl WorkspaceStatusReport {
    /// Returns the workspace identity.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the derived status dimensions and facts.
    #[must_use]
    pub const fn status(&self) -> &WorkspaceStatus {
        &self.status
    }

    /// Reports whether `status` must exit nonzero for this requested state.
    #[must_use]
    pub fn is_requested_state_failure(&self) -> bool {
        let dimensions = self.status.dimensions();
        matches!(
            dimensions.environment(),
            EnvironmentStatus::Missing
                | EnvironmentStatus::PartiallyRunning
                | EnvironmentStatus::Ambiguous
                | EnvironmentStatus::DockerUnavailable
        ) || matches!(
            dimensions.lifecycle(),
            LifecycleStatus::Failed | LifecycleStatus::Indeterminate
        ) || !matches!(
            dimensions.forwarding(),
            ForwardingStatus::Active | ForwardingStatus::NotConfigured
        ) || !matches!(dimensions.local_health(), LocalHealthStatus::Valid)
            || self.credentials.is_unavailable()
            || self
                .status
                .facts()
                .iter()
                .any(|fact| fact.severity() == StatusFactSeverity::Error)
    }
}

/// A complete deterministic list result.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceListReport {
    workspaces: Vec<WorkspaceListItem>,
    #[serde(skip)]
    docker_warning: Option<String>,
}

impl WorkspaceListReport {
    /// Returns sorted list items.
    #[must_use]
    pub fn workspaces(&self) -> &[WorkspaceListItem] {
        &self.workspaces
    }

    /// Returns the bounded Docker-query diagnostic when live truth was unavailable.
    #[must_use]
    pub fn docker_warning(&self) -> Option<&str> {
        self.docker_warning.as_deref()
    }
}

/// A requested workspace cannot produce detailed status.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum StatusRequestError {
    /// No local directory has the requested name.
    #[error("workspace `{name}` does not exist")]
    Missing {
        /// Requested workspace.
        name: WorkspaceName,
    },
    /// The local entry is corrupt, unsupported, or unsafe.
    #[error("workspace `{name}` cannot be read: {message}")]
    Invalid {
        /// Requested workspace.
        name: WorkspaceName,
        /// Safe local-state diagnostic.
        message: String,
    },
}

/// Correlates sorted local entries against one optional Docker snapshot.
///
/// This function is pure apart from read-only supervisor and Git-branch probes.
/// It never creates, repairs, migrates, provisions, or starts anything.
#[must_use]
pub fn correlate_workspace_reports(
    root: &CdenvRoot,
    entries: &[EnumeratedWorkspace],
    docker: &DockerSnapshot,
) -> WorkspaceListReport {
    let inputs = entries
        .iter()
        .filter_map(valid_correlation_input)
        .collect::<Vec<_>>();
    let correlations = match docker {
        DockerSnapshot::Available(containers) => correlate_containers(containers, &inputs),
        DockerSnapshot::Unavailable(_) => Vec::new(),
    };
    let by_name = correlations
        .iter()
        .map(|correlation| (correlation.workspace.as_str(), correlation))
        .collect::<BTreeMap<_, _>>();
    let workspaces = entries
        .iter()
        .map(|entry| list_item(root, entry, docker, by_name.get(entry.name()).copied()))
        .collect();
    let docker_warning = match docker {
        DockerSnapshot::Available(_) => None,
        DockerSnapshot::Unavailable(message) => Some(message.clone()),
    };
    WorkspaceListReport {
        workspaces,
        docker_warning,
    }
}

fn valid_correlation_input(entry: &EnumeratedWorkspace) -> Option<crate::WorkspaceCorrelation<'_>> {
    let WorkspaceEntryStatus::Valid { loaded, .. } = entry.status() else {
        return None;
    };
    let state = loaded.state();
    let active = state.active()?;
    Some(crate::WorkspaceCorrelation {
        workspace: state.name(),
        generation: active.generation(),
        recorded_container: Some(active.container_id()),
    })
}

fn list_item(
    root: &CdenvRoot,
    entry: &EnumeratedWorkspace,
    docker: &DockerSnapshot,
    correlation: Option<&CorrelatedContainers>,
) -> WorkspaceListItem {
    match entry.status() {
        WorkspaceEntryStatus::Valid { loaded, operation } => {
            let state = loaded.state();
            let paths = root.workspace(state.name());
            let status = derive_valid_status(state, *operation, &paths, docker, correlation, false);
            WorkspaceListItem {
                name: entry.name().to_owned(),
                status,
                source: Some(state.repository_source().as_str().to_owned()),
                config: Some(state.desired_devcontainer_config().as_str().to_owned()),
                profile: Some(state.devcontainer_profile().as_str().to_owned()),
                credentials: Some(crate::credential_status(root, state.name())),
                message: None,
            }
        }
        invalid => WorkspaceListItem {
            name: entry.name().to_owned(),
            status: invalid_local_status(invalid),
            source: None,
            config: None,
            profile: None,
            credentials: None,
            message: Some(entry_message(invalid)),
        },
    }
}

fn derive_valid_status(
    state: &crate::WorkspaceState,
    operation: PersistedOperationStatus,
    paths: &WorkspacePaths<'_>,
    docker: &DockerSnapshot,
    correlation: Option<&CorrelatedContainers>,
    requested: bool,
) -> WorkspaceStatus {
    let environment = derive_environment(state.active(), docker, correlation);
    let operation_kind = match operation {
        PersistedOperationStatus::Idle => ForegroundOperation::Idle,
        PersistedOperationStatus::Active(kind) | PersistedOperationStatus::Interrupted(kind) => {
            kind
        }
    };
    let configuration = derive_configuration(state.desired_fingerprints(), state.active());
    let lifecycle = derive_lifecycle(state.active());
    let (forwarding, endpoints) = derive_forwarding(state, paths, environment);
    let local_health = derive_local_health(state, operation, correlation);
    let dimensions = StatusDimensions::new(
        environment,
        operation_kind,
        configuration,
        lifecycle,
        forwarding,
        local_health,
    );
    let facts = derive_facts(dimensions, &endpoints, requested);
    WorkspaceStatus::new(dimensions, endpoints, facts)
}

fn derive_environment(
    active: Option<&crate::ActiveGeneration>,
    docker: &DockerSnapshot,
    correlation: Option<&CorrelatedContainers>,
) -> EnvironmentStatus {
    if matches!(docker, DockerSnapshot::Unavailable(_)) {
        return EnvironmentStatus::DockerUnavailable;
    }
    let Some(active) = active else {
        return EnvironmentStatus::Missing;
    };
    let Some(correlation) = correlation else {
        return EnvironmentStatus::Missing;
    };
    if correlation.current.is_empty() {
        return EnvironmentStatus::Missing;
    }

    let expected_services = active.scenario().compose().map(|(_, services)| services);
    let ambiguous = if let Some(expected) = expected_services {
        let actual = correlation
            .current
            .iter()
            .filter_map(|container| container.labels.get(COMPOSE_SERVICE_LABEL))
            .collect::<Vec<_>>();
        let unique = actual.iter().copied().collect::<BTreeSet<_>>();
        actual.len() != unique.len()
            || unique.len() != expected.len()
            || expected.iter().any(|service| !unique.contains(service))
    } else {
        correlation.current.len() != 1
    };
    if ambiguous {
        return EnvironmentStatus::Ambiguous;
    }
    let running = correlation
        .current
        .iter()
        .filter(|container| container.is_running())
        .count();
    if running == correlation.current.len() {
        EnvironmentStatus::Running
    } else if running == 0 {
        EnvironmentStatus::Stopped
    } else {
        EnvironmentStatus::PartiallyRunning
    }
}

fn derive_configuration(
    desired: &PlanFingerprints,
    active: Option<&crate::ActiveGeneration>,
) -> ConfigurationStatus {
    let Some(active) = active else {
        return ConfigurationStatus::current();
    };
    ConfigurationStatus::new(
        ConfigurationValidity::Valid,
        drift(desired.build(), active.fingerprints().build()),
        drift(desired.create(), active.fingerprints().create()),
        if desired.runtime() == active.fingerprints().runtime() {
            RuntimeConfigurationDrift::Current
        } else {
            RuntimeConfigurationDrift::Pending
        },
    )
}

fn drift(left: &crate::KeyedDigest, right: &crate::KeyedDigest) -> ConfigurationDrift {
    if left == right {
        ConfigurationDrift::Current
    } else {
        ConfigurationDrift::Drifted
    }
}

fn derive_lifecycle(active: Option<&crate::ActiveGeneration>) -> LifecycleStatus {
    let Some(active) = active else {
        return LifecycleStatus::Complete;
    };
    if active.lifecycle().indeterminate() {
        LifecycleStatus::Indeterminate
    } else if active.lifecycle().running().is_some() {
        LifecycleStatus::RunningInBackground
    } else {
        LifecycleStatus::Complete
    }
}

fn derive_forwarding(
    workspace: &crate::WorkspaceState,
    paths: &WorkspacePaths<'_>,
    environment: EnvironmentStatus,
) -> (ForwardingStatus, Vec<ForwardingEndpointAssignment>) {
    let Some(active) = workspace.active() else {
        return (ForwardingStatus::NotConfigured, Vec::new());
    };
    let forwarding = active.forwarding();
    let endpoints = forwarding.assigned().to_vec();
    if forwarding.requested().is_empty() {
        return (ForwardingStatus::NotConfigured, endpoints);
    }
    if !matches!(environment, EnvironmentStatus::Running) {
        return (ForwardingStatus::TargetUnavailable, endpoints);
    }
    let runtime = inspect_supervisor_runtime(*paths);
    if !matches!(
        runtime,
        Ok(SupervisorRuntimeInspection {
            directory: crate::ManagedPathState::Valid,
            socket: crate::ManagedPathState::Valid,
            state_file: crate::ManagedPathState::Valid,
            lifetime_lock: crate::ManagedPathState::Valid
        })
    ) {
        return (ForwardingStatus::MissingSupervisor, endpoints);
    }
    let state = crate::load_supervisor_state(&paths.supervisor_state_file());
    let identity_matches = state.is_ok_and(|state| {
        state.installation == *workspace.installation_id()
            && state.workspace == *workspace.name()
            && state.generation == active.generation()
            && state.agent_build_id == *active.provisioned().agent_build_id()
            && state.agent_protocol == active.provisioned().protocol_version()
            && forwarding.supervisor_build_id() == Some(&state.host_build_id)
            && state.listeners.len() == endpoints.len()
            && state
                .listeners
                .iter()
                .zip(&endpoints)
                .all(|(listener, endpoint)| {
                    endpoint.assigned().is_some_and(|assigned| {
                        listener.ip() == assigned.address()
                            && listener.port() == assigned.port().get()
                    })
                })
    });
    if endpoints.len() != forwarding.requested().len()
        || endpoints
            .iter()
            .any(|endpoint| endpoint.assigned().is_none())
        || !identity_matches
    {
        (ForwardingStatus::Degraded, endpoints)
    } else {
        (ForwardingStatus::Active, endpoints)
    }
}

fn derive_local_health(
    state: &crate::WorkspaceState,
    operation: PersistedOperationStatus,
    correlation: Option<&CorrelatedContainers>,
) -> LocalHealthStatus {
    if matches!(operation, PersistedOperationStatus::Interrupted(_)) {
        return LocalHealthStatus::Interrupted;
    }
    if state.last_error().is_some() {
        return LocalHealthStatus::LastOperationFailed;
    }
    if let (Some(active), Some(correlation)) = (state.active(), correlation) {
        let recorded = correlation
            .current
            .iter()
            .find(|container| container.id == *active.container_id());
        let identity_drift = recorded.is_none_or(|container| {
            container.image_id.as_ref().map(crate::ImageId::as_str) != Some(active.image_id())
                || container.labels.get("cdenv.profile").map(String::as_str)
                    != Some(state.devcontainer_profile().as_str())
        });
        if identity_drift
            || !correlation.external_replacements.is_empty()
            || !correlation.stale.is_empty()
        {
            return LocalHealthStatus::ProvisionDrift;
        }
    }
    LocalHealthStatus::Valid
}

fn derive_facts(
    dimensions: StatusDimensions,
    endpoints: &[ForwardingEndpointAssignment],
    requested: bool,
) -> Vec<StatusFact> {
    let mut facts = Vec::new();
    let contextual = |code| {
        if requested {
            StatusFact::error(code)
        } else {
            StatusFact::warning(code)
        }
    };
    match dimensions.environment() {
        EnvironmentStatus::DockerUnavailable => {
            facts.push(contextual(StatusFactCode::DockerUnavailable));
        }
        EnvironmentStatus::Ambiguous => {
            facts.push(StatusFact::error(StatusFactCode::AmbiguousContainers));
        }
        _ => {}
    }
    if dimensions.configuration().build() == ConfigurationDrift::Drifted {
        facts.push(StatusFact::warning(StatusFactCode::BuildDrift));
    }
    if dimensions.configuration().create() == ConfigurationDrift::Drifted {
        facts.push(StatusFact::warning(StatusFactCode::CreateDrift));
    }
    if dimensions.configuration().runtime() == RuntimeConfigurationDrift::Pending {
        facts.push(StatusFact::error(StatusFactCode::RuntimeDriftPending));
    }
    match dimensions.lifecycle() {
        LifecycleStatus::Failed => facts.push(StatusFact::error(StatusFactCode::LifecycleFailed)),
        LifecycleStatus::Indeterminate => {
            facts.push(StatusFact::error(StatusFactCode::LifecycleIndeterminate));
        }
        _ => {}
    }
    match dimensions.forwarding() {
        ForwardingStatus::Degraded => {
            facts.push(StatusFact::error(StatusFactCode::ForwardingDegraded));
        }
        ForwardingStatus::MissingSupervisor => facts.push(StatusFact::error(
            StatusFactCode::MissingForwardingSupervisor,
        )),
        ForwardingStatus::TargetUnavailable => facts.push(StatusFact::error(
            StatusFactCode::ForwardingTargetUnavailable,
        )),
        _ => {}
    }
    match dimensions.local_health() {
        LocalHealthStatus::Interrupted => {
            facts.push(StatusFact::error(StatusFactCode::InterruptedOperation));
        }
        LocalHealthStatus::LastOperationFailed => {
            facts.push(StatusFact::error(StatusFactCode::LastOperationFailed));
        }
        LocalHealthStatus::Corrupt => {
            facts.push(StatusFact::error(StatusFactCode::CorruptLocalState));
        }
        LocalHealthStatus::ProvisionDrift => {
            facts.push(StatusFact::error(StatusFactCode::ProvisionDrift));
        }
        LocalHealthStatus::Valid => {}
    }
    if endpoints.iter().any(|endpoint| {
        endpoint
            .assigned()
            .is_some_and(|assigned| assigned != endpoint.requested())
    }) {
        facts.push(StatusFact::warning(
            StatusFactCode::AlternateForwardingEndpoint,
        ));
    }
    facts
}

fn invalid_local_status(entry: &WorkspaceEntryStatus) -> WorkspaceStatus {
    let dimensions = StatusDimensions::new(
        EnvironmentStatus::Missing,
        ForegroundOperation::Idle,
        ConfigurationStatus::current(),
        LifecycleStatus::Complete,
        ForwardingStatus::NotConfigured,
        LocalHealthStatus::Corrupt,
    );
    let _ = entry;
    WorkspaceStatus::new(
        dimensions,
        Vec::new(),
        vec![StatusFact::error(StatusFactCode::CorruptLocalState)],
    )
}

fn entry_message(entry: &WorkspaceEntryStatus) -> String {
    match entry {
        WorkspaceEntryStatus::InvalidName => "invalid workspace directory name".to_owned(),
        WorkspaceEntryStatus::MissingState => "workspace state is missing".to_owned(),
        WorkspaceEntryStatus::Corrupt { message } | WorkspaceEntryStatus::Unsafe { message } => {
            message.clone()
        }
        WorkspaceEntryStatus::NewerSchema { found, supported } => {
            format!("state schema {found} is newer than supported schema {supported}")
        }
        WorkspaceEntryStatus::UnsupportedOlderSchema { found } => {
            format!("state schema {found} has no supported migration")
        }
        WorkspaceEntryStatus::Valid { .. } => String::new(),
    }
}

/// Builds detailed status for one requested workspace from an existing list pass.
///
/// # Errors
///
/// Returns [`StatusRequestError`] when the requested local entry is absent or invalid.
pub fn requested_workspace_status(
    root: &CdenvRoot,
    entries: &[EnumeratedWorkspace],
    docker: &DockerSnapshot,
    name: &WorkspaceName,
) -> Result<WorkspaceStatusReport, StatusRequestError> {
    let entry = entries
        .iter()
        .find(|entry| entry.name() == name.as_str())
        .ok_or_else(|| StatusRequestError::Missing { name: name.clone() })?;
    let WorkspaceEntryStatus::Valid { loaded, operation } = entry.status() else {
        return Err(StatusRequestError::Invalid {
            name: name.clone(),
            message: entry_message(entry.status()),
        });
    };
    let state = loaded.state();
    let active = state.active();
    let correlation = match docker {
        DockerSnapshot::Available(containers) => active.and_then(|active| {
            correlate_containers(
                containers,
                &[crate::WorkspaceCorrelation {
                    workspace: state.name(),
                    generation: active.generation(),
                    recorded_container: Some(active.container_id()),
                }],
            )
            .into_iter()
            .next()
        }),
        DockerSnapshot::Unavailable(_) => None,
    };
    let paths = root.workspace(state.name());
    let status = derive_valid_status(
        state,
        *operation,
        &paths,
        docker,
        correlation.as_ref(),
        true,
    );
    let desired = fingerprint_values(state.desired_fingerprints());
    let active_fingerprints = active.map(|active| fingerprint_values(active.fingerprints()));
    Ok(WorkspaceStatusReport {
        name: name.to_string(),
        source: state.repository_source().as_str().to_owned(),
        checkout_path: paths.checkout_dir().display().to_string(),
        selected_config: state.desired_devcontainer_config().as_str().to_owned(),
        profile: state.devcontainer_profile().as_str().to_owned(),
        current_git_branch: current_git_branch(&paths.checkout_dir()),
        status,
        fingerprints: FingerprintReport {
            desired,
            active: active_fingerprints,
        },
        scenario: active.map(|active| scenario_report(active.scenario())),
        lifecycle_completed_through: active
            .and_then(|active| active.lifecycle().completed_through()),
        lifecycle_running: active.and_then(|active| active.lifecycle().running()),
        forwarding_endpoints: active
            .map_or_else(Vec::new, |active| active.forwarding().assigned().to_vec()),
        container_id: active.map(|active| active.container_id().to_string()),
        container_architecture: active
            .map(|active| active.provisioned().container_architecture().to_string()),
        remote_user: active.map(|active| active.provisioned().remote_user().to_owned()),
        remote_workspace_folder: active
            .map(|active| active.provisioned().remote_workspace_folder().to_owned()),
        feature_digests: active
            .map_or_else(BTreeMap::new, |active| active.feature_digests().clone()),
        agent_build_id: active.map(|active| active.provisioned().agent_build_id().to_string()),
        agent_protocol_version: active.map(|active| active.provisioned().protocol_version().get()),
        credentials: crate::credential_status(root, name),
    })
}

fn fingerprint_values(fingerprints: &PlanFingerprints) -> FingerprintValues {
    FingerprintValues {
        build: fingerprints.build().to_string(),
        create: fingerprints.create().to_string(),
        runtime: fingerprints.runtime().to_string(),
    }
}

fn scenario_report(scenario: &ActiveScenario) -> ScenarioReport {
    match scenario {
        ActiveScenario::Image => ScenarioReport::Image,
        ActiveScenario::Dockerfile => ScenarioReport::Dockerfile,
        ActiveScenario::Compose { .. } => {
            let Some((project, managed_services)) = scenario.compose() else {
                unreachable!("the matched Compose scenario exposes Compose details");
            };
            ScenarioReport::Compose {
                project: project.to_owned(),
                managed_services: managed_services.to_vec(),
            }
        }
    }
}

fn current_git_branch(checkout: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["symbolic-ref", "--quiet", "--short", "HEAD"])
        .current_dir(checkout)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = std::str::from_utf8(&output.stdout).ok()?.trim();
    (!branch.is_empty() && !branch.chars().any(char::is_control)).then(|| branch.to_owned())
}

/// Renders a concise deterministic human list.
///
/// # Errors
///
/// Returns an I/O error when the destination cannot be written.
pub fn render_human_list(
    writer: &mut (impl Write + ?Sized),
    report: &WorkspaceListReport,
) -> io::Result<()> {
    writeln!(
        writer,
        "NAME\tENVIRONMENT\tOPERATION\tCONFIGURATION\tHEALTH\tCREDENTIALS"
    )?;
    for item in report.workspaces() {
        let dimensions = item.status().dimensions();
        writeln!(
            writer,
            "{}\t{}\t{}\t{}/{}/{}\t{}\t{}",
            item.name,
            enum_name(dimensions.environment()),
            enum_name(dimensions.operation()),
            enum_name(dimensions.configuration().build()),
            enum_name(dimensions.configuration().create()),
            enum_name(dimensions.configuration().runtime()),
            enum_name(dimensions.local_health()),
            item.credentials
                .as_ref()
                .map_or("unavailable", crate::CredentialStatusReport::summary),
        )?;
    }
    Ok(())
}

/// Renders deterministic detailed human status.
///
/// # Errors
///
/// Returns an I/O error when the destination cannot be written.
pub fn render_human_status(
    writer: &mut (impl Write + ?Sized),
    report: &WorkspaceStatusReport,
) -> io::Result<()> {
    let dimensions = report.status.dimensions();
    writeln!(writer, "Workspace: {}", report.name)?;
    writeln!(writer, "Source: {}", report.source)?;
    writeln!(writer, "Checkout: {}", report.checkout_path)?;
    writeln!(
        writer,
        "Config: {} ({})",
        report.selected_config, report.profile
    )?;
    writeln!(
        writer,
        "Branch: {}",
        report
            .current_git_branch
            .as_deref()
            .unwrap_or("detached or unavailable")
    )?;
    writeln!(
        writer,
        "Environment: {}",
        enum_name(dimensions.environment())
    )?;
    writeln!(writer, "Operation: {}", enum_name(dimensions.operation()))?;
    writeln!(writer, "Lifecycle: {}", enum_name(dimensions.lifecycle()))?;
    writeln!(writer, "Forwarding: {}", enum_name(dimensions.forwarding()))?;
    writeln!(writer, "Credentials: {}", report.credentials.summary())?;
    writeln!(
        writer,
        "Local health: {}",
        enum_name(dimensions.local_health())
    )?;
    render_human_active_details(writer, report)?;
    for fact in report.status.facts() {
        writeln!(writer, "{:?}: {}", fact.severity(), enum_name(fact.code()))?;
    }
    Ok(())
}

fn render_human_active_details(
    writer: &mut (impl Write + ?Sized),
    report: &WorkspaceStatusReport,
) -> io::Result<()> {
    let active = report.fingerprints.active.as_ref();
    writeln!(
        writer,
        "Fingerprints: build {} -> {}; create {} -> {}; runtime {} -> {}",
        report.fingerprints.desired.build,
        active.map_or("none", |value| value.build.as_str()),
        report.fingerprints.desired.create,
        active.map_or("none", |value| value.create.as_str()),
        report.fingerprints.desired.runtime,
        active.map_or("none", |value| value.runtime.as_str()),
    )?;
    match &report.scenario {
        Some(ScenarioReport::Image) => writeln!(writer, "Scenario: image")?,
        Some(ScenarioReport::Dockerfile) => writeln!(writer, "Scenario: dockerfile")?,
        Some(ScenarioReport::Compose {
            project,
            managed_services,
        }) => writeln!(
            writer,
            "Scenario: compose {} [{}]",
            safe_text(project),
            managed_services
                .iter()
                .map(|service| safe_text(service))
                .collect::<Vec<_>>()
                .join(", ")
        )?,
        None => writeln!(writer, "Scenario: none")?,
    }
    writeln!(
        writer,
        "Lifecycle stages: completed={}; running={}",
        report
            .lifecycle_completed_through
            .map_or_else(|| "none".to_owned(), enum_name),
        report
            .lifecycle_running
            .map_or_else(|| "none".to_owned(), enum_name),
    )?;
    for endpoint in &report.forwarding_endpoints {
        let requested = endpoint.requested();
        let assigned = endpoint.assigned().map_or_else(
            || "pending".to_owned(),
            |assigned| format!("{}:{}", assigned.address(), assigned.port()),
        );
        writeln!(
            writer,
            "Port: {}:{} -> {}",
            requested.address(),
            requested.port(),
            assigned
        )?;
    }
    writeln!(
        writer,
        "Container: {} ({})",
        report.container_id.as_deref().unwrap_or("none"),
        report
            .container_architecture
            .as_deref()
            .unwrap_or("unknown")
    )?;
    writeln!(
        writer,
        "Remote: {} at {}",
        safe_text(report.remote_user.as_deref().unwrap_or("unknown")),
        safe_text(
            report
                .remote_workspace_folder
                .as_deref()
                .unwrap_or("unknown")
        )
    )?;
    for (feature, digest) in &report.feature_digests {
        writeln!(
            writer,
            "Feature: {} {}",
            safe_text(feature),
            safe_text(digest)
        )?;
    }
    writeln!(
        writer,
        "Agent: build={} protocol={}",
        report.agent_build_id.as_deref().unwrap_or("unknown"),
        report
            .agent_protocol_version
            .map_or_else(|| "unknown".to_owned(), |version| version.to_string())
    )
}

fn safe_text(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn enum_name(value: impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_owned())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::SystemTime;

    use cdenv_core::{
        ContainerId, EnvironmentStatus, ForwardingStatus, LocalHealthStatus,
        RuntimeConfigurationDrift, WorkspaceName,
    };

    use super::*;
    use crate::{ImageId, RootEnvironment, reserve_workspace};

    const PRIMARY_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SECOND_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const IMAGE_ID: &str =
        "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct NoEnvironment;

    impl RootEnvironment for NoEnvironment {
        fn cdenv_home(&self) -> Option<std::ffi::OsString> {
            None
        }

        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn root(parent: &Path) -> CdenvRoot {
        CdenvRoot::resolve(Some(&parent.join("root")), &NoEnvironment)
            .expect("test root should resolve")
    }

    fn write_compose_workspace(root: &CdenvRoot, last_error: bool) -> Vec<u8> {
        let name = WorkspaceName::parse("project").expect("workspace name");
        let reservation = reserve_workspace(root, &name).expect("workspace reservation");
        let mut value: serde_json::Value = serde_json::from_slice(include_bytes!(
            "../tests/fixtures/state-compose-background.json"
        ))
        .expect("fixture JSON");
        if !last_error {
            value["lastError"] = serde_json::Value::Null;
        }
        let bytes = serde_json::to_vec_pretty(&value).expect("state bytes");
        fs::write(root.workspace(&name).state_file(), &bytes).expect("state write");
        reservation.commit();
        bytes
    }

    fn container(id: &str, service: &str, generation: &str, running: bool) -> DiscoveredContainer {
        DiscoveredContainer {
            id: ContainerId::parse(id).expect("container ID"),
            names: vec![format!("project-{service}")],
            image_id: Some(ImageId::parse(IMAGE_ID).expect("image ID")),
            labels: BTreeMap::from([
                ("cdenv.workspace".to_owned(), "project".to_owned()),
                ("cdenv.generation".to_owned(), generation.to_owned()),
                (
                    "cdenv.profile".to_owned(),
                    "cdenv-devcontainer-v1".to_owned(),
                ),
                (COMPOSE_SERVICE_LABEL.to_owned(), service.to_owned()),
            ]),
            state: Some(if running { "running" } else { "exited" }.to_owned()),
        }
    }

    #[test]
    fn combined_dimensions_are_derived_without_collapsing_each_other() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = root(temporary.path());
        write_compose_workspace(&root, true);
        let entries = enumerate_workspaces(&root).expect("enumeration");
        let docker = DockerSnapshot::Available(vec![
            container(PRIMARY_ID, "app", "3", true),
            container(SECOND_ID, "db", "3", true),
        ]);

        let report = correlate_workspace_reports(&root, &entries, &docker);
        let dimensions = report.workspaces()[0].status().dimensions();

        assert_eq!(
            (
                dimensions.environment(),
                dimensions.configuration().runtime(),
                dimensions.lifecycle(),
                dimensions.forwarding(),
                dimensions.local_health(),
            ),
            (
                EnvironmentStatus::Running,
                RuntimeConfigurationDrift::Pending,
                LifecycleStatus::RunningInBackground,
                ForwardingStatus::MissingSupervisor,
                LocalHealthStatus::LastOperationFailed,
            )
        );
    }

    #[test]
    fn duplicate_compose_service_is_ambiguous_without_selecting_a_container() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = root(temporary.path());
        write_compose_workspace(&root, false);
        let entries = enumerate_workspaces(&root).expect("enumeration");
        let docker = DockerSnapshot::Available(vec![
            container(PRIMARY_ID, "app", "3", true),
            container(SECOND_ID, "app", "3", true),
        ]);

        let report = correlate_workspace_reports(&root, &entries, &docker);

        assert_eq!(
            report.workspaces()[0].status().dimensions().environment(),
            EnvironmentStatus::Ambiguous
        );
    }

    #[test]
    fn stale_resource_is_reported_as_provision_drift() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = root(temporary.path());
        write_compose_workspace(&root, false);
        let entries = enumerate_workspaces(&root).expect("enumeration");
        let docker = DockerSnapshot::Available(vec![
            container(PRIMARY_ID, "app", "3", true),
            container(SECOND_ID, "db", "3", true),
            container(
                "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                "old",
                "2",
                false,
            ),
        ]);

        let report = correlate_workspace_reports(&root, &entries, &docker);

        assert_eq!(
            report.workspaces()[0].status().dimensions().local_health(),
            LocalHealthStatus::ProvisionDrift
        );
    }

    #[test]
    fn docker_unavailable_is_read_only_and_retained_with_no_workspaces() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = root(temporary.path());
        let bytes = write_compose_workspace(&root, false);
        let path = root
            .workspace(&WorkspaceName::parse("project").expect("name"))
            .state_file();
        let modified = fs::metadata(&path)
            .expect("metadata")
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let entries = enumerate_workspaces(&root).expect("enumeration");

        let report = correlate_workspace_reports(
            &root,
            &entries,
            &DockerSnapshot::Unavailable("daemon offline".to_owned()),
        );

        assert_eq!(
            (
                report.workspaces()[0].status().dimensions().environment(),
                report.docker_warning(),
                fs::read(&path).expect("state bytes"),
                fs::metadata(path)
                    .expect("metadata")
                    .modified()
                    .unwrap_or(SystemTime::UNIX_EPOCH),
            ),
            (
                EnvironmentStatus::DockerUnavailable,
                Some("daemon offline"),
                bytes,
                modified,
            )
        );
    }

    #[test]
    fn list_json_payload_is_sorted_and_does_not_serialize_internal_diagnostics() {
        let report = WorkspaceListReport {
            workspaces: Vec::new(),
            docker_warning: Some("private adapter detail".to_owned()),
        };

        let value = serde_json::to_value(report).expect("report JSON");

        assert_eq!(value, serde_json::json!({"workspaces": []}));
    }
}
