//! Cancellable image/Dockerfile creation through Docker CLI claims and Bollard verification.

use std::future::Future;
use std::path::Path;

use cdenv_core::{ContainerArchitecture, ContainerId, GenerationId, InstallationId, WorkspaceName};
use cdenv_devcontainer::{
    BuildPlan, CreateOptionsPlan, HostCapabilities, HostRequirementError,
    HostRequirementEvaluation, HostRequirementWarning, HostRequirements, MountKind, PlannedMount,
    PortPlan, PublicationBinding, PublicationProtocol, RuntimePlan, evaluate_host_requirements,
};
use thiserror::Error;

use crate::bollard::BollardApi;
use crate::{
    BollardAdapter, BollardAdapterError, CancellationToken, ContainerDiscoveryScope,
    ContainerExpectation, ContainerInspection, CorrelatedContainers, DiscoveredContainer,
    DockerBuildContext, DockerBuildRequest, DockerCliAdapter, DockerCliError, DockerCreateRequest,
    DockerResourceIdentity, DockerfileInput, ImageCleanupExpectation, ImageId, ImageInspection,
    InspectedMount, InspectedPortBinding, WorkspaceCorrelation, correlate_containers,
};

/// Static Docker CLI seam used by image-scenario orchestration and component fakes.
#[doc(hidden)]
pub trait ImageDockerCli: Send + Sync {
    /// Pulls a base image and leaves inspection authoritative.
    fn pull<'a>(
        &'a self,
        image: &'a str,
        cwd: &'a Path,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<(), DockerCliError>> + Send + 'a;

    /// Builds and returns only the claimed content-addressed image ID.
    fn build<'a>(
        &'a self,
        request: &'a DockerBuildRequest<'a>,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<ImageId, DockerCliError>> + Send + 'a;

    /// Creates and returns only the claimed full container ID.
    fn create<'a>(
        &'a self,
        request: &'a DockerCreateRequest<'a>,
        cwd: &'a Path,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<ContainerId, DockerCliError>> + Send + 'a;
}

impl ImageDockerCli for DockerCliAdapter {
    async fn pull(
        &self,
        image: &str,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<(), DockerCliError> {
        self.pull(image, cwd, cancellation).await.map(|_| ())
    }

    async fn build(
        &self,
        request: &DockerBuildRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ImageId, DockerCliError> {
        self.build(request, cancellation)
            .await
            .map(|claim| claim.image_id)
    }

    async fn create(
        &self,
        request: &DockerCreateRequest<'_>,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<ContainerId, DockerCliError> {
        self.create(request, cwd, cancellation)
            .await
            .map(|claim| claim.container_id)
    }
}

/// Static Bollard seam used by image-scenario orchestration and component fakes.
#[doc(hidden)]
pub trait ImageDockerEngine: Send + Sync {
    /// Lists all installation/workspace matches in one request.
    fn discover<'a>(
        &'a self,
        installation: &'a InstallationId,
        workspace: &'a WorkspaceName,
    ) -> impl Future<Output = Result<Vec<DiscoveredContainer>, BollardAdapterError>> + Send + 'a;

    /// Inspects one exact container.
    fn inspect_container<'a>(
        &'a self,
        id: &'a ContainerId,
    ) -> impl Future<Output = Result<ContainerInspection, BollardAdapterError>> + Send + 'a;

    /// Inspects an image by ID or reference.
    fn inspect_image<'a>(
        &'a self,
        id: &'a str,
    ) -> impl Future<Output = Result<ImageInspection, BollardAdapterError>> + Send + 'a;

    /// Starts one exact container.
    fn start<'a>(
        &'a self,
        id: &'a ContainerId,
    ) -> impl Future<Output = Result<(), BollardAdapterError>> + Send + 'a;

    /// Stops one exact operation-owned candidate before cleanup.
    fn stop<'a>(
        &'a self,
        id: &'a ContainerId,
        grace: std::time::Duration,
    ) -> impl Future<Output = Result<(), BollardAdapterError>> + Send + 'a;

    /// Removes only a verified operation-owned container.
    fn cleanup_container<'a>(
        &'a self,
        expected: ContainerExpectation<'a>,
    ) -> impl Future<Output = Result<(), BollardAdapterError>> + Send + 'a;

    /// Removes only a verified operation-owned image.
    fn cleanup_image<'a>(
        &'a self,
        expected: ImageCleanupExpectation<'a>,
    ) -> impl Future<Output = Result<(), BollardAdapterError>> + Send + 'a;
}

impl<A: BollardApi> ImageDockerEngine for BollardAdapter<A> {
    async fn discover(
        &self,
        installation: &InstallationId,
        workspace: &WorkspaceName,
    ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
        self.discover(ContainerDiscoveryScope {
            installation,
            workspace: Some(workspace),
            generation: None,
        })
        .await
    }

    async fn inspect_container(
        &self,
        id: &ContainerId,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        self.inspect_container(id).await
    }

    async fn inspect_image(&self, id: &str) -> Result<ImageInspection, BollardAdapterError> {
        self.inspect_image(id).await
    }

    async fn start(&self, id: &ContainerId) -> Result<(), BollardAdapterError> {
        self.start(id).await
    }

    async fn stop(
        &self,
        id: &ContainerId,
        grace: std::time::Duration,
    ) -> Result<(), BollardAdapterError> {
        self.stop(id, grace).await
    }

    async fn cleanup_container(
        &self,
        expected: ContainerExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        self.cleanup_container(expected).await
    }

    async fn cleanup_image(
        &self,
        expected: ImageCleanupExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        self.cleanup_image(expected).await
    }
}

/// Borrowed inputs for one image or Dockerfile primary-container creation.
pub struct ImageContainerCreateRequest<'a> {
    /// Pure image or Dockerfile build plan.
    pub build: &'a BuildPlan,
    /// Canonical repository checkout.
    pub checkout: &'a Path,
    /// Required operation-owned tag for Dockerfile builds.
    pub build_tag: Option<&'a str>,
    /// Repository/generated build context selection.
    pub build_context: &'a DockerBuildContext,
    /// Repository/generated Dockerfile selection.
    pub dockerfile: DockerfileInput<'a>,
    /// Operation-owned generation-specific container name.
    pub container_name: &'a str,
    /// Stable Docker identity labels.
    pub identity: DockerResourceIdentity<'a>,
    /// Effective runtime/create settings.
    pub runtime: &'a RuntimePlan,
    /// Validated Docker create passthrough.
    pub create_options: &'a CreateOptionsPlan,
    /// Requested publications.
    pub ports: &'a PortPlan,
    /// Container command.
    pub command: &'a [String],
    /// Container paths reserved for later cdenv provisioning.
    pub cdenv_owned_targets: &'a [cdenv_devcontainer::ContainerPath],
    /// Merged host requirements.
    pub host_requirements: Option<&'a HostRequirements>,
    /// Injected reliable/unknown host capability evidence.
    pub host_capabilities: &'a HostCapabilities,
    /// Persisted prior active container, if any. It is never modified here.
    pub recorded_container: Option<&'a ContainerId>,
}

