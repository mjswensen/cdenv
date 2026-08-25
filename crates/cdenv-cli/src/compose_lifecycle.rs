//! Drift-safe lifecycle coordination for persisted Docker Compose service sets.

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;

use cdenv_core::ContainerId;
use cdenv_devcontainer::PortPlan;
use thiserror::Error;

use crate::bollard::{
    BollardApi, COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, GENERATION_LABEL, INSTALLATION_LABEL,
    PROFILE_LABEL, WORKSPACE_LABEL,
};
use crate::{
    BollardAdapter, BollardAdapterError, CancellationToken, ComposeAdapter, ComposeAdapterError,
    ComposePrimaryExpectation, ComposeStopRequest, ComposeUpClaim, ComposeUpRequest,
    ContainerDiscoveryScope, ContainerInspection, DiscoveredContainer, DockerResourceIdentity,
    ImageId, verify_port_bindings,
};

/// Static Compose seam kept narrow enough for deterministic component tests.
#[doc(hidden)]
pub trait ComposeLifecycleCli: Send + Sync {
    /// Performs the sole create/rebuild reconciliation operation.
    fn up<'a>(
        &'a self,
        request: &'a ComposeUpRequest<'a>,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<ComposeUpClaim, ComposeAdapterError>> + Send + 'a;

    /// Stops exactly the persisted managed service set.
    fn stop<'a>(
        &'a self,
        request: &'a ComposeStopRequest<'a>,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<(), ComposeAdapterError>> + Send + 'a;
}

impl ComposeLifecycleCli for ComposeAdapter {
    async fn up(
        &self,
        request: &ComposeUpRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeUpClaim, ComposeAdapterError> {
        self.up(request, cancellation).await
    }

    async fn stop(
        &self,
        request: &ComposeStopRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<(), ComposeAdapterError> {
        self.stop(request, cancellation).await
    }
}

/// Static Docker control seam for Compose service-set inspection and direct restart.
#[doc(hidden)]
pub trait ComposeLifecycleEngine: Send + Sync {
    /// Discovers the exact active workspace generation.
    fn discover<'a>(
        &'a self,
        identity: DockerResourceIdentity<'a>,
    ) -> impl Future<Output = Result<Vec<DiscoveredContainer>, BollardAdapterError>> + Send + 'a;

    /// Inspects one exact service container.
    fn inspect<'a>(
        &'a self,
        id: &'a ContainerId,
    ) -> impl Future<Output = Result<ContainerInspection, BollardAdapterError>> + Send + 'a;

    /// Starts one recorded service container directly, without Compose reconciliation.
    fn start<'a>(
        &'a self,
        id: &'a ContainerId,
    ) -> impl Future<Output = Result<(), BollardAdapterError>> + Send + 'a;

    /// Independently verifies the primary claim and rejects a duplicate primary.
    fn verify_primary<'a>(
        &'a self,
        expected: ComposePrimaryExpectation<'a>,
    ) -> impl Future<Output = Result<ContainerInspection, BollardAdapterError>> + Send + 'a;
}

impl<A: BollardApi> ComposeLifecycleEngine for BollardAdapter<A> {
    async fn discover(
        &self,
        identity: DockerResourceIdentity<'_>,
    ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
        self.discover(ContainerDiscoveryScope {
            installation: identity.installation,
            workspace: Some(identity.workspace),
            generation: Some(identity.generation),
        })
        .await
    }

    async fn inspect(&self, id: &ContainerId) -> Result<ContainerInspection, BollardAdapterError> {
        self.inspect_container(id).await
    }

    async fn start(&self, id: &ContainerId) -> Result<(), BollardAdapterError> {
        self.start(id).await
    }

    async fn verify_primary(
        &self,
        expected: ComposePrimaryExpectation<'_>,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        self.verify_compose_primary(expected).await
    }
}

