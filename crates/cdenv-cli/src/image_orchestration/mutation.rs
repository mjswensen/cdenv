//! Image and Dockerfile mutation sequencing and operation-owned rollback.

use cdenv_core::ContainerId;
use cdenv_devcontainer::{
    BuildPlan, HostRequirementEvaluation, PortPlan, RuntimePlan, evaluate_host_requirements,
};

use super::planning::{
    ImageContainerMatchState, classify_image_container_matches, verify_built_image, verify_runtime,
};
use super::{
    ImageCleanupFailure, ImageContainerCreateRequest, ImageContainerError, ImageContainerFacts,
    ImageContainerOrchestrator, ImageContainerStopOutcome, ImageDockerCli, ImageDockerEngine,
    RecordedContainerRequest,
};
use crate::{
    BollardAdapter, CancellationToken, ContainerExpectation, ContainerInspection,
    DockerBuildRequest, DockerCreateRequest, DockerResourceIdentity, ImageCleanupExpectation,
    ImageId, WorkspaceCorrelation, correlate_containers,
};

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
                            no_cache: request.no_cache,
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