/// Borrowed inputs for direct restart of an unchanged recorded image container.
pub struct RecordedContainerRequest<'a> {
    /// Recorded container ID.
    pub container: &'a ContainerId,
    /// Exact cdenv-owned container name.
    pub container_name: &'a str,
    /// Expected image identity.
    pub image: &'a ImageId,
    /// Stable Docker identity labels.
    pub identity: DockerResourceIdentity<'a>,
    /// Effective active-generation runtime settings.
    pub runtime: &'a RuntimePlan,
    /// Active-generation publications.
    pub ports: &'a PortPlan,
}

/// Verified facts returned without mutating persisted active state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageContainerFacts {
    /// Exact verified container ID.
    pub container: ContainerId,
    /// Exact verified image ID.
    pub image: ImageId,
    /// Container image architecture used for later agent selection.
    pub architecture: ContainerArchitecture,
    /// Positive environment generation.
    pub generation: GenerationId,
    /// Whether the container was verified running.
    pub running: bool,
    /// Non-fatal host-requirement findings from pre-mutation evaluation.
    pub host_warnings: Vec<HostRequirementWarning>,
}

/// Live discovery classification that never chooses among unsafe matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageContainerMatchState {
    /// No current or stale labeled containers exist.
    Missing,
    /// Only prior/malformed generations exist.
    StaleOnly,
    /// The exact recorded current generation is stopped.
    RecordedStopped,
    /// The exact recorded current generation is already running.
    RecordedRunning,
    /// One current-generation container exists but does not match persisted identity.
    ExternalReplacement,
    /// More than one current-generation container exists.
    AmbiguousCurrent,
}

/// Classifies correlated matches without selecting an arbitrary container.
#[must_use]
pub fn classify_image_container_matches(
    matches: &CorrelatedContainers,
    recorded: Option<&ContainerId>,
) -> ImageContainerMatchState {
    match matches.current.as_slice() {
        [] if matches.stale.is_empty() => ImageContainerMatchState::Missing,
        [] => ImageContainerMatchState::StaleOnly,
        [_first, _second, ..] => ImageContainerMatchState::AmbiguousCurrent,
        [current] if recorded.is_some_and(|id| id == &current.id) && current.is_running() => {
            ImageContainerMatchState::RecordedRunning
        }
        [current] if recorded.is_some_and(|id| id == &current.id) => {
            ImageContainerMatchState::RecordedStopped
        }
        [_] => ImageContainerMatchState::ExternalReplacement,
    }
}

/// Idempotent result of stopping one exact recorded image container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageContainerStopOutcome {
    /// The verified running container transitioned to stopped.
    Stopped,
    /// The verified recorded container was already stopped.
    AlreadyStopped,
    /// No current-generation container exists.
    Missing,
}

/// Resource that could not be cleaned after a known operation failure.
#[derive(Debug)]
pub enum ImageCleanupFailure {
    /// Operation-owned candidate container cleanup failed safely.
    Container(BollardAdapterError),
    /// Operation-owned generated image cleanup failed safely.
    Image(BollardAdapterError),
}

/// Layered image-scenario orchestration failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ImageContainerError {
    /// A hard host requirement was reliably unmet before mutation.
    #[error(transparent)]
    HostRequirement(#[from] HostRequirementError),
    /// Compose is intentionally outside this orchestration boundary.
    #[error("image-scenario orchestration does not accept Compose plans")]
    ComposeUnsupported,
    /// A Dockerfile build omitted its required operation-owned tag.
    #[error("Dockerfile creation requires an operation-owned build tag")]
    MissingBuildTag,
    /// Cooperative cancellation was observed between adapter operations.
    #[error("image container creation was cancelled")]
    Cancelled,
    /// Existing live Docker truth makes mutation unsafe.
    #[error("cannot create or restart from Docker container state {state:?}")]
    UnsafeContainerState {
        /// Exact non-selecting discovery classification.
        state: ImageContainerMatchState,
    },
    /// Specification-facing Docker CLI operation failed.
    #[error("Docker CLI {operation} failed: {source}")]
    DockerCli {
        /// Safe operation name.
        operation: &'static str,
        /// Preserved adapter source.
        #[source]
        source: DockerCliError,
    },
    /// Bollard discovery, inspect, start, or verification failed.
    #[error("Docker {operation} failed: {source}")]
    Bollard {
        /// Safe operation name.
        operation: &'static str,
        /// Preserved adapter source.
        #[source]
        source: BollardAdapterError,
    },
    /// Image identity or ownership labels did not match a build claim.
    #[error("built image verification failed for {field}")]
    ImageMismatch {
        /// Safe mismatched field.
        field: &'static str,
    },
    /// Mount, user, workspace, or publication inspection did not match the create plan.
    #[error("container runtime verification failed for {field}")]
    RuntimeMismatch {
        /// Safe mismatched field.
        field: &'static str,
    },
    /// Primary failure retained alongside safe cleanup failures.
    #[error("{primary}; operation-owned cleanup also failed")]
    CleanupFailed {
        /// Original orchestration failure.
        primary: Box<ImageContainerError>,
        /// Cleanup failures; unrelated resources remain untouched.
        failures: Vec<ImageCleanupFailure>,
    },
}

/// Static-dispatch image/Dockerfile creation coordinator.
pub struct ImageContainerOrchestrator<D, E> {
    docker: D,
    engine: E,
}

impl<D, E> ImageContainerOrchestrator<D, E> {
    /// Constructs an orchestrator from explicit Docker CLI and Bollard seams.
    #[must_use]
    pub const fn new(docker: D, engine: E) -> Self {
        Self { docker, engine }
    }
}

#[derive(Default)]
struct OwnedResources {
    container: Option<ContainerId>,
    container_image: Option<ImageId>,
    generated_image: Option<ImageId>,
}

impl<D: ImageDockerCli, E: ImageDockerEngine> ImageContainerOrchestrator<D, E> {
    /// Creates, starts, and independently verifies one image/Dockerfile primary container.
    ///
    /// The method returns facts only. It does not write workspace state or mark a generation active.
    ///
    /// # Errors
    ///
    /// Returns host-evaluation, unsafe discovery, cancellation, adapter, verification, or cleanup
    /// failures. Known operation-owned candidates are cleaned on failure.
    pub async fn create(
        &self,
        request: &ImageContainerCreateRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ImageContainerFacts, ImageContainerError> {
        let host =
            evaluate_host_requirements(request.host_requirements, request.host_capabilities)?;
        check_cancellation(cancellation)?;
        let containers = self
            .engine
            .discover(request.identity.installation, request.identity.workspace)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "discovery",
                source,
            })?;
        let correlated = correlate_containers(
            &containers,
            &[WorkspaceCorrelation {
                workspace: request.identity.workspace,
                generation: request.identity.generation,
                recorded_container: request.recorded_container,
            }],
        );
        let state = classify_image_container_matches(&correlated[0], request.recorded_container);
        if !matches!(
            state,
            ImageContainerMatchState::Missing | ImageContainerMatchState::StaleOnly
        ) {
            return Err(ImageContainerError::UnsafeContainerState { state });
        }