/// Persisted inputs used by unchanged resume and explicit down.
pub struct RecordedComposeRequest<'a> {
    /// Stable identity of the active generation.
    pub identity: DockerResourceIdentity<'a>,
    /// Isolated Compose project name.
    pub project: &'a str,
    /// Configured primary service.
    pub primary_service: &'a str,
    /// Persisted, independently verified primary container ID.
    pub primary: &'a ContainerId,
    /// Persisted complete managed service set.
    pub managed_services: &'a [String],
}

/// Inputs used only for initial creation or explicit rebuild reconciliation.
pub struct CreateComposeRequest<'a> {
    /// Adapter request containing the immutable Compose plan.
    pub up: ComposeUpRequest<'a>,
    /// Stable active-generation identity.
    pub identity: DockerResourceIdentity<'a>,
    /// Expected final primary image.
    pub primary_image: &'a ImageId,
    /// Validated create-time primary-service publications.
    pub ports: &'a PortPlan,
}

/// Verified Compose facts safe to persist as the active generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeLifecycleFacts {
    /// Verified primary ID.
    pub primary: ContainerId,
    /// Exact service/container mapping, including dependencies.
    pub managed: BTreeMap<String, ContainerId>,
}

/// Idempotent result of stopping a persisted Compose managed service set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeStopOutcome {
    /// At least one managed service transitioned to stopped.
    Stopped(ComposeLifecycleFacts),
    /// Every verified managed service was already stopped.
    AlreadyStopped(ComposeLifecycleFacts),
    /// Every persisted managed service is known missing.
    Missing,
}

/// Non-selecting live classification of a persisted Compose service set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ComposeServiceSetState {
    /// Every persisted service is running.
    CompleteRunning,
    /// Every persisted service is stopped.
    CompleteStopped,
    /// The complete set exists in mixed running state.
    PartiallyRunning {
        /// Running persisted services.
        running: Vec<String>,
        /// Stopped persisted services.
        stopped: Vec<String>,
    },
    /// One or more persisted services are absent.
    Missing {
        /// Missing persisted services.
        missing: Vec<String>,
        /// Present running persisted services.
        running: Vec<String>,
        /// Present stopped persisted services.
        stopped: Vec<String>,
    },
    /// One or more persisted service names resolve to multiple containers.
    Ambiguous {
        /// Ambiguous persisted service names.
        services: Vec<String>,
    },
    /// The primary service exists but is not the persisted primary ID.
    PrimarySubstituted {
        /// Unexpected primary ID.
        actual: ContainerId,
    },
}

/// Compose lifecycle failure that distinguishes unsafe live truth from adapter failures.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ComposeLifecycleError {
    /// Cooperative cancellation was observed between mutations.
    #[error("Compose lifecycle operation was cancelled after services {completed:?}")]
    Cancelled {
        /// Services already directly started by this operation.
        completed: Vec<String>,
    },
    /// Persisted service names are empty, duplicated, or unsafe.
    #[error("the persisted Compose managed service set is invalid")]
    InvalidManagedSet,
    /// Live state is incomplete or ambiguous and was not silently repaired.
    #[error("Compose managed service set is unsafe: {state:?}")]
    UnsafeServiceSet {
        /// Exact non-selecting classification.
        state: ComposeServiceSetState,
    },
    /// Compose subprocess operation failed.
    #[error("Docker Compose {operation} failed: {source}")]
    Compose {
        /// Safe operation name.
        operation: &'static str,
        /// Preserved adapter error.
        #[source]
        source: ComposeAdapterError,
    },
    /// Docker discovery, inspection, verification, or start failed.
    #[error("Docker {operation} failed: {source}")]
    Docker {
        /// Safe operation name.
        operation: &'static str,
        /// Preserved adapter error.
        #[source]
        source: BollardAdapterError,
    },
    /// Compose returned claims different from the immutable managed plan.
    #[error("Compose reconciliation returned a different managed service set")]
    ClaimSetMismatch,
    /// A claimed service did not retain its required cdenv and Compose identity.
    #[error("Compose service `{service}` verification failed for {field}")]
    ServiceMismatch {
        /// Persisted service.
        service: String,
        /// Safe mismatched field.
        field: &'static str,
    },
}