        let mut owned = OwnedResources::default();
        match self
            .create_after_discovery(request, &host, cancellation, &mut owned)
            .await
        {
            Ok(facts) => Ok(facts),
            Err(primary) => {
                let failures = self.cleanup(request, &owned).await;
                if failures.is_empty() {
                    Err(primary)
                } else {
                    Err(ImageContainerError::CleanupFailed {
                        primary: Box::new(primary),
                        failures,
                    })
                }
            }
        }
    }

    /// Stops a uniquely resolved recorded image container without removing it.
    ///
    /// Missing and already-stopped resources are successful idempotent outcomes.
    /// Duplicate or externally replaced current-generation resources remain hard errors.
    ///
    /// # Errors
    ///
    /// Returns cancellation, unsafe discovery, inspect/verification, or stop errors.
    pub async fn stop_recorded(
        &self,
        request: &RecordedContainerRequest<'_>,
        grace: std::time::Duration,
        cancellation: &CancellationToken,
    ) -> Result<ImageContainerStopOutcome, ImageContainerError> {
        check_cancellation(cancellation)?;
        let containers = self
            .engine
            .discover(request.identity.installation, request.identity.workspace)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "discovery",
                source,
            })?;
        let correlated = correlate_containers(
            &containers,
            &[WorkspaceCorrelation {
                workspace: request.identity.workspace,
                generation: request.identity.generation,
                recorded_container: Some(request.container),
            }],
        );
        match classify_image_container_matches(&correlated[0], Some(request.container)) {
            ImageContainerMatchState::Missing | ImageContainerMatchState::StaleOnly => {
                Ok(ImageContainerStopOutcome::Missing)
            }
            ImageContainerMatchState::RecordedStopped => {
                self.inspect_and_verify(
                    request.container,
                    request.container_name,
                    request.image,
                    request.identity,
                    request.runtime,
                    request.ports,
                    false,
                )
                .await?;
                Ok(ImageContainerStopOutcome::AlreadyStopped)
            }
            ImageContainerMatchState::RecordedRunning => {
                self.inspect_and_verify(
                    request.container,
                    request.container_name,
                    request.image,
                    request.identity,
                    request.runtime,
                    request.ports,
                    true,
                )
                .await?;
                check_cancellation(cancellation)?;
                self.engine
                    .stop(request.container, grace)
                    .await
                    .map_err(|source| ImageContainerError::Bollard {
                        operation: "stop recorded container",
                        source,
                    })?;
                self.inspect_and_verify(
                    request.container,
                    request.container_name,
                    request.image,
                    request.identity,
                    request.runtime,
                    request.ports,
                    false,
                )
                .await?;
                Ok(ImageContainerStopOutcome::Stopped)
            }
            state => Err(ImageContainerError::UnsafeContainerState { state }),
        }
    }

    /// Starts an unchanged, uniquely resolved, recorded stopped image container directly.
    ///
    /// # Errors
    ///
    /// Returns unsafe discovery, inspect/verification, unsupported architecture, or start errors.
    pub async fn restart_recorded(
        &self,
        request: &RecordedContainerRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ImageContainerFacts, ImageContainerError> {
        check_cancellation(cancellation)?;
        let containers = self
            .engine
            .discover(request.identity.installation, request.identity.workspace)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "discovery",
                source,
            })?;
        let correlated = correlate_containers(
            &containers,
            &[WorkspaceCorrelation {
                workspace: request.identity.workspace,
                generation: request.identity.generation,
                recorded_container: Some(request.container),
            }],
        );
        let state = classify_image_container_matches(&correlated[0], Some(request.container));
        if state != ImageContainerMatchState::RecordedStopped {
            return Err(ImageContainerError::UnsafeContainerState { state });
        }
        let stopped = self
            .inspect_and_verify(
                request.container,
                request.container_name,
                request.image,
                request.identity,
                request.runtime,
                request.ports,
                false,
            )
            .await?;
        check_cancellation(cancellation)?;
        self.engine
            .start(request.container)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "start recorded container",
                source,
            })?;
        let running = self
            .inspect_and_verify(
                request.container,
                request.container_name,
                request.image,
                request.identity,
                request.runtime,
                request.ports,
                true,
            )
            .await?;
        let image = self
            .engine
            .inspect_image(running.image_id.as_str())
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "inspect recorded image",
                source,
            })?;
        if image.id != running.image_id {
            return Err(ImageContainerError::ImageMismatch { field: "image ID" });
        }
        debug_assert_eq!(stopped.id, running.id);
        Ok(ImageContainerFacts {
            container: running.id,
            image: image.id,
            architecture: image.architecture,
            generation: request.identity.generation,
            running: true,
            host_warnings: Vec::new(),
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "the cancellable mutation sequence remains auditable in execution order"
    )]
    async fn create_after_discovery(
        &self,
        request: &ImageContainerCreateRequest<'_>,
        host: &HostRequirementEvaluation,
        cancellation: &CancellationToken,
        owned: &mut OwnedResources,
    ) -> Result<ImageContainerFacts, ImageContainerError> {
        let image = match request.build {
            BuildPlan::Image { image } => {
                self.docker
                    .pull(image, request.checkout, cancellation)
                    .await
                    .map_err(|source| ImageContainerError::DockerCli {
                        operation: "pull",
                        source,
                    })?;
                check_cancellation(cancellation)?;
                self.engine.inspect_image(image).await.map_err(|source| {
                    ImageContainerError::Bollard {
                        operation: "inspect pulled image",
                        source,
                    }
                })?
            }
            BuildPlan::Dockerfile(plan) => {
                let tag = request
                    .build_tag
                    .ok_or(ImageContainerError::MissingBuildTag)?;
                let claim = self
                    .docker
                    .build(
                        &DockerBuildRequest {
                            plan,
                            checkout: request.checkout,
                            tag,
                            identity: request.identity,
                            context: request.build_context,
                            dockerfile: request.dockerfile,
                        },
                        cancellation,
                    )
                    .await
                    .map_err(|source| ImageContainerError::DockerCli {
                        operation: "build",
                        source,
                    })?;
                owned.generated_image = Some(claim.clone());
                check_cancellation(cancellation)?;
                let image = self
                    .engine
                    .inspect_image(claim.as_str())
                    .await
                    .map_err(|source| ImageContainerError::Bollard {
                        operation: "inspect built image",
                        source,
                    })?;
                verify_built_image(&image, &claim, request.identity)?;
                image
            }
            BuildPlan::Compose => return Err(ImageContainerError::ComposeUnsupported),
        };
        owned.container_image = Some(image.id.clone());
        check_cancellation(cancellation)?;
        let container = self
            .docker
            .create(
                &DockerCreateRequest {
                    name: request.container_name,
                    image: image.id.as_str(),
                    identity: request.identity,
                    runtime: request.runtime,
                    options: request.create_options,
                    ports: request.ports,
                    gpu_access: host.gpu_access,
                    command: request.command,
                    cdenv_owned_targets: request.cdenv_owned_targets,
                },
                request.checkout,
                cancellation,
            )
            .await
            .map_err(|source| ImageContainerError::DockerCli {
                operation: "create",
                source,
            })?;
        owned.container = Some(container.clone());
        check_cancellation(cancellation)?;
        let created = self
            .inspect_and_verify(
                &container,
                request.container_name,
                &image.id,
                request.identity,
                request.runtime,
                request.ports,
                false,
            )
            .await?;
        let container_image = self
            .engine
            .inspect_image(created.image_id.as_str())
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "inspect container image",
                source,
            })?;
        if container_image.id != created.image_id {
            return Err(ImageContainerError::ImageMismatch { field: "image ID" });
        }
        let architecture = container_image.architecture;
        check_cancellation(cancellation)?;
        self.engine
            .start(&container)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "start candidate",
                source,
            })?;
        check_cancellation(cancellation)?;
        let running = self
            .inspect_and_verify(
                &container,
                request.container_name,
                &image.id,
                request.identity,
                request.runtime,
                request.ports,
                true,
            )
            .await?;
        Ok(ImageContainerFacts {
            container: running.id,
            image: image.id,
            architecture,
            generation: request.identity.generation,
            running: true,
            host_warnings: host.warnings.clone(),
        })
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the verification boundary keeps each independently claimed value explicit"
    )]
    async fn inspect_and_verify(
        &self,
        container: &ContainerId,
        name: &str,
        image: &ImageId,
        identity: DockerResourceIdentity<'_>,
        runtime: &RuntimePlan,
        ports: &PortPlan,
        running: bool,
    ) -> Result<ContainerInspection, ImageContainerError> {
        let inspection = self
            .engine
            .inspect_container(container)
            .await
            .map_err(|source| ImageContainerError::Bollard {
                operation: "inspect container",
                source,
            })?;
        BollardAdapter::<crate::bollard::BollardClientApi>::verify_container(
            &inspection,
            ContainerExpectation {
                id: container,
                name,
                image_id: image,
                installation: identity.installation,
                workspace: identity.workspace,
                generation: identity.generation,
                profile: identity.profile,
                project: None,
                service: None,
                running: Some(running),
            },
        )
        .map_err(|source| ImageContainerError::Bollard {
            operation: "verify container identity",
            source,
        })?;
        verify_runtime(&inspection, runtime, ports)?;
        Ok(inspection)
    }

    async fn cleanup(
        &self,
        request: &ImageContainerCreateRequest<'_>,
        owned: &OwnedResources,
    ) -> Vec<ImageCleanupFailure> {
        let mut failures = Vec::new();
        if let (Some(container), Some(image)) = (&owned.container, &owned.container_image) {
            let expected = ContainerExpectation {
                id: container,
                name: request.container_name,
                image_id: image,
                installation: request.identity.installation,
                workspace: request.identity.workspace,
                generation: request.identity.generation,
                profile: request.identity.profile,
                project: None,
                service: None,
                running: None,
            };
            match self.engine.inspect_container(container).await {
                Ok(inspection) => {
                    if let Err(error) =
                        BollardAdapter::<crate::bollard::BollardClientApi>::verify_container(
                            &inspection,
                            expected,
                        )
                    {
                        failures.push(ImageCleanupFailure::Container(error));
                    } else {
                        if inspection.running
                            && let Err(error) = self
                                .engine
                                .stop(container, std::time::Duration::from_secs(5))
                                .await
                        {
                            failures.push(ImageCleanupFailure::Container(error));
                        }
                        if let Err(error) = self.engine.cleanup_container(expected).await {
                            failures.push(ImageCleanupFailure::Container(error));
                        }
                    }
                }
                Err(error) => failures.push(ImageCleanupFailure::Container(error)),
            }
        }
        if let Some(image) = &owned.generated_image
            && let Err(error) = self
                .engine
                .cleanup_image(ImageCleanupExpectation {
                    id: image,
                    installation: request.identity.installation,
                    workspace: request.identity.workspace,
                    generation: request.identity.generation,
                    profile: request.identity.profile,
                })
                .await
        {
            failures.push(ImageCleanupFailure::Image(error));
        }
        failures
    }
}

fn check_cancellation(cancellation: &CancellationToken) -> Result<(), ImageContainerError> {
    if cancellation.is_cancelled() {
        Err(ImageContainerError::Cancelled)
    } else {
        Ok(())
    }
}

fn verify_built_image(
    image: &ImageInspection,
    claim: &ImageId,
    identity: DockerResourceIdentity<'_>,
) -> Result<(), ImageContainerError> {
    if &image.id != claim {
        return Err(ImageContainerError::ImageMismatch { field: "image ID" });
    }
    for (key, expected) in [
        ("cdenv.installation", identity.installation.to_string()),
        ("cdenv.workspace", identity.workspace.to_string()),
        ("cdenv.generation", identity.generation.to_string()),
        ("cdenv.profile", identity.profile.to_string()),
        ("cdenv.generated", "true".to_owned()),
    ] {
        if image.labels.get(key) != Some(&expected) {
            return Err(ImageContainerError::ImageMismatch { field: key });
        }
    }
    Ok(())
}

fn verify_runtime(
    inspection: &ContainerInspection,
    runtime: &RuntimePlan,
    ports: &PortPlan,
) -> Result<(), ImageContainerError> {
    if inspection.user != runtime.container_user.as_str() {
        return Err(ImageContainerError::RuntimeMismatch {
            field: "container user",
        });
    }
    if inspection.working_directory != runtime.workspace.folder.as_str() {
        return Err(ImageContainerError::RuntimeMismatch {
            field: "workspace folder",
        });
    }
    let expected_mounts = std::iter::once(&runtime.workspace.mount)
        .chain(&runtime.mounts)
        .collect::<Vec<_>>();
    if inspection.mounts.len() != expected_mounts.len()
        || expected_mounts.iter().any(|expected| {
            !inspection
                .mounts
                .iter()
                .any(|actual| mount_matches(actual, expected))
        })
    {
        return Err(ImageContainerError::RuntimeMismatch { field: "mounts" });
    }
    if !ports_match(&inspection.ports, ports) {
        return Err(ImageContainerError::RuntimeMismatch { field: "ports" });
    }
    Ok(())
}

fn mount_matches(actual: &InspectedMount, expected: &PlannedMount) -> bool {
    let kind = match expected.kind {
        MountKind::Bind => "bind",
        MountKind::Volume => "volume",
    };
    actual.kind == kind
        && actual.source == expected.source
        && actual.target == expected.target.as_str()
        && expected.options.iter().all(|option| {
            let expected = option.value.as_ref().map_or_else(
                || option.name.clone(),
                |value| format!("{}={value}", option.name),
            );
            actual
                .mode
                .as_deref()
                .unwrap_or_default()
                .split(',')
                .any(|value| value == expected)
        })
}