/// Coordinator that reconciles only on create/rebuild and directly starts recorded containers.
pub struct ComposeLifecycleOrchestrator<C, E> {
    compose: C,
    engine: E,
}

impl<C, E> ComposeLifecycleOrchestrator<C, E> {
    /// Constructs a coordinator from explicit static adapter seams.
    #[must_use]
    pub const fn new(compose: C, engine: E) -> Self {
        Self { compose, engine }
    }
}

impl<C: ComposeLifecycleCli, E: ComposeLifecycleEngine> ComposeLifecycleOrchestrator<C, E> {
    /// Reconciles during initial creation or explicit rebuild and verifies the complete claim.
    ///
    /// # Errors
    ///
    /// Returns cancellation, Compose, Docker, claim-set, or identity verification failures.
    pub async fn create_or_rebuild(
        &self,
        request: &CreateComposeRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeLifecycleFacts, ComposeLifecycleError> {
        check_cancelled(cancellation, &[])?;
        let claim = self
            .compose
            .up(&request.up, cancellation)
            .await
            .map_err(|source| ComposeLifecycleError::Compose {
                operation: "reconciliation",
                source,
            })?;
        let expected = request.up.plan.managed_services();
        let Some(claimed_primary) = claim.managed.get(request.up.plan.primary_service()) else {
            return Err(ComposeLifecycleError::ClaimSetMismatch);
        };
        if claim.managed.keys().ne(expected.iter()) || &claim.primary != claimed_primary {
            return Err(ComposeLifecycleError::ClaimSetMismatch);
        }
        check_cancelled(cancellation, &[])?;
        let primary_inspection = self
            .engine
            .verify_primary(ComposePrimaryExpectation {
                id: &claim.primary,
                image_id: request.primary_image,
                installation: request.identity.installation,
                workspace: request.identity.workspace,
                generation: request.identity.generation,
                profile: request.identity.profile,
                project: request.up.plan.project_name(),
                service: request.up.plan.primary_service(),
                running: true,
            })
            .await
            .map_err(|source| ComposeLifecycleError::Docker {
                operation: "verify Compose primary",
                source,
            })?;
        verify_port_bindings(&primary_inspection, request.ports).map_err(|source| {
            ComposeLifecycleError::Docker {
                operation: "verify Compose primary port bindings",
                source,
            }
        })?;
        for (service, id) in &claim.managed {
            check_cancelled(cancellation, &[])?;
            let inspection =
                self.engine
                    .inspect(id)
                    .await
                    .map_err(|source| ComposeLifecycleError::Docker {
                        operation: "inspect managed Compose service",
                        source,
                    })?;
            verify_service(
                &inspection,
                id,
                service,
                request.up.plan.project_name(),
                request.identity,
                true,
            )?;
        }
        Ok(ComposeLifecycleFacts {
            primary: claim.primary,
            managed: claim.managed,
        })
    }

    /// Starts stopped recorded containers directly; this never invokes Compose reconciliation.
    ///
    /// # Errors
    ///
    /// Missing, ambiguous, or substituted sets are reported without mutation.
    pub async fn resume(
        &self,
        request: &RecordedComposeRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeLifecycleFacts, ComposeLifecycleError> {
        validate_recorded(request)?;
        check_cancelled(cancellation, &[])?;
        let discovered = self.discover(request).await?;
        let (state, managed) = resolve_service_set(request, &discovered);
        if matches!(
            state,
            ComposeServiceSetState::Missing { .. }
                | ComposeServiceSetState::Ambiguous { .. }
                | ComposeServiceSetState::PrimarySubstituted { .. }
        ) {
            return Err(ComposeLifecycleError::UnsafeServiceSet { state });
        }
        let mut completed = Vec::new();
        for service in request.managed_services {
            check_cancelled(cancellation, &completed)?;
            let container = &managed[service];
            let inspection = self.engine.inspect(container).await.map_err(|source| {
                ComposeLifecycleError::Docker {
                    operation: "inspect recorded Compose service",
                    source,
                }
            })?;
            verify_service(
                &inspection,
                container,
                service,
                request.project,
                request.identity,
                None,
            )?;
            if !inspection.running {
                check_cancelled(cancellation, &completed)?;
                self.engine.start(container).await.map_err(|source| {
                    ComposeLifecycleError::Docker {
                        operation: "start recorded Compose service",
                        source,
                    }
                })?;
                completed.push(service.clone());
            }
        }
        check_cancelled(cancellation, &completed)?;
        let discovered = self.discover(request).await?;
        let (state, managed) = resolve_service_set(request, &discovered);
        if state != ComposeServiceSetState::CompleteRunning {
            return Err(ComposeLifecycleError::UnsafeServiceSet { state });
        }
        Ok(ComposeLifecycleFacts {
            primary: request.primary.clone(),
            managed,
        })
    }

    /// Idempotently stops the persisted set while preserving all project resources.
    ///
    /// A complete stopped set is verified without invoking Compose. A completely
    /// missing set is a known successful absence; partial absence and ambiguity
    /// remain errors.
    ///
    /// # Errors
    ///
    /// Returns invalid identity, partial/ambiguous state, inspection, cancellation,
    /// Compose stop, or post-stop verification failures.
    pub async fn stop_recorded(
        &self,
        recorded: &RecordedComposeRequest<'_>,
        stop: &ComposeStopRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeStopOutcome, ComposeLifecycleError> {
        validate_recorded(recorded)?;
        if stop.project.project_name != recorded.project
            || stop.managed_services != recorded.managed_services
        {
            return Err(ComposeLifecycleError::InvalidManagedSet);
        }
        check_cancelled(cancellation, &[])?;
        let discovered = self.discover(recorded).await?;
        let (state, managed) = resolve_service_set(recorded, &discovered);
        if is_completely_missing(&state, recorded.managed_services.len()) {
            return Ok(ComposeStopOutcome::Missing);
        }
        if state == ComposeServiceSetState::CompleteStopped {
            self.verify_recorded_set(recorded, &managed).await?;
            return Ok(ComposeStopOutcome::AlreadyStopped(ComposeLifecycleFacts {
                primary: recorded.primary.clone(),
                managed,
            }));
        }
        if matches!(
            state,
            ComposeServiceSetState::Missing { .. }
                | ComposeServiceSetState::Ambiguous { .. }
                | ComposeServiceSetState::PrimarySubstituted { .. }
        ) {
            return Err(ComposeLifecycleError::UnsafeServiceSet { state });
        }
        self.down(recorded, stop, cancellation)
            .await
            .map(ComposeStopOutcome::Stopped)
    }

    /// Stops the persisted set through Compose V2 and preserves all project resources.
    ///
    /// Services outside the persisted set are deliberately ignored.
    ///
    /// # Errors
    ///
    /// Refuses missing/ambiguous/substituted sets and verifies the complete stopped result.
    pub async fn down(
        &self,
        recorded: &RecordedComposeRequest<'_>,
        stop: &ComposeStopRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeLifecycleFacts, ComposeLifecycleError> {
        validate_recorded(recorded)?;
        if stop.project.project_name != recorded.project
            || stop.managed_services != recorded.managed_services
        {
            return Err(ComposeLifecycleError::InvalidManagedSet);
        }
        check_cancelled(cancellation, &[])?;
        let discovered = self.discover(recorded).await?;
        let (state, managed) = resolve_service_set(recorded, &discovered);
        if matches!(
            state,
            ComposeServiceSetState::Missing { .. }
                | ComposeServiceSetState::Ambiguous { .. }
                | ComposeServiceSetState::PrimarySubstituted { .. }
        ) {
            return Err(ComposeLifecycleError::UnsafeServiceSet { state });
        }
        for service in recorded.managed_services {
            let container = &managed[service];
            let inspection = self.engine.inspect(container).await.map_err(|source| {
                ComposeLifecycleError::Docker {
                    operation: "inspect Compose service before stop",
                    source,
                }
            })?;
            verify_service(
                &inspection,
                container,
                service,
                recorded.project,
                recorded.identity,
                None,
            )?;
        }
        check_cancelled(cancellation, &[])?;
        self.compose
            .stop(stop, cancellation)
            .await
            .map_err(|source| ComposeLifecycleError::Compose {
                operation: "managed stop",
                source,
            })?;
        check_cancelled(cancellation, &[])?;
        let discovered = self.discover(recorded).await?;
        let (state, managed) = resolve_service_set(recorded, &discovered);
        if state != ComposeServiceSetState::CompleteStopped {
            return Err(ComposeLifecycleError::UnsafeServiceSet { state });
        }
        Ok(ComposeLifecycleFacts {
            primary: recorded.primary.clone(),
            managed,
        })
    }

    async fn verify_recorded_set(
        &self,
        recorded: &RecordedComposeRequest<'_>,
        managed: &BTreeMap<String, ContainerId>,
    ) -> Result<(), ComposeLifecycleError> {
        for service in recorded.managed_services {
            let container = &managed[service];
            let inspection = self.engine.inspect(container).await.map_err(|source| {
                ComposeLifecycleError::Docker {
                    operation: "inspect recorded Compose service",
                    source,
                }
            })?;
            verify_service(
                &inspection,
                container,
                service,
                recorded.project,
                recorded.identity,
                None,
            )?;
        }
        Ok(())
    }

    async fn discover(
        &self,
        request: &RecordedComposeRequest<'_>,
    ) -> Result<Vec<DiscoveredContainer>, ComposeLifecycleError> {
        self.engine
            .discover(request.identity)
            .await
            .map_err(|source| ComposeLifecycleError::Docker {
                operation: "discover managed Compose services",
                source,
            })
    }
}

/// Classifies persisted service names without selecting among duplicate matches.
#[must_use]
pub fn classify_compose_service_set(
    request: &RecordedComposeRequest<'_>,
    discovered: &[DiscoveredContainer],
) -> ComposeServiceSetState {
    resolve_service_set(request, discovered).0
}

fn resolve_service_set(
    request: &RecordedComposeRequest<'_>,
    discovered: &[DiscoveredContainer],
) -> (ComposeServiceSetState, BTreeMap<String, ContainerId>) {
    let mut grouped: BTreeMap<&str, Vec<&DiscoveredContainer>> = BTreeMap::new();
    for container in discovered.iter().filter(|container| {
        label(&container.labels, COMPOSE_PROJECT_LABEL) == Some(request.project)
    }) {
        if let Some(service) = label(&container.labels, COMPOSE_SERVICE_LABEL) {
            grouped.entry(service).or_default().push(container);
        }
    }
    let mut missing = Vec::new();
    let mut ambiguous = Vec::new();
    let mut running = Vec::new();
    let mut stopped = Vec::new();
    let mut managed = BTreeMap::new();
    for service in request.managed_services {
        match grouped.get(service.as_str()).map(Vec::as_slice) {
            None | Some([]) => missing.push(service.clone()),
            Some([container]) => {
                managed.insert(service.clone(), container.id.clone());
                if container.is_running() {
                    running.push(service.clone());
                } else {
                    stopped.push(service.clone());
                }
            }
            Some(_) => ambiguous.push(service.clone()),
        }
    }
    let state = if !ambiguous.is_empty() {
        ComposeServiceSetState::Ambiguous {
            services: ambiguous,
        }
    } else if !missing.is_empty() {
        ComposeServiceSetState::Missing {
            missing,
            running,
            stopped,
        }
    } else if managed
        .get(request.primary_service)
        .is_some_and(|id| id != request.primary)
    {
        ComposeServiceSetState::PrimarySubstituted {
            actual: managed[request.primary_service].clone(),
        }
    } else if stopped.is_empty() {
        ComposeServiceSetState::CompleteRunning
    } else if running.is_empty() {
        ComposeServiceSetState::CompleteStopped
    } else {
        ComposeServiceSetState::PartiallyRunning { running, stopped }
    };
    (state, managed)
}

fn is_completely_missing(state: &ComposeServiceSetState, expected: usize) -> bool {
    matches!(
        state,
        ComposeServiceSetState::Missing {
            missing,
            running,
            stopped,
        } if missing.len() == expected && running.is_empty() && stopped.is_empty()
    )
}

fn validate_recorded(request: &RecordedComposeRequest<'_>) -> Result<(), ComposeLifecycleError> {
    let unique = request.managed_services.iter().collect::<BTreeSet<_>>();
    if request.managed_services.is_empty()
        || unique.len() != request.managed_services.len()
        || !request
            .managed_services
            .iter()
            .any(|service| service == request.primary_service)
        || request.managed_services.iter().any(|service| {
            service.is_empty() || service.starts_with('-') || service.chars().any(char::is_control)
        })
    {
        return Err(ComposeLifecycleError::InvalidManagedSet);
    }
    Ok(())
}

fn verify_service(
    inspection: &ContainerInspection,
    id: &ContainerId,
    service: &str,
    project: &str,
    identity: DockerResourceIdentity<'_>,
    running: impl Into<Option<bool>>,
) -> Result<(), ComposeLifecycleError> {
    let checks = [
        ("container ID", inspection.id.as_str(), id.as_str()),
        (
            "installation label",
            label(&inspection.labels, INSTALLATION_LABEL).unwrap_or(""),
            identity.installation.as_str(),
        ),
        (
            "workspace label",
            label(&inspection.labels, WORKSPACE_LABEL).unwrap_or(""),
            identity.workspace.as_str(),
        ),
        (
            "generation label",
            label(&inspection.labels, GENERATION_LABEL).unwrap_or(""),
            &identity.generation.to_string(),
        ),
        (
            "profile label",
            label(&inspection.labels, PROFILE_LABEL).unwrap_or(""),
            identity.profile.as_str(),
        ),
        (
            "Compose project label",
            label(&inspection.labels, COMPOSE_PROJECT_LABEL).unwrap_or(""),
            project,
        ),
        (
            "Compose service label",
            label(&inspection.labels, COMPOSE_SERVICE_LABEL).unwrap_or(""),
            service,
        ),
    ];
    for (field, actual, expected) in checks {
        if actual != expected {
            return Err(ComposeLifecycleError::ServiceMismatch {
                service: service.to_owned(),
                field,
            });
        }
    }
    if running
        .into()
        .is_some_and(|expected| inspection.running != expected)
    {
        return Err(ComposeLifecycleError::ServiceMismatch {
            service: service.to_owned(),
            field: "running state",
        });
    }
    Ok(())
}

fn label<'a>(labels: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    labels.get(name).map(String::as_str)
}

fn check_cancelled(
    cancellation: &CancellationToken,
    completed: &[String],
) -> Result<(), ComposeLifecycleError> {
    if cancellation.is_cancelled() {
        Err(ComposeLifecycleError::Cancelled {
            completed: completed.to_vec(),
        })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use cdenv_core::{GenerationId, InstallationId, ProfileId, WorkspaceName};

    use super::*;

    const APP_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DB_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn identity<'a>(
        installation: &'a InstallationId,
        workspace: &'a WorkspaceName,
        profile: &'a ProfileId,
    ) -> DockerResourceIdentity<'a> {
        DockerResourceIdentity {
            installation,
            workspace,
            generation: GenerationId::new(1).expect("generation"),
            profile,
        }
    }

    fn service(id: &str, name: &str, state: &str) -> DiscoveredContainer {
        DiscoveredContainer {
            id: ContainerId::parse(id).expect("container ID"),
            names: vec![format!("project-{name}-1")],
            image_id: Some(
                ImageId::parse(
                    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
                )
                .expect("image ID"),
            ),
            labels: BTreeMap::from([
                (COMPOSE_PROJECT_LABEL.to_owned(), "project".to_owned()),
                (COMPOSE_SERVICE_LABEL.to_owned(), name.to_owned()),
                (INSTALLATION_LABEL.to_owned(), "installation".to_owned()),
                (WORKSPACE_LABEL.to_owned(), "workspace".to_owned()),
                (GENERATION_LABEL.to_owned(), "1".to_owned()),
                (PROFILE_LABEL.to_owned(), "profile".to_owned()),
            ]),
            state: Some(state.to_owned()),
        }
    }

    #[test]
    fn classification_reports_partial_running_set_exactly() {
        let installation = InstallationId::parse("installation").expect("installation");
        let workspace = WorkspaceName::parse("workspace").expect("workspace");
        let profile = ProfileId::parse("profile").expect("profile");
        let primary = ContainerId::parse(APP_ID).expect("primary");
        let managed = vec!["app".to_owned(), "db".to_owned()];
        let request = RecordedComposeRequest {
            identity: identity(&installation, &workspace, &profile),
            project: "project",
            primary_service: "app",
            primary: &primary,
            managed_services: &managed,
        };

        let state = classify_compose_service_set(
            &request,
            &[
                service(APP_ID, "app", "running"),
                service(DB_ID, "db", "exited"),
            ],
        );

        assert_eq!(
            state,
            ComposeServiceSetState::PartiallyRunning {
                running: vec!["app".to_owned()],
                stopped: vec!["db".to_owned()],
            }
        );
    }

    #[test]
    fn classification_reports_missing_service_without_selecting_a_replacement() {
        let installation = InstallationId::parse("installation").expect("installation");
        let workspace = WorkspaceName::parse("workspace").expect("workspace");
        let profile = ProfileId::parse("profile").expect("profile");
        let primary = ContainerId::parse(APP_ID).expect("primary");
        let managed = vec!["app".to_owned(), "db".to_owned()];
        let request = RecordedComposeRequest {
            identity: identity(&installation, &workspace, &profile),
            project: "project",
            primary_service: "app",
            primary: &primary,
            managed_services: &managed,
        };

        let state = classify_compose_service_set(&request, &[service(APP_ID, "app", "running")]);

        assert_eq!(
            state,
            ComposeServiceSetState::Missing {
                missing: vec!["db".to_owned()],
                running: vec!["app".to_owned()],
                stopped: Vec::new(),
            }
        );
    }

    #[derive(Clone)]
    struct NoCompose;

    impl ComposeLifecycleCli for NoCompose {
        async fn up(
            &self,
            _request: &ComposeUpRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<ComposeUpClaim, ComposeAdapterError> {
            panic!("ordinary resume must not invoke Compose up")
        }

        async fn stop(
            &self,
            _request: &ComposeStopRequest<'_>,
            _cancellation: &CancellationToken,
        ) -> Result<(), ComposeAdapterError> {
            panic!("ordinary resume must not invoke Compose stop")
        }
    }

    #[derive(Clone)]
    struct MemoryEngine(Arc<Mutex<Vec<DiscoveredContainer>>>);

    impl ComposeLifecycleEngine for MemoryEngine {
        async fn discover(
            &self,
            _identity: DockerResourceIdentity<'_>,
        ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
            Ok(self.0.lock().expect("engine state").clone())
        }

        async fn inspect(
            &self,
            id: &ContainerId,
        ) -> Result<ContainerInspection, BollardAdapterError> {
            let container = self
                .0
                .lock()
                .expect("engine state")
                .iter()
                .find(|container| &container.id == id)
                .cloned()
                .expect("recorded container");
            let running = container.is_running();
            Ok(ContainerInspection {
                id: container.id,
                name: container.names[0].clone(),
                image_id: container.image_id.expect("image ID"),
                image_reference: None,
                labels: container.labels,
                user: String::new(),
                working_directory: String::new(),
                mounts: Vec::new(),
                ports: Vec::new(),
                running,
            })
        }

        async fn start(&self, id: &ContainerId) -> Result<(), BollardAdapterError> {
            let mut containers = self.0.lock().expect("engine state");
            let container = containers
                .iter_mut()
                .find(|container| &container.id == id)
                .expect("recorded container");
            container.state = Some("running".to_owned());
            Ok(())
        }

        async fn verify_primary(
            &self,
            _expected: ComposePrimaryExpectation<'_>,
        ) -> Result<ContainerInspection, BollardAdapterError> {
            panic!("resume verifies recorded members instead of a reconciliation claim")
        }
    }

    #[tokio::test]
    async fn completely_missing_down_is_idempotent_but_duplicate_service_is_unsafe() {
        let installation = InstallationId::parse("installation").expect("installation");
        let workspace = WorkspaceName::parse("workspace").expect("workspace");
        let profile = ProfileId::parse("profile").expect("profile");
        let primary = ContainerId::parse(APP_ID).expect("primary");
        let managed = vec!["app".to_owned(), "db".to_owned()];
        let recorded = RecordedComposeRequest {
            identity: identity(&installation, &workspace, &profile),
            project: "project",
            primary_service: "app",
            primary: &primary,
            managed_services: &managed,
        };
        let files = Vec::new();
        let stop = ComposeStopRequest {
            project: crate::ComposeProject {
                files: &files,
                project_name: "project",
                working_directory: std::path::Path::new("/tmp"),
            },
            managed_services: &managed,
        };
        let engine = MemoryEngine(Arc::new(Mutex::new(Vec::new())));
        let orchestrator = ComposeLifecycleOrchestrator::new(NoCompose, engine.clone());

        let missing = orchestrator
            .stop_recorded(&recorded, &stop, &CancellationToken::default())
            .await
            .expect("known missing set");
        assert_eq!(missing, ComposeStopOutcome::Missing);

        *engine.0.lock().expect("engine state") = vec![
            service(APP_ID, "app", "running"),
            service(
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "app",
                "running",
            ),
            service(DB_ID, "db", "running"),
        ];
        let error = orchestrator
            .stop_recorded(&recorded, &stop, &CancellationToken::default())
            .await
            .expect_err("duplicate service must fail");
        assert!(matches!(
            error,
            ComposeLifecycleError::UnsafeServiceSet {
                state: ComposeServiceSetState::Ambiguous { .. }
            }
        ));
    }

    #[tokio::test]
    async fn ordinary_resume_directly_starts_recorded_set_without_compose_reconciliation() {
        let installation = InstallationId::parse("installation").expect("installation");
        let workspace = WorkspaceName::parse("workspace").expect("workspace");
        let profile = ProfileId::parse("profile").expect("profile");
        let primary = ContainerId::parse(APP_ID).expect("primary");
        let managed = vec!["app".to_owned(), "db".to_owned()];
        let request = RecordedComposeRequest {
            identity: identity(&installation, &workspace, &profile),
            project: "project",
            primary_service: "app",
            primary: &primary,
            managed_services: &managed,
        };
        let engine = MemoryEngine(Arc::new(Mutex::new(vec![
            service(APP_ID, "app", "exited"),
            service(DB_ID, "db", "exited"),
            service(
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "manual",
                "running",
            ),
        ])));
        let orchestrator = ComposeLifecycleOrchestrator::new(NoCompose, engine.clone());

        let facts = orchestrator
            .resume(&request, &CancellationToken::default())
            .await
            .expect("direct resume");

        assert_eq!(facts.managed.keys().cloned().collect::<Vec<_>>(), managed);
        let manual = &engine.0.lock().expect("engine state")[2];
        assert!(manual.is_running());
    }
}