fn ports_match(actual: &[InspectedPortBinding], expected: &PortPlan) -> bool {
    let expected_count: usize = expected
        .publications
        .iter()
        .map(|publication| {
            usize::from(
                publication.container_ports.end.get() - publication.container_ports.start.get(),
            ) + 1
        })
        .sum();
    if actual.len() != expected_count {
        return false;
    }
    expected.publications.iter().all(|publication| {
        let protocol = match publication.protocol {
            PublicationProtocol::Tcp => "tcp",
            PublicationProtocol::Udp => "udp",
            PublicationProtocol::Sctp => "sctp",
        };
        (publication.container_ports.start.get()..=publication.container_ports.end.get()).all(
            |port| {
                let key = format!("{port}/{protocol}");
                let host_port = publication.host_ports.map(|range| {
                    range.start.get() + (port - publication.container_ports.start.get())
                });
                actual.iter().any(|binding| {
                    binding.container == key
                        && host_ip_matches(binding.host_ip.as_deref(), publication.binding)
                        && host_port.map_or_else(
                            || {
                                binding.host_port.as_deref().is_some_and(|value| {
                                    value.parse::<u16>().is_ok_and(|value| value > 0)
                                })
                            },
                            |port| binding.host_port.as_deref() == Some(&port.to_string()),
                        )
                })
            },
        )
    })
}

fn host_ip_matches(actual: Option<&str>, expected: PublicationBinding) -> bool {
    match expected {
        PublicationBinding::Loopback(address) | PublicationBinding::NonLoopback(address) => {
            actual == Some(address.to_string().as_str())
        }
        PublicationBinding::AllInterfaces => {
            actual.is_none_or(|value| matches!(value, "" | "0.0.0.0" | "::"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    use cdenv_core::ProfileId;
    use cdenv_devcontainer::{
        ConfigPath, DockerOptionPlanningInputs, HostSubstitutionInputs, Measured, ParseLimits,
        RawProfile, RuntimePlanningInputs, ScenarioMetadata, StableIdentityLabels,
        UnknownMeasurement, merge_image_metadata, parse_jsonc, plan_docker_options, plan_ports,
        plan_runtime, validate_profile,
    };

    use super::*;

    const CONTAINER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const OTHER_CONTAINER_ID: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const IMAGE_ID: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[derive(Clone, Default)]
    struct FakeCli {
        state: Arc<Mutex<CliState>>,
    }

    #[derive(Default)]
    struct CliState {
        calls: Vec<&'static str>,
        fail: Option<&'static str>,
        cancel_on_build: bool,
        gpu_access: Option<cdenv_devcontainer::GpuAccessIntent>,
    }

    impl FakeCli {
        fn failing(stage: &'static str) -> Self {
            Self {
                state: Arc::new(Mutex::new(CliState {
                    fail: Some(stage),
                    ..CliState::default()
                })),
            }
        }

        fn cancelling_build() -> Self {
            Self {
                state: Arc::new(Mutex::new(CliState {
                    cancel_on_build: true,
                    ..CliState::default()
                })),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state.lock().expect("CLI lock").calls.clone()
        }

        fn gpu_access(&self) -> Option<cdenv_devcontainer::GpuAccessIntent> {
            self.state.lock().expect("CLI lock").gpu_access
        }
    }

    impl ImageDockerCli for FakeCli {
        async fn pull(
            &self,
            _image: &str,
            _cwd: &Path,
            _cancellation: &CancellationToken,
        ) -> Result<(), DockerCliError> {
            let mut state = self.state.lock().expect("CLI lock");
            state.calls.push("pull");
            if state.fail == Some("pull") {
                Err(DockerCliError::InvalidImageId)
            } else {
                Ok(())
            }
        }

        async fn build(
            &self,
            _request: &DockerBuildRequest<'_>,
            cancellation: &CancellationToken,
        ) -> Result<ImageId, DockerCliError> {
            let mut state = self.state.lock().expect("CLI lock");
            state.calls.push("build");
            if state.cancel_on_build {
                cancellation.cancel();
            }
            if state.fail == Some("build") {
                Err(DockerCliError::InvalidImageId)
            } else {
                ImageId::parse(IMAGE_ID)
            }
        }

        async fn create(
            &self,
            request: &DockerCreateRequest<'_>,
            _cwd: &Path,
            _cancellation: &CancellationToken,
        ) -> Result<ContainerId, DockerCliError> {
            let mut state = self.state.lock().expect("CLI lock");
            state.calls.push("create");
            state.gpu_access = Some(request.gpu_access);
            if state.fail == Some("create") {
                Err(DockerCliError::InvalidContainerId)
            } else {
                ContainerId::parse(CONTAINER_ID).map_err(DockerCliError::ContainerId)
            }
        }
    }

    #[derive(Clone, Default)]
    struct FakeEngine {
        state: Arc<Mutex<EngineState>>,
    }

    #[derive(Default)]
    struct EngineState {
        discovered: Vec<DiscoveredContainer>,
        containers: VecDeque<Result<ContainerInspection, BollardAdapterError>>,
        images: VecDeque<Result<ImageInspection, BollardAdapterError>>,
        calls: Vec<&'static str>,
        fail_start: bool,
        fail_cleanup: bool,
        cleaned_containers: Vec<ContainerId>,
        cleaned_images: Vec<ImageId>,
    }

    impl FakeEngine {
        fn with_discovery(self, containers: Vec<DiscoveredContainer>) -> Self {
            self.state.lock().expect("engine lock").discovered = containers;
            self
        }

        fn with_containers(
            self,
            containers: impl IntoIterator<Item = ContainerInspection>,
        ) -> Self {
            self.state.lock().expect("engine lock").containers =
                containers.into_iter().map(Ok).collect();
            self
        }

        fn with_images(self, images: impl IntoIterator<Item = ImageInspection>) -> Self {
            self.state.lock().expect("engine lock").images = images.into_iter().map(Ok).collect();
            self
        }

        fn failing_start(self) -> Self {
            self.state.lock().expect("engine lock").fail_start = true;
            self
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state.lock().expect("engine lock").calls.clone()
        }

        fn cleanup_counts(&self) -> (usize, usize) {
            let state = self.state.lock().expect("engine lock");
            (state.cleaned_containers.len(), state.cleaned_images.len())
        }
    }

    impl ImageDockerEngine for FakeEngine {
        async fn discover(
            &self,
            _installation: &InstallationId,
            _workspace: &WorkspaceName,
        ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("discover");
            Ok(state.discovered.clone())
        }

        async fn inspect_container(
            &self,
            _id: &ContainerId,
        ) -> Result<ContainerInspection, BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("inspect-container");
            state.containers.pop_front().expect("container inspection")
        }

        async fn inspect_image(&self, _id: &str) -> Result<ImageInspection, BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("inspect-image");
            state.images.pop_front().expect("image inspection")
        }

        async fn start(&self, _id: &ContainerId) -> Result<(), BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("start");
            if state.fail_start {
                Err(BollardAdapterError::UnexpectedResponse {
                    operation: "fake start",
                })
            } else {
                Ok(())
            }
        }

        async fn stop(
            &self,
            _id: &ContainerId,
            _grace: std::time::Duration,
        ) -> Result<(), BollardAdapterError> {
            self.state.lock().expect("engine lock").calls.push("stop");
            Ok(())
        }

        async fn cleanup_container(
            &self,
            expected: ContainerExpectation<'_>,
        ) -> Result<(), BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("cleanup-container");
            state.cleaned_containers.push(expected.id.clone());
            if state.fail_cleanup {
                Err(BollardAdapterError::UnexpectedResponse {
                    operation: "fake cleanup",
                })
            } else {
                Ok(())
            }
        }

        async fn cleanup_image(
            &self,
            expected: ImageCleanupExpectation<'_>,
        ) -> Result<(), BollardAdapterError> {
            let mut state = self.state.lock().expect("engine lock");
            state.calls.push("cleanup-image");
            state.cleaned_images.push(expected.id.clone());
            if state.fail_cleanup {
                Err(BollardAdapterError::UnexpectedResponse {
                    operation: "fake cleanup",
                })
            } else {
                Ok(())
            }
        }
    }

    struct Plans {
        docker: cdenv_devcontainer::DockerOptionsPlan,
        runtime: RuntimePlan,
        ports: PortPlan,
        requirements: Option<HostRequirements>,
    }

    fn profile(source: &str) -> RawProfile {
        let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("config path");
        let document =
            parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSONC");
        validate_profile(&document).expect("profile")
    }

    fn plans(source: &str) -> Plans {
        let profile = profile(source);
        let effective = merge_image_metadata(&[], &profile).expect("metadata");
        let checkout = "/workspaces/source";
        let local_env = BTreeMap::new();
        let labels = StableIdentityLabels::new("installation", "workspace");
        let owned = [cdenv_devcontainer::ContainerPath::parse("/opt/cdenv").expect("owned")];
        let runtime = plan_runtime(
            &profile,
            &effective,
            &RuntimePlanningInputs {
                local_workspace_folder: checkout,
                local_env: &local_env,
                identity_labels: &labels,
                scenario_metadata: ScenarioMetadata::default(),
                cdenv_owned_targets: &owned,
                host_user: None,
            },
        )
        .expect("runtime");
        let substitutions = HostSubstitutionInputs {
            local_workspace_folder: checkout,
            container_workspace_folder: runtime.workspace.folder.as_str(),
            local_env: &local_env,
            identity_labels: &labels,
        };
        let docker = plan_docker_options(
            &profile,
            &runtime,
            &DockerOptionPlanningInputs {
                config_directory: ".devcontainer",
                substitutions: &substitutions,
                cdenv_owned_targets: &owned,
            },
        )
        .expect("Docker plan");
        let ports = plan_ports(&profile, &effective).expect("ports");
        Plans {
            docker,
            runtime,
            ports,
            requirements: effective.host_requirements,
        }
    }

    fn capabilities() -> HostCapabilities {
        HostCapabilities {
            cpus: Measured::Reliable(8),
            memory_bytes: Measured::Reliable(16 << 30),
            storage_bytes: Measured::Reliable(100 << 30),
            gpu: Measured::Unknown(UnknownMeasurement::Unmeasurable),
        }
    }

    fn identities() -> (InstallationId, WorkspaceName, ProfileId, GenerationId) {
        (
            InstallationId::parse("installation").expect("installation"),
            WorkspaceName::parse("workspace").expect("workspace"),
            ProfileId::parse("cdenv-devcontainer-v1").expect("profile"),
            GenerationId::new(2).expect("generation"),
        )
    }

    fn identity<'a>(
        installation: &'a InstallationId,
        workspace: &'a WorkspaceName,
        profile: &'a ProfileId,
        generation: GenerationId,
    ) -> DockerResourceIdentity<'a> {
        DockerResourceIdentity {
            installation,
            workspace,
            generation,
            profile,
        }
    }

    fn image(labels: bool) -> ImageInspection {
        let labels = if labels {
            BTreeMap::from([
                ("cdenv.installation".to_owned(), "installation".to_owned()),
                ("cdenv.workspace".to_owned(), "workspace".to_owned()),
                ("cdenv.generation".to_owned(), "2".to_owned()),
                (
                    "cdenv.profile".to_owned(),
                    "cdenv-devcontainer-v1".to_owned(),
                ),
                ("cdenv.generated".to_owned(), "true".to_owned()),
            ])
        } else {
            BTreeMap::new()
        };
        ImageInspection {
            id: ImageId::parse(IMAGE_ID).expect("image ID"),
            labels,
            architecture: ContainerArchitecture::X86_64,
        }
    }

    fn inspection(runtime: &RuntimePlan, ports: &PortPlan, running: bool) -> ContainerInspection {
        let mut mounts = std::iter::once(&runtime.workspace.mount)
            .chain(&runtime.mounts)
            .map(|mount| InspectedMount {
                kind: match mount.kind {
                    MountKind::Bind => "bind",
                    MountKind::Volume => "volume",
                }
                .to_owned(),
                source: mount.source.clone(),
                target: mount.target.as_str().to_owned(),
                mode: Some(
                    mount
                        .options
                        .iter()
                        .map(|option| {
                            option.value.as_ref().map_or_else(
                                || option.name.clone(),
                                |value| format!("{}={value}", option.name),
                            )
                        })
                        .collect::<Vec<_>>()
                        .join(","),
                ),
            })
            .collect::<Vec<_>>();
        mounts.sort();
        let mut inspected_ports = Vec::new();
        for publication in &ports.publications {
            let protocol = match publication.protocol {
                PublicationProtocol::Tcp => "tcp",
                PublicationProtocol::Udp => "udp",
                PublicationProtocol::Sctp => "sctp",
            };
            for port in
                publication.container_ports.start.get()..=publication.container_ports.end.get()
            {
                let host_port = publication.host_ports.map_or(49152, |range| {
                    range.start.get() + port - publication.container_ports.start.get()
                });
                let host_ip = match publication.binding {
                    PublicationBinding::Loopback(address)
                    | PublicationBinding::NonLoopback(address) => Some(address.to_string()),
                    PublicationBinding::AllInterfaces => Some("0.0.0.0".to_owned()),
                };
                inspected_ports.push(InspectedPortBinding {
                    container: format!("{port}/{protocol}"),
                    host_ip,
                    host_port: Some(host_port.to_string()),
                });
            }
        }
        inspected_ports.sort();
        ContainerInspection {
            id: ContainerId::parse(CONTAINER_ID).expect("container ID"),
            name: "cdenv-workspace-2".to_owned(),
            image_id: ImageId::parse(IMAGE_ID).expect("image ID"),
            image_reference: Some(IMAGE_ID.to_owned()),
            labels: BTreeMap::from([
                ("cdenv.installation".to_owned(), "installation".to_owned()),
                ("cdenv.workspace".to_owned(), "workspace".to_owned()),
                ("cdenv.generation".to_owned(), "2".to_owned()),
                (
                    "cdenv.profile".to_owned(),
                    "cdenv-devcontainer-v1".to_owned(),
                ),
            ]),
            user: runtime.container_user.as_str().to_owned(),
            working_directory: runtime.workspace.folder.as_str().to_owned(),
            mounts,
            ports: inspected_ports,
            running,
        }
    }

    fn discovered(id: &str, generation: &str, running: bool) -> DiscoveredContainer {
        DiscoveredContainer {
            id: ContainerId::parse(id).expect("container ID"),
            names: vec!["cdenv-workspace-2".to_owned()],
            image_id: Some(ImageId::parse(IMAGE_ID).expect("image ID")),
            labels: BTreeMap::from([
                ("cdenv.installation".to_owned(), "installation".to_owned()),
                ("cdenv.workspace".to_owned(), "workspace".to_owned()),
                ("cdenv.generation".to_owned(), generation.to_owned()),
            ]),
            state: Some(if running { "running" } else { "exited" }.to_owned()),
        }
    }

    #[test]
    fn orchestration_errors_are_send_sync_and_static() {
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}

        assert_error::<ImageContainerError>();
    }

    #[tokio::test]
    async fn image_creation_evaluates_then_pulls_creates_verifies_and_starts_without_state_writes()
    {
        let plans = plans(r#"{"image":"example.invalid/base:latest","appPort":3000}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::default();
        let engine = FakeEngine::default()
            .with_images([image(false), image(false)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, false),
                inspection(&plans.runtime, &plans.ports, true),
            ]);
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine.clone());
        let caps = capabilities();
        let owned = [cdenv_devcontainer::ContainerPath::parse("/opt/cdenv").expect("owned")];

        let facts = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/workspaces/source"),
                    build_tag: None,
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &["sleep".to_owned(), "infinity".to_owned()],
                    cdenv_owned_targets: &owned,
                    host_requirements: plans.requirements.as_ref(),
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect("creation");

        assert_eq!(
            (facts.container.as_str(), facts.architecture, facts.running),
            (CONTAINER_ID, ContainerArchitecture::X86_64, true)
        );
        assert_eq!(cli.calls(), ["pull", "create"]);
        assert_eq!(
            engine.calls(),
            [
                "discover",
                "inspect-image",
                "inspect-container",
                "inspect-image",
                "start",
                "inspect-container"
            ]
        );
    }

    #[tokio::test]
    async fn hard_host_requirement_fails_before_any_adapter_mutation() {
        let plans = plans(r#"{"image":"base","hostRequirements":{"cpus":64}}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::default();
        let engine = FakeEngine::default();
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine.clone());
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: None,
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: plans.requirements.as_ref(),
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("host requirement");

        assert!(matches!(error, ImageContainerError::HostRequirement(_)));
        assert!(cli.calls().is_empty());
        assert!(engine.calls().is_empty());
    }

    #[tokio::test]
    async fn explicit_gpu_requirement_is_the_only_source_of_an_owned_gpu_grant() {
        let plans = plans(r#"{"image":"base","hostRequirements":{"gpu":true}}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::default();
        let engine = FakeEngine::default()
            .with_images([image(false), image(false)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, false),
                inspection(&plans.runtime, &plans.ports, true),
            ]);
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine);
        let caps = HostCapabilities {
            gpu: Measured::Reliable(Some(cdenv_devcontainer::GpuCapabilities {
                cores: None,
                memory_bytes: None,
            })),
            ..capabilities()
        };

        orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: None,
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: plans.requirements.as_ref(),
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect("GPU creation");

        assert_eq!(
            cli.gpu_access(),
            Some(cdenv_devcontainer::GpuAccessIntent::Requested)
        );
    }

    #[tokio::test]
    async fn dockerfile_creation_builds_verified_labeled_image_before_candidate() {
        let plans = plans(r#"{"build":{"dockerfile":"Dockerfile"}}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::default();
        let engine = FakeEngine::default()
            .with_images([image(true), image(true)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, false),
                inspection(&plans.runtime, &plans.ports, true),
            ]);
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine);
        let caps = capabilities();

        let facts = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: Some("cdenv/workspace:g2"),
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect("Dockerfile creation");

        assert_eq!(facts.image.as_str(), IMAGE_ID);
        assert_eq!(cli.calls(), ["build", "create"]);
    }

    #[tokio::test]
    async fn dockerfile_image_label_mismatch_cleans_only_the_claimed_generated_image() {
        let plans = plans(r#"{"build":{"dockerfile":"Dockerfile"}}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::default();
        let engine = FakeEngine::default().with_images([image(false)]);
        let orchestrator = ImageContainerOrchestrator::new(cli, engine.clone());
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: Some("cdenv/workspace:g2"),
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("label mismatch");

        assert!(matches!(error, ImageContainerError::ImageMismatch { .. }));
        assert_eq!(engine.cleanup_counts(), (0, 1));
    }

    #[tokio::test]
    async fn cancellation_after_build_cleans_generated_image_before_container_create() {
        let plans = plans(r#"{"build":{"dockerfile":"Dockerfile"}}"#);
        let (installation, workspace, profile, generation) = identities();
        let cli = FakeCli::cancelling_build();
        let engine = FakeEngine::default();
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine.clone());
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: Some("cdenv/workspace:g2"),
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("cancelled");

        assert!(matches!(error, ImageContainerError::Cancelled));
        assert_eq!(cli.calls(), ["build"]);
        assert_eq!(engine.cleanup_counts(), (0, 1));
    }

    #[tokio::test]
    async fn runtime_mismatches_clean_candidate_and_generated_image_without_prior_resources() {
        let plans = plans(r#"{"build":{"dockerfile":"Dockerfile"},"appPort":3000}"#);
        let (installation, workspace, profile, generation) = identities();
        let caps = capabilities();
        let stale = discovered(OTHER_CONTAINER_ID, "1", false);
        for field in ["user", "workspace", "mounts", "ports"] {
            let mut wrong = inspection(&plans.runtime, &plans.ports, false);
            match field {
                "user" => wrong.user = "other".to_owned(),
                "workspace" => wrong.working_directory = "/other".to_owned(),
                "mounts" => wrong.mounts.clear(),
                "ports" => wrong.ports.clear(),
                _ => unreachable!(),
            }
            let cleanup_inspection = inspection(&plans.runtime, &plans.ports, false);
            let engine = FakeEngine::default()
                .with_discovery(vec![stale.clone()])
                .with_images([image(true)])
                .with_containers([wrong, cleanup_inspection]);
            let orchestrator = ImageContainerOrchestrator::new(FakeCli::default(), engine.clone());
            let error = orchestrator
                .create(
                    &ImageContainerCreateRequest {
                        build: &plans.docker.build,
                        checkout: Path::new("/tmp"),
                        build_tag: Some("cdenv/workspace:g2"),
                        build_context: &DockerBuildContext::Repository,
                        dockerfile: DockerfileInput::Repository,
                        container_name: "cdenv-workspace-2",
                        identity: identity(&installation, &workspace, &profile, generation),
                        runtime: &plans.runtime,
                        create_options: &plans.docker.create,
                        ports: &plans.ports,
                        command: &[],
                        cdenv_owned_targets: &[],
                        host_requirements: None,
                        host_capabilities: &caps,
                        recorded_container: Some(&stale.id),
                    },
                    &CancellationToken::default(),
                )
                .await
                .expect_err("runtime mismatch");
            assert!(
                matches!(error, ImageContainerError::RuntimeMismatch { .. }),
                "{field}: {error:?}"
            );
            assert_eq!(engine.cleanup_counts(), (1, 1));
            assert!(
                !engine
                    .state
                    .lock()
                    .expect("engine lock")
                    .cleaned_containers
                    .contains(&stale.id)
            );
        }
    }

    #[tokio::test]
    async fn start_failure_cleans_only_the_verified_candidate_and_generated_image() {
        let plans = plans(r#"{"build":{"dockerfile":"Dockerfile"}}"#);
        let (installation, workspace, profile, generation) = identities();
        let engine = FakeEngine::default()
            .with_images([image(true), image(true)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, false),
                inspection(&plans.runtime, &plans.ports, false),
            ])
            .failing_start();
        let orchestrator = ImageContainerOrchestrator::new(FakeCli::default(), engine.clone());
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: Some("cdenv/workspace:g2"),
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("start failure");

        assert!(matches!(
            error,
            ImageContainerError::Bollard {
                operation: "start candidate",
                ..
            }
        ));
        assert_eq!(engine.cleanup_counts(), (1, 1));
    }

    #[tokio::test]
    async fn unsupported_architecture_is_preserved_and_does_not_create_a_container() {
        let plans = plans(r#"{"image":"base"}"#);
        let (installation, workspace, profile, generation) = identities();
        let error = ContainerArchitecture::parse("riscv64").expect_err("unsupported");
        let engine = FakeEngine::default();
        engine
            .state
            .lock()
            .expect("engine lock")
            .images
            .push_back(Err(BollardAdapterError::UnsupportedArchitecture(error)));
        let cli = FakeCli::default();
        let orchestrator = ImageContainerOrchestrator::new(cli.clone(), engine);
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: None,
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("architecture");

        assert!(matches!(
            error,
            ImageContainerError::Bollard {
                source: BollardAdapterError::UnsupportedArchitecture(_),
                ..
            }
        ));
        assert_eq!(cli.calls(), ["pull"]);
    }

    #[test]
    fn match_classification_rejects_missing_stale_external_and_duplicate_without_selection() {
        let workspace = WorkspaceName::parse("workspace").expect("workspace");
        let generation = GenerationId::new(2).expect("generation");
        let recorded = ContainerId::parse(CONTAINER_ID).expect("recorded");
        let cases = [
            (vec![], ImageContainerMatchState::Missing),
            (
                vec![discovered(OTHER_CONTAINER_ID, "1", false)],
                ImageContainerMatchState::StaleOnly,
            ),
            (
                vec![discovered(OTHER_CONTAINER_ID, "2", false)],
                ImageContainerMatchState::ExternalReplacement,
            ),
            (
                vec![
                    discovered(CONTAINER_ID, "2", false),
                    discovered(OTHER_CONTAINER_ID, "2", false),
                ],
                ImageContainerMatchState::AmbiguousCurrent,
            ),
        ];
        for (containers, expected) in cases {
            let correlated = correlate_containers(
                &containers,
                &[WorkspaceCorrelation {
                    workspace: &workspace,
                    generation,
                    recorded_container: Some(&recorded),
                }],
            );
            assert_eq!(
                classify_image_container_matches(&correlated[0], Some(&recorded)),
                expected
            );
        }
    }

    #[tokio::test]
    async fn recorded_running_container_is_verified_stopped_and_repeated_stop_is_safe() {
        let plans = plans(r#"{"image":"base","appPort":3000}"#);
        let (installation, workspace, profile, generation) = identities();
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let image_id = ImageId::parse(IMAGE_ID).expect("image");
        let request = RecordedContainerRequest {
            container: &container,
            container_name: "cdenv-workspace-2",
            image: &image_id,
            identity: identity(&installation, &workspace, &profile, generation),
            runtime: &plans.runtime,
            ports: &plans.ports,
        };
        let engine = FakeEngine::default()
            .with_discovery(vec![discovered(CONTAINER_ID, "2", true)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, true),
                inspection(&plans.runtime, &plans.ports, false),
            ]);
        let orchestrator = ImageContainerOrchestrator::new(FakeCli::default(), engine.clone());

        let outcome = orchestrator
            .stop_recorded(
                &request,
                std::time::Duration::from_secs(10),
                &CancellationToken::default(),
            )
            .await
            .expect("stop");

        assert_eq!(outcome, ImageContainerStopOutcome::Stopped);
        assert_eq!(
            engine.calls(),
            ["discover", "inspect-container", "stop", "inspect-container"]
        );

        engine.state.lock().expect("engine lock").discovered =
            vec![discovered(CONTAINER_ID, "2", false)];
        engine
            .state
            .lock()
            .expect("engine lock")
            .containers
            .push_back(Ok(inspection(&plans.runtime, &plans.ports, false)));
        let repeated = orchestrator
            .stop_recorded(
                &request,
                std::time::Duration::from_secs(10),
                &CancellationToken::default(),
            )
            .await
            .expect("repeated stop");
        assert_eq!(repeated, ImageContainerStopOutcome::AlreadyStopped);
    }

    #[tokio::test]
    async fn recorded_stopped_container_is_verified_started_and_reverified_directly() {
        let plans = plans(r#"{"image":"base","appPort":3000}"#);
        let (installation, workspace, profile, generation) = identities();
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let image_id = ImageId::parse(IMAGE_ID).expect("image");
        let engine = FakeEngine::default()
            .with_discovery(vec![discovered(CONTAINER_ID, "2", false)])
            .with_containers([
                inspection(&plans.runtime, &plans.ports, false),
                inspection(&plans.runtime, &plans.ports, true),
            ])
            .with_images([image(false)]);
        let orchestrator = ImageContainerOrchestrator::new(FakeCli::default(), engine.clone());

        let facts = orchestrator
            .restart_recorded(
                &RecordedContainerRequest {
                    container: &container,
                    container_name: "cdenv-workspace-2",
                    image: &image_id,
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    ports: &plans.ports,
                },
                &CancellationToken::default(),
            )
            .await
            .expect("restart");

        assert!(facts.running);
        assert_eq!(
            engine.calls(),
            [
                "discover",
                "inspect-container",
                "start",
                "inspect-container",
                "inspect-image"
            ]
        );
    }

    #[tokio::test]
    async fn cli_create_failure_preserves_source_and_never_attempts_candidate_cleanup() {
        let plans = plans(r#"{"image":"base"}"#);
        let (installation, workspace, profile, generation) = identities();
        let engine = FakeEngine::default().with_images([image(false)]);
        let orchestrator =
            ImageContainerOrchestrator::new(FakeCli::failing("create"), engine.clone());
        let caps = capabilities();

        let error = orchestrator
            .create(
                &ImageContainerCreateRequest {
                    build: &plans.docker.build,
                    checkout: Path::new("/tmp"),
                    build_tag: None,
                    build_context: &DockerBuildContext::Repository,
                    dockerfile: DockerfileInput::Repository,
                    container_name: "cdenv-workspace-2",
                    identity: identity(&installation, &workspace, &profile, generation),
                    runtime: &plans.runtime,
                    create_options: &plans.docker.create,
                    ports: &plans.ports,
                    command: &[],
                    cdenv_owned_targets: &[],
                    host_requirements: None,
                    host_capabilities: &caps,
                    recorded_container: None,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("create failure");

        assert!(matches!(
            error,
            ImageContainerError::DockerCli {
                operation: "create",
                ..
            }
        ));
        assert_eq!(engine.cleanup_counts(), (0, 0));
    }
}
