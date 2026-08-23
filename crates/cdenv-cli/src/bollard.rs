//! Typed discovery, verification, and control for cdenv-owned Docker resources.

mod exec;

use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::time::Duration;

use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerInspectResponse, ContainerSummary, ImageInspect};
use bollard::query_parameters::{
    ListContainersOptionsBuilder, RemoveContainerOptionsBuilder, RemoveImageOptionsBuilder,
    RenameContainerOptionsBuilder, StopContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use cdenv_core::{
    ContainerArchitecture, ContainerId, GenerationId, InstallationId, ProfileId,
    UnsupportedContainerArchitecture, WorkspaceName,
};
use futures_util::StreamExt;
use thiserror::Error;

pub use exec::{
    AttachedExec, DetachedExec, ExecCommand, ExecId, ExecInspect, ExecStreamError,
    MAXIMUM_EXEC_FRAME_BYTES, decode_docker_multiplexed,
};

use exec::{ExecApiConfiguration, ExecApiIo, ExecOutput};

use crate::{BollardConnector, DockerEndpoint, ImageId};

/// Default bound applied to each Docker discovery and control API call.
pub const BOLLARD_CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) const INSTALLATION_LABEL: &str = "cdenv.installation";
pub(crate) const WORKSPACE_LABEL: &str = "cdenv.workspace";
pub(crate) const GENERATION_LABEL: &str = "cdenv.generation";
pub(crate) const PROFILE_LABEL: &str = "cdenv.profile";
const GENERATED_IMAGE_LABEL: &str = "cdenv.generated";
pub(crate) const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub(crate) const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

/// The exact scope sent in one `all=true` container-list request.
#[derive(Clone, Copy, Debug)]
pub struct ContainerDiscoveryScope<'a> {
    /// Required installation namespace.
    pub installation: &'a InstallationId,
    /// Optional exact workspace filter. Omit this to correlate many workspaces in memory.
    pub workspace: Option<&'a WorkspaceName>,
    /// Optional exact generation filter.
    pub generation: Option<GenerationId>,
}

impl ContainerDiscoveryScope<'_> {
    fn filters(self) -> BTreeMap<String, Vec<String>> {
        let mut labels = vec![format!("{INSTALLATION_LABEL}={}", self.installation)];
        if let Some(workspace) = self.workspace {
            labels.push(format!("{WORKSPACE_LABEL}={workspace}"));
        }
        if let Some(generation) = self.generation {
            labels.push(format!("{GENERATION_LABEL}={generation}"));
        }
        BTreeMap::from([("label".to_owned(), labels)])
    }
}

/// A lossless, typed container-list entry used for in-memory correlation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredContainer {
    /// Full, non-abbreviated container ID.
    pub id: ContainerId,
    /// Every Docker-reported name, with the historical leading slash removed.
    pub names: Vec<String>,
    /// Exact image identity reported by Docker, when present.
    pub image_id: Option<ImageId>,
    /// All labels returned by the list API.
    pub labels: BTreeMap<String, String>,
    /// Docker's state text, when present.
    pub state: Option<String>,
}

impl DiscoveredContainer {
    /// Reports whether the list result says this container is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state.as_deref() == Some("running")
    }
}

/// Inputs for classifying a workspace from one installation-wide list result.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceCorrelation<'a> {
    /// Workspace to correlate.
    pub workspace: &'a WorkspaceName,
    /// Expected active generation.
    pub generation: GenerationId,
    /// Persisted container identity, when one was successfully provisioned.
    pub recorded_container: Option<&'a ContainerId>,
}

/// All current and stale matches for one workspace; no arbitrary match is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelatedContainers {
    /// Workspace identity.
    pub workspace: WorkspaceName,
    /// Every match for the expected generation.
    pub current: Vec<DiscoveredContainer>,
    /// Every match for another or malformed generation.
    pub stale: Vec<DiscoveredContainer>,
    /// Whether the recorded ID occurs among current matches.
    pub recorded_current: bool,
    /// Current-generation matches other than the recorded ID.
    pub external_replacements: Vec<DiscoveredContainer>,
}

/// Correlates many workspaces in memory while preserving duplicate and stale matches.
#[must_use]
pub fn correlate_containers(
    containers: &[DiscoveredContainer],
    inputs: &[WorkspaceCorrelation<'_>],
) -> Vec<CorrelatedContainers> {
    inputs
        .iter()
        .map(|input| {
            let expected_generation = input.generation.to_string();
            let mut current = Vec::new();
            let mut stale = Vec::new();
            for container in containers.iter().filter(|container| {
                container.labels.get(WORKSPACE_LABEL).map(String::as_str)
                    == Some(input.workspace.as_str())
            }) {
                if container
                    .labels
                    .get(GENERATION_LABEL)
                    .is_some_and(|value| value == &expected_generation)
                {
                    current.push(container.clone());
                } else {
                    stale.push(container.clone());
                }
            }
            let recorded_current = input
                .recorded_container
                .is_some_and(|recorded| current.iter().any(|container| &container.id == recorded));
            let external_replacements = current
                .iter()
                .filter(|container| {
                    input
                        .recorded_container
                        .is_none_or(|recorded| &container.id != recorded)
                })
                .cloned()
                .collect();
            CorrelatedContainers {
                workspace: input.workspace.clone(),
                current,
                stale,
                recorded_current,
                external_replacements,
            }
        })
        .collect()
}

/// One Docker-inspected container mount.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InspectedMount {
    /// Docker mount kind, such as `bind` or `volume`.
    pub kind: String,
    /// Bind source path or named-volume name.
    pub source: Option<String>,
    /// Absolute container destination.
    pub target: String,
    /// Docker-normalized comma-separated mode.
    pub mode: Option<String>,
}

/// One Docker-inspected requested host port binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InspectedPortBinding {
    /// Container port/protocol key, such as `3000/tcp`.
    pub container: String,
    /// Docker-normalized host address.
    pub host_ip: Option<String>,
    /// Requested or daemon-assigned host port.
    pub host_port: Option<String>,
}

/// Authoritative typed container inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerInspection {
    /// Full container identity.
    pub id: ContainerId,
    /// Exact name with Docker's historical leading slash removed.
    pub name: String,
    /// Content-addressed image identity used to create the container.
    pub image_id: ImageId,
    /// Image reference supplied at creation, when present.
    pub image_reference: Option<String>,
    /// Exact labels.
    pub labels: BTreeMap<String, String>,
    /// Configured container user.
    pub user: String,
    /// Configured container working directory.
    pub working_directory: String,
    /// Docker-inspected mounts in stable order.
    pub mounts: Vec<InspectedMount>,
    /// Docker-inspected requested port bindings in stable order.
    pub ports: Vec<InspectedPortBinding>,
    /// Whether Docker reports the container running.
    pub running: bool,
}

/// Authoritative typed image inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInspection {
    /// Content-addressed image identity.
    pub id: ImageId,
    /// Exact image labels.
    pub labels: BTreeMap<String, String>,
    /// Supported container architecture.
    pub architecture: ContainerArchitecture,
}

/// Expected identity and state used to reject drift or external replacement.
#[derive(Clone, Copy, Debug)]
pub struct ContainerExpectation<'a> {
    /// Exact recorded/claimed container ID.
    pub id: &'a ContainerId,
    /// Exact cdenv-owned name.
    pub name: &'a str,
    /// Exact content-addressed image identity.
    pub image_id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
    /// Compose project, for Compose primary containers.
    pub project: Option<&'a str>,
    /// Compose service, for Compose primary containers.
    pub service: Option<&'a str>,
    /// Required running state, or `None` when either state is acceptable.
    pub running: Option<bool>,
}

/// Expected authoritative identity for a Compose primary claim.
#[derive(Clone, Copy, Debug)]
pub struct ComposePrimaryExpectation<'a> {
    /// Exact ID claimed by Compose for the primary service.
    pub id: &'a ContainerId,
    /// Exact content-addressed final image.
    pub image_id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
    /// Isolated Compose project.
    pub project: &'a str,
    /// Exact primary service.
    pub service: &'a str,
    /// Required running state.
    pub running: bool,
}

/// Identity required before narrowly scoped image cleanup.
#[derive(Clone, Copy, Debug)]
pub struct ImageCleanupExpectation<'a> {
    /// Exact operation-claimed image ID.
    pub id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
}

/// Internal API request exposed only to permit static fake implementations at adapter seams.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BollardApiRequest {
    /// Daemon ping.
    Ping,
    /// Container list.
    ListContainers {
        /// Must be true for discovery.
        all: bool,
        /// Exact Docker filters.
        filters: BTreeMap<String, Vec<String>>,
    },
    /// Container inspect.
    InspectContainer { id: String },
    /// Image inspect.
    InspectImage { id: String },
    /// Container start.
    StartContainer { id: String },
    /// Graceful container stop.
    StopContainer { id: String, seconds: i32 },
    /// Container rename.
    RenameContainer { id: String, name: String },
    /// Tar archive upload.
    UploadArchive {
        id: String,
        path: String,
        archive: Vec<u8>,
    },
    /// Exact container removal.
    RemoveContainer { id: String },
    /// Exact image removal.
    RemoveImage { id: String },
    /// Create an attached or detached Exec configuration.
    CreateExec {
        /// Exact container ID.
        container: String,
        /// Owned Exec settings.
        configuration: ExecApiConfiguration,
    },
    /// Start an Exec process without attaching streams.
    StartDetachedExec { id: String },
    /// Inspect an Exec process.
    InspectExec { id: String },
}

/// Internal API response paired with [`BollardApiRequest`].
#[doc(hidden)]
#[derive(Debug)]
pub enum BollardApiResponse {
    /// Ping text.
    Ping(String),
    /// Container summaries.
    Containers(Vec<ContainerSummary>),
    /// Container inspection.
    Container(Box<ContainerInspectResponse>),
    /// Image inspection.
    Image(Box<ImageInspect>),
    /// Created Exec ID.
    ExecCreated(String),
    /// Exec inspection response.
    ExecInspect(Box<bollard::models::ExecInspectResponse>),
    /// Successful unit response.
    Unit,
}

/// Static-dispatch API seam used by the real Bollard client and deterministic fakes.
#[doc(hidden)]
pub trait BollardApi: Clone + Send + Sync + 'static {
    /// Executes one typed control API request.
    fn execute(
        &self,
        request: BollardApiRequest,
    ) -> impl Future<Output = Result<BollardApiResponse, BollardApiError>> + Send;

    /// Starts an attached Exec upgrade and returns its separated API streams.
    fn start_attached_exec(
        &self,
        _id: String,
        _output_capacity: usize,
    ) -> impl Future<Output = Result<ExecApiIo, BollardApiError>> + Send {
        async {
            Err(BollardApiError {
                message: "attached Exec is unavailable from this API implementation".to_owned(),
            })
        }
    }
}

/// A bounded, safe rendering of a lower-level API failure.
#[doc(hidden)]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{message}")]
pub struct BollardApiError {
    message: String,
}

impl BollardApiError {
    #[expect(
        clippy::needless_pass_by_value,
        reason = "Result::map_err transfers ownership of the Bollard error"
    )]
    fn from_bollard(error: bollard::errors::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

/// Real pinned Bollard API implementation.
#[derive(Clone)]
pub struct BollardClientApi {
    client: Docker,
}

impl BollardApi for BollardClientApi {
    #[expect(
        clippy::too_many_lines,
        reason = "the one-request dispatch keeps the exact API mapping auditable in one match"
    )]
    async fn execute(
        &self,
        request: BollardApiRequest,
    ) -> Result<BollardApiResponse, BollardApiError> {
        match request {
            BollardApiRequest::Ping => self
                .client
                .ping()
                .await
                .map(BollardApiResponse::Ping)
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::ListContainers { all, filters } => {
                let filters: HashMap<_, _> = filters.into_iter().collect();
                let options = ListContainersOptionsBuilder::new()
                    .all(all)
                    .filters(&filters)
                    .build();
                self.client
                    .list_containers(Some(options))
                    .await
                    .map(BollardApiResponse::Containers)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::InspectContainer { id } => self
                .client
                .inspect_container(&id, None)
                .await
                .map(Box::new)
                .map(BollardApiResponse::Container)
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::InspectImage { id } => self
                .client
                .inspect_image(&id)
                .await
                .map(Box::new)
                .map(BollardApiResponse::Image)
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::StartContainer { id } => self
                .client
                .start_container(&id, None)
                .await
                .map(|()| BollardApiResponse::Unit)
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::StopContainer { id, seconds } => {
                let options = StopContainerOptionsBuilder::new().t(seconds).build();
                self.client
                    .stop_container(&id, Some(options))
                    .await
                    .map(|()| BollardApiResponse::Unit)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::RenameContainer { id, name } => {
                let options = RenameContainerOptionsBuilder::new().name(&name).build();
                self.client
                    .rename_container(&id, options)
                    .await
                    .map(|()| BollardApiResponse::Unit)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::UploadArchive { id, path, archive } => {
                let options = UploadToContainerOptionsBuilder::new().path(&path).build();
                self.client
                    .upload_to_container(&id, Some(options), bollard::body_full(archive.into()))
                    .await
                    .map(|()| BollardApiResponse::Unit)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::RemoveContainer { id } => {
                let options = RemoveContainerOptionsBuilder::new()
                    .force(false)
                    .v(false)
                    .build();
                self.client
                    .remove_container(&id, Some(options))
                    .await
                    .map(|()| BollardApiResponse::Unit)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::RemoveImage { id } => {
                let options = RemoveImageOptionsBuilder::new()
                    .force(false)
                    .noprune(true)
                    .build();
                self.client
                    .remove_image(&id, Some(options), None)
                    .await
                    .map(|_| BollardApiResponse::Unit)
                    .map_err(BollardApiError::from_bollard)
            }
            BollardApiRequest::CreateExec {
                container,
                configuration,
            } => self
                .client
                .create_exec(&container, CreateExecOptions::<String>::from(configuration))
                .await
                .map(|created| BollardApiResponse::ExecCreated(created.id))
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::StartDetachedExec { id } => self
                .client
                .start_exec(
                    &id,
                    Some(StartExecOptions {
                        detach: true,
                        tty: false,
                        output_capacity: None,
                    }),
                )
                .await
                .and_then(|result| match result {
                    StartExecResults::Detached => Ok(BollardApiResponse::Unit),
                    StartExecResults::Attached { .. } => {
                        Err(bollard::errors::Error::DockerResponseServerError {
                            status_code: 500,
                            message: "Docker attached a detached Exec start".to_owned(),
                        })
                    }
                })
                .map_err(BollardApiError::from_bollard),
            BollardApiRequest::InspectExec { id } => self
                .client
                .inspect_exec(&id)
                .await
                .map(Box::new)
                .map(BollardApiResponse::ExecInspect)
                .map_err(BollardApiError::from_bollard),
        }
    }

    async fn start_attached_exec(
        &self,
        id: String,
        output_capacity: usize,
    ) -> Result<ExecApiIo, BollardApiError> {
        let result = self
            .client
            .start_exec(
                &id,
                Some(StartExecOptions {
                    detach: false,
                    tty: false,
                    output_capacity: Some(output_capacity),
                }),
            )
            .await
            .map_err(BollardApiError::from_bollard)?;
        let StartExecResults::Attached { output, input } = result else {
            return Err(BollardApiError {
                message: "Docker detached an attached Exec start".to_owned(),
            });
        };
        let output = output.map(|item| {
            item.map(|frame| match frame {
                bollard::container::LogOutput::StdOut { message } => {
                    ExecOutput::Stdout(message.to_vec())
                }
                bollard::container::LogOutput::StdErr { message } => {
                    ExecOutput::Stderr(message.to_vec())
                }
                bollard::container::LogOutput::StdIn { message } => {
                    ExecOutput::UnexpectedStdin(message.to_vec())
                }
                bollard::container::LogOutput::Console { message } => {
                    ExecOutput::UnexpectedConsole(message.to_vec())
                }
            })
            .map_err(BollardApiError::from_bollard)
        });
        Ok(ExecApiIo {
            output: Box::pin(output),
            input,
        })
    }
}

/// Typed Bollard adapter with one statically dispatched API implementation.
#[derive(Clone)]
pub struct BollardAdapter<A = BollardClientApi> {
    endpoint: DockerEndpoint,
    api: A,
    timeout: Duration,
}

impl BollardAdapter<BollardClientApi> {
    /// Constructs the production adapter from a connector bound to the resolved Unix socket.
    #[must_use]
    pub fn from_connector(connector: &BollardConnector) -> Self {
        Self {
            endpoint: connector.endpoint().clone(),
            api: BollardClientApi {
                client: connector.client().clone(),
            },
            timeout: BOLLARD_CONTROL_TIMEOUT,
        }
    }
}

impl<A: BollardApi> BollardAdapter<A> {
    /// Constructs an adapter with a static API implementation.
    #[doc(hidden)]
    #[must_use]
    pub fn with_api(endpoint: DockerEndpoint, api: A, timeout: Duration) -> Self {
        Self {
            endpoint,
            api,
            timeout,
        }
    }

    /// Returns the exact resolved daemon endpoint.
    #[must_use]
    pub const fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    /// Pings the daemon within the adapter's per-call timeout.
    ///
    /// # Errors
    ///
    /// Returns a timeout, transport, or unexpected-response error.
    pub async fn ping(&self) -> Result<(), BollardAdapterError> {
        match self.call("ping", BollardApiRequest::Ping).await? {
            BollardApiResponse::Ping(value) if value == "OK" => Ok(()),
            BollardApiResponse::Ping(value) => Err(BollardAdapterError::UnexpectedPing { value }),
            _ => Err(BollardAdapterError::UnexpectedResponse { operation: "ping" }),
        }
    }

    /// Lists all matching containers in exactly one API call.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, response-shape, or identity-mapping error.
    pub async fn discover(
        &self,
        scope: ContainerDiscoveryScope<'_>,
    ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
        let response = self
            .call(
                "list containers",
                BollardApiRequest::ListContainers {
                    all: true,
                    filters: scope.filters(),
                },
            )
            .await?;
        let BollardApiResponse::Containers(containers) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "list containers",
            });
        };
        containers.into_iter().map(map_summary).collect()
    }

    /// Inspects and maps one exact container ID.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, incomplete-response, or malformed-identity error.
    pub async fn inspect_container(
        &self,
        id: &ContainerId,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        let response = self
            .call(
                "inspect container",
                BollardApiRequest::InspectContainer { id: id.to_string() },
            )
            .await?;
        let BollardApiResponse::Container(container) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "inspect container",
            });
        };
        map_container(*container)
    }

    /// Inspects an image and maps its exact identity, labels, and supported architecture.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, incomplete-response, or unsupported-architecture error.
    pub async fn inspect_image(&self, id: &str) -> Result<ImageInspection, BollardAdapterError> {
        let response = self
            .call(
                "inspect image",
                BollardApiRequest::InspectImage { id: id.to_owned() },
            )
            .await?;
        let BollardApiResponse::Image(image) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "inspect image",
            });
        };
        map_image(*image)
    }

    /// Verifies every expected container identity field and state.
    ///
    /// # Errors
    ///
    /// Returns the first exact identity, label, Compose, image, name, or state mismatch.
    pub fn verify_container(
        inspection: &ContainerInspection,
        expected: ContainerExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        verify_value("container ID", expected.id.as_str(), inspection.id.as_str())?;
        verify_value("container name", expected.name, &inspection.name)?;
        verify_value(
            "image ID",
            expected.image_id.as_str(),
            inspection.image_id.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            INSTALLATION_LABEL,
            expected.installation.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            WORKSPACE_LABEL,
            expected.workspace.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            GENERATION_LABEL,
            &expected.generation.to_string(),
        )?;
        verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
        verify_optional_label(&inspection.labels, COMPOSE_PROJECT_LABEL, expected.project)?;
        verify_optional_label(&inspection.labels, COMPOSE_SERVICE_LABEL, expected.service)?;
        if let Some(running) = expected.running {
            verify_value(
                "running state",
                &running.to_string(),
                &inspection.running.to_string(),
            )?;
        }
        Ok(())
    }

    /// Verifies a Compose primary claim and rejects any second matching primary container.
    ///
    /// # Errors
    ///
    /// Returns an inspect/discovery failure, exact identity mismatch, or ambiguous-primary error.
    pub async fn verify_compose_primary(
        &self,
        expected: ComposePrimaryExpectation<'_>,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        let inspection = self.inspect_container(expected.id).await?;
        verify_value("container ID", expected.id.as_str(), inspection.id.as_str())?;
        verify_value(
            "image ID",
            expected.image_id.as_str(),
            inspection.image_id.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            INSTALLATION_LABEL,
            expected.installation.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            WORKSPACE_LABEL,
            expected.workspace.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            GENERATION_LABEL,
            &expected.generation.to_string(),
        )?;
        verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
        verify_label(&inspection.labels, COMPOSE_PROJECT_LABEL, expected.project)?;
        verify_label(&inspection.labels, COMPOSE_SERVICE_LABEL, expected.service)?;
        verify_value(
            "running state",
            &expected.running.to_string(),
            &inspection.running.to_string(),
        )?;

        let matches = self
            .discover(ContainerDiscoveryScope {
                installation: expected.installation,
                workspace: Some(expected.workspace),
                generation: Some(expected.generation),
            })
            .await?
            .into_iter()
            .filter(|container| {
                container
                    .labels
                    .get(COMPOSE_PROJECT_LABEL)
                    .map(String::as_str)
                    == Some(expected.project)
                    && container
                        .labels
                        .get(COMPOSE_SERVICE_LABEL)
                        .map(String::as_str)
                        == Some(expected.service)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].id != *expected.id {
            return Err(BollardAdapterError::AmbiguousComposePrimary {
                project: expected.project.to_owned(),
                service: expected.service.to_owned(),
                matches: matches.len(),
            });
        }
        Ok(inspection)
    }

    /// Starts one exact container.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, or response-shape error.
    pub async fn start(&self, id: &ContainerId) -> Result<(), BollardAdapterError> {
        self.unit(
            "start container",
            BollardApiRequest::StartContainer { id: id.to_string() },
        )
        .await
    }

    /// Stops one exact container with Docker's graceful-stop timeout.
    ///
    /// # Errors
    ///
    /// Returns an invalid timeout, API, timeout, or response-shape error.
    pub async fn stop(&self, id: &ContainerId, grace: Duration) -> Result<(), BollardAdapterError> {
        let seconds =
            i32::try_from(grace.as_secs()).map_err(|_| BollardAdapterError::InvalidStopTimeout)?;
        self.unit(
            "stop container",
            BollardApiRequest::StopContainer {
                id: id.to_string(),
                seconds,
            },
        )
        .await
    }

    /// Renames one exact container.
    ///
    /// # Errors
    ///
    /// Returns an invalid name, API, timeout, or response-shape error.
    pub async fn rename(&self, id: &ContainerId, name: &str) -> Result<(), BollardAdapterError> {
        if name.is_empty() || name.starts_with('-') || name.chars().any(char::is_control) {
            return Err(BollardAdapterError::InvalidContainerName);
        }
        self.unit(
            "rename container",
            BollardApiRequest::RenameContainer {
                id: id.to_string(),
                name: name.to_owned(),
            },
        )
        .await
    }

    /// Uploads an uncompressed tar archive for extraction below an absolute container path.
    ///
    /// # Errors
    ///
    /// Returns an invalid path, API, timeout, or response-shape error.
    pub async fn upload_archive(
        &self,
        id: &ContainerId,
        path: &str,
        archive: &[u8],
    ) -> Result<(), BollardAdapterError> {
        if !path.starts_with('/') || path.contains('\0') {
            return Err(BollardAdapterError::InvalidArchivePath);
        }
        self.unit(
            "upload archive",
            BollardApiRequest::UploadArchive {
                id: id.to_string(),
                path: path.to_owned(),
                archive: archive.to_vec(),
            },
        )
        .await
    }

    /// Removes a container only after re-inspecting its exact ID and all cdenv identity labels.
    ///
    /// # Errors
    ///
    /// Returns an inspect/verification failure, a running-state refusal, or a removal failure.
    pub async fn cleanup_container(
        &self,
        expected: ContainerExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        let inspection = self.inspect_container(expected.id).await?;
        Self::verify_container(&inspection, expected)?;
        if inspection.running {
            return Err(BollardAdapterError::CleanupRunningContainer { id: inspection.id });
        }
        self.unit(
            "remove container",
            BollardApiRequest::RemoveContainer {
                id: inspection.id.to_string(),
            },
        )
        .await
    }

    /// Removes an image only after exact ID and cdenv-generated labels are verified.
    ///
    /// # Errors
    ///
    /// Returns an inspect/verification or removal failure.
    pub async fn cleanup_image(
        &self,
        expected: ImageCleanupExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        let inspection = self.inspect_image(expected.id.as_str()).await?;
        verify_value("image ID", expected.id.as_str(), inspection.id.as_str())?;
        verify_label(
            &inspection.labels,
            INSTALLATION_LABEL,
            expected.installation.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            WORKSPACE_LABEL,
            expected.workspace.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            GENERATION_LABEL,
            &expected.generation.to_string(),
        )?;
        verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
        verify_label(&inspection.labels, GENERATED_IMAGE_LABEL, "true")?;
        self.unit(
            "remove image",
            BollardApiRequest::RemoveImage {
                id: inspection.id.as_str().to_owned(),
            },
        )
        .await
    }

    async fn unit(
        &self,
        operation: &'static str,
        request: BollardApiRequest,
    ) -> Result<(), BollardAdapterError> {
        match self.call(operation, request).await? {
            BollardApiResponse::Unit => Ok(()),
            _ => Err(BollardAdapterError::UnexpectedResponse { operation }),
        }
    }

    async fn call(
        &self,
        operation: &'static str,
        request: BollardApiRequest,
    ) -> Result<BollardApiResponse, BollardAdapterError> {
        tokio::time::timeout(self.timeout, self.api.execute(request))
            .await
            .map_err(|_| BollardAdapterError::TimedOut {
                operation,
                timeout: self.timeout,
            })?
            .map_err(|source| BollardAdapterError::Api { operation, source })
    }
}

/// Precise discovery, verification, or control failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BollardAdapterError {
    /// A bounded Docker API operation timed out.
    #[error("Docker {operation} timed out after {timeout:?}")]
    TimedOut {
        /// Safe operation name.
        operation: &'static str,
        /// Configured bound.
        timeout: Duration,
    },
    /// Bollard or the daemon rejected an operation.
    #[error("Docker {operation} failed: {source}")]
    Api {
        /// Safe operation name.
        operation: &'static str,
        /// Bounded lower-level failure.
        source: BollardApiError,
    },
    /// Ping returned a response other than Docker's exact `OK`.
    #[error("Docker ping returned unexpected response {value:?}")]
    UnexpectedPing {
        /// Unexpected daemon text.
        value: String,
    },
    /// The static API seam returned the wrong typed response.
    #[error("Docker {operation} returned an unexpected response shape")]
    UnexpectedResponse {
        /// Safe operation name.
        operation: &'static str,
    },
    /// Docker omitted a required inspect/list field.
    #[error("Docker {resource} response omitted required field {field}")]
    MissingField {
        /// API response kind.
        resource: &'static str,
        /// Missing Docker field.
        field: &'static str,
    },
    /// Docker returned a malformed full container ID.
    #[error("Docker returned invalid container ID {value:?}: {source}")]
    InvalidContainerId {
        /// Rejected daemon value.
        value: String,
        /// Exact identity validation failure.
        source: cdenv_core::ContainerIdError,
    },
    /// Docker returned a malformed content-addressed image ID.
    #[error("Docker {resource} returned invalid image ID {value:?}")]
    InvalidImageId {
        /// API response kind.
        resource: &'static str,
        /// Rejected daemon value.
        value: String,
    },
    /// An image has no supported architecture.
    #[error(transparent)]
    UnsupportedArchitecture(#[from] UnsupportedContainerArchitecture),
    /// An authoritative value did not match the operation/state claim.
    #[error("Docker verification mismatch for {field}: expected {expected:?}, found {actual:?}")]
    VerificationMismatch {
        /// Identity/state field.
        field: &'static str,
        /// Required value.
        expected: String,
        /// Docker value, or absence.
        actual: Option<String>,
    },
    /// Compose primary discovery did not produce exactly the claimed container.
    #[error(
        "Compose primary `{project}/{service}` is ambiguous or substituted ({matches} matching containers)"
    )]
    AmbiguousComposePrimary {
        /// Isolated project.
        project: String,
        /// Exact primary service.
        service: String,
        /// Number of matching primary labels.
        matches: usize,
    },
    /// Grace timeout does not fit Docker's API.
    #[error("container stop timeout is too large for the Docker API")]
    InvalidStopTimeout,
    /// A rename target is empty, option-shaped, or contains control characters.
    #[error("invalid Docker container rename target")]
    InvalidContainerName,
    /// Upload extraction target is not a safe absolute container path.
    #[error("Docker archive upload path must be an absolute NUL-free container path")]
    InvalidArchivePath,
    /// Cleanup deliberately refuses to force-remove a running container.
    #[error("refusing to clean up running operation-owned container {id}")]
    CleanupRunningContainer {
        /// Container deliberately left intact.
        id: ContainerId,
    },
}

fn map_summary(summary: ContainerSummary) -> Result<DiscoveredContainer, BollardAdapterError> {
    let id = required(summary.id, "container list", "Id")?;
    let id = ContainerId::parse(&id)
        .map_err(|source| BollardAdapterError::InvalidContainerId { value: id, source })?;
    Ok(DiscoveredContainer {
        id,
        names: summary
            .names
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim_start_matches('/').to_owned())
            .collect(),
        image_id: summary
            .image_id
            .map(|value| parse_image_id(value, "container list"))
            .transpose()?,
        labels: summary.labels.unwrap_or_default().into_iter().collect(),
        state: summary.state.map(|state| state.to_string()),
    })
}

fn map_container(
    container: ContainerInspectResponse,
) -> Result<ContainerInspection, BollardAdapterError> {
    let id = required(container.id, "container inspect", "Id")?;
    let id = ContainerId::parse(&id)
        .map_err(|source| BollardAdapterError::InvalidContainerId { value: id, source })?;
    let config = required(container.config, "container inspect", "Config")?;
    let state = required(container.state, "container inspect", "State")?;
    let mut mounts = container
        .mounts
        .unwrap_or_default()
        .into_iter()
        .map(|mount| {
            let kind = required(mount.typ, "container inspect mount", "Type")?;
            let source = if kind == "volume" {
                mount.name
            } else {
                mount.source
            };
            Ok(InspectedMount {
                kind,
                source,
                target: required(mount.destination, "container inspect mount", "Destination")?,
                mode: mount.mode,
            })
        })
        .collect::<Result<Vec<_>, BollardAdapterError>>()?;
    mounts.sort();
    let mut ports = container
        .host_config
        .and_then(|host| host.port_bindings)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(container, bindings)| {
            bindings
                .unwrap_or_default()
                .into_iter()
                .map(move |binding| InspectedPortBinding {
                    container: container.clone(),
                    host_ip: binding.host_ip,
                    host_port: binding.host_port,
                })
        })
        .collect::<Vec<_>>();
    ports.sort();
    Ok(ContainerInspection {
        id,
        name: required(container.name, "container inspect", "Name")?
            .trim_start_matches('/')
            .to_owned(),
        image_id: parse_image_id(
            required(container.image, "container inspect", "Image")?,
            "container inspect",
        )?,
        image_reference: config.image,
        labels: config.labels.unwrap_or_default().into_iter().collect(),
        user: config.user.unwrap_or_default(),
        working_directory: config.working_dir.unwrap_or_default(),
        mounts,
        ports,
        running: state.running.unwrap_or(false),
    })
}

fn map_image(image: ImageInspect) -> Result<ImageInspection, BollardAdapterError> {
    let architecture = required(image.architecture, "image inspect", "Architecture")?;
    Ok(ImageInspection {
        id: parse_image_id(required(image.id, "image inspect", "Id")?, "image inspect")?,
        labels: image
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default()
            .into_iter()
            .collect(),
        architecture: ContainerArchitecture::parse(&architecture)?,
    })
}

fn parse_image_id(value: String, resource: &'static str) -> Result<ImageId, BollardAdapterError> {
    ImageId::parse(&value).map_err(|_| BollardAdapterError::InvalidImageId { resource, value })
}

fn required<T>(
    value: Option<T>,
    resource: &'static str,
    field: &'static str,
) -> Result<T, BollardAdapterError> {
    value.ok_or(BollardAdapterError::MissingField { resource, field })
}

fn verify_value(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), BollardAdapterError> {
    if expected == actual {
        Ok(())
    } else {
        Err(BollardAdapterError::VerificationMismatch {
            field,
            expected: expected.to_owned(),
            actual: Some(actual.to_owned()),
        })
    }
}

fn verify_label(
    labels: &BTreeMap<String, String>,
    key: &'static str,
    expected: &str,
) -> Result<(), BollardAdapterError> {
    match labels.get(key) {
        Some(actual) if actual == expected => Ok(()),
        actual => Err(BollardAdapterError::VerificationMismatch {
            field: key,
            expected: expected.to_owned(),
            actual: actual.cloned(),
        }),
    }
}

fn verify_optional_label(
    labels: &BTreeMap<String, String>,
    key: &'static str,
    expected: Option<&str>,
) -> Result<(), BollardAdapterError> {
    match expected {
        Some(expected) => verify_label(labels, key, expected),
        None if labels.contains_key(key) => Err(BollardAdapterError::VerificationMismatch {
            field: key,
            expected: "<absent>".to_owned(),
            actual: labels.get(key).cloned(),
        }),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};

    use bollard::models::{
        ContainerConfig, ContainerState, ContainerSummaryStateEnum, HostConfig, ImageConfig,
        MountPoint, PortBinding,
    };

    use super::*;
    use crate::{DockerEnvironment, DockerSocketProbe};

    const FIRST_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SECOND_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const IMAGE_ID: &str =
        "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    #[derive(Clone)]
    struct FakeApi {
        state: Arc<Mutex<FakeState>>,
        delay: Duration,
    }

    struct FakeState {
        requests: Vec<BollardApiRequest>,
        responses: VecDeque<Result<BollardApiResponse, BollardApiError>>,
    }

    impl FakeApi {
        fn new(responses: impl IntoIterator<Item = BollardApiResponse>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    requests: Vec::new(),
                    responses: responses.into_iter().map(Ok).collect(),
                })),
                delay: Duration::ZERO,
            }
        }

        fn failing(message: &str) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    requests: Vec::new(),
                    responses: VecDeque::from([Err(BollardApiError {
                        message: message.to_owned(),
                    })]),
                })),
                delay: Duration::ZERO,
            }
        }

        fn requests(&self) -> Vec<BollardApiRequest> {
            self.state.lock().expect("fake lock").requests.clone()
        }
    }

    impl BollardApi for FakeApi {
        async fn execute(
            &self,
            request: BollardApiRequest,
        ) -> Result<BollardApiResponse, BollardApiError> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let mut state = self.state.lock().expect("fake lock");
            state.requests.push(request);
            state.responses.pop_front().expect("fake response")
        }
    }

    struct Environment(OsString);

    impl DockerEnvironment for Environment {
        fn docker_host(&self) -> Option<OsString> {
            Some(self.0.clone())
        }
        fn docker_context(&self) -> Option<OsString> {
            None
        }
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
        fn runtime_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    struct Socket(PathBuf);

    impl DockerSocketProbe for Socket {
        fn is_unix_socket(&self, path: &Path) -> bool {
            path == self.0
        }
    }

    fn endpoint() -> DockerEndpoint {
        let path = PathBuf::from("/tmp/cdenv-bollard-test.sock");
        DockerEndpoint::resolve_with_probe(
            &Environment(OsString::from("unix:///tmp/cdenv-bollard-test.sock")),
            &Socket(path),
        )
        .expect("test endpoint")
    }

    fn identities() -> (InstallationId, WorkspaceName, ProfileId, GenerationId) {
        (
            InstallationId::parse("installation").expect("installation"),
            WorkspaceName::parse("workspace").expect("workspace"),
            ProfileId::parse("cdenv-devcontainer-v1").expect("profile"),
            GenerationId::new(2).expect("generation"),
        )
    }

    fn labels() -> HashMap<String, String> {
        HashMap::from([
            (INSTALLATION_LABEL.to_owned(), "installation".to_owned()),
            (WORKSPACE_LABEL.to_owned(), "workspace".to_owned()),
            (GENERATION_LABEL.to_owned(), "2".to_owned()),
            (PROFILE_LABEL.to_owned(), "cdenv-devcontainer-v1".to_owned()),
            (GENERATED_IMAGE_LABEL.to_owned(), "true".to_owned()),
            (COMPOSE_PROJECT_LABEL.to_owned(), "project".to_owned()),
            (COMPOSE_SERVICE_LABEL.to_owned(), "service".to_owned()),
        ])
    }

    fn summary(id: &str, workspace: &str, generation: &str) -> ContainerSummary {
        let mut labels = labels();
        labels.insert(WORKSPACE_LABEL.to_owned(), workspace.to_owned());
        labels.insert(GENERATION_LABEL.to_owned(), generation.to_owned());
        ContainerSummary {
            id: Some(id.to_owned()),
            names: Some(vec![format!("/{workspace}")]),
            image_id: Some(IMAGE_ID.to_owned()),
            labels: Some(labels),
            state: Some(ContainerSummaryStateEnum::RUNNING),
            ..ContainerSummary::default()
        }
    }

    fn raw_container(running: bool) -> ContainerInspectResponse {
        ContainerInspectResponse {
            id: Some(FIRST_ID.to_owned()),
            name: Some("/owned-name".to_owned()),
            image: Some(IMAGE_ID.to_owned()),
            config: Some(ContainerConfig {
                image: Some("example:tag".to_owned()),
                labels: Some(labels()),
                user: Some("developer".to_owned()),
                working_dir: Some("/workspaces/project".to_owned()),
                ..ContainerConfig::default()
            }),
            state: Some(ContainerState {
                running: Some(running),
                ..ContainerState::default()
            }),
            mounts: Some(vec![MountPoint {
                typ: Some("bind".to_owned()),
                source: Some("/source".to_owned()),
                destination: Some("/workspaces/project".to_owned()),
                mode: Some("rw".to_owned()),
                ..MountPoint::default()
            }]),
            host_config: Some(HostConfig {
                port_bindings: Some(HashMap::from([(
                    "3000/tcp".to_owned(),
                    Some(vec![PortBinding {
                        host_ip: Some("127.0.0.1".to_owned()),
                        host_port: Some("3000".to_owned()),
                    }]),
                )])),
                ..HostConfig::default()
            }),
            ..ContainerInspectResponse::default()
        }
    }

    fn raw_image(architecture: &str) -> ImageInspect {
        ImageInspect {
            id: Some(IMAGE_ID.to_owned()),
            architecture: Some(architecture.to_owned()),
            config: Some(ImageConfig {
                labels: Some(labels()),
                ..ImageConfig::default()
            }),
            ..ImageInspect::default()
        }
    }

    fn expectation<'a>(
        id: &'a ContainerId,
        image_id: &'a ImageId,
        installation: &'a InstallationId,
        workspace: &'a WorkspaceName,
        profile: &'a ProfileId,
        generation: GenerationId,
    ) -> ContainerExpectation<'a> {
        ContainerExpectation {
            id,
            name: "owned-name",
            image_id,
            installation,
            workspace,
            generation,
            profile,
            project: Some("project"),
            service: Some("service"),
            running: Some(false),
        }
    }

    #[test]
    fn adapter_errors_are_send_sync_and_static() {
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}

        assert_error::<BollardAdapterError>();
    }

    #[tokio::test]
    async fn ping_requires_exact_ok_and_obeys_the_api_timeout() {
        let api = FakeApi::new([BollardApiResponse::Ping("OK".to_owned())]);
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        adapter.ping().await.expect("exact ping");

        let slow = FakeApi {
            delay: Duration::from_millis(50),
            ..FakeApi::new([BollardApiResponse::Ping("OK".to_owned())])
        };
        let adapter = BollardAdapter::with_api(endpoint(), slow, Duration::from_millis(1));
        let error = adapter.ping().await.expect_err("slow ping");

        assert!(matches!(
            error,
            BollardAdapterError::TimedOut {
                operation: "ping",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn discover_sends_all_and_exact_label_filters() {
        let api = FakeApi::new([BollardApiResponse::Containers(Vec::new())]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let (installation, workspace, _, generation) = identities();

        adapter
            .discover(ContainerDiscoveryScope {
                installation: &installation,
                workspace: Some(&workspace),
                generation: Some(generation),
            })
            .await
            .expect("discovery");

        assert_eq!(
            api.requests(),
            [BollardApiRequest::ListContainers {
                all: true,
                filters: BTreeMap::from([(
                    "label".to_owned(),
                    vec![
                        "cdenv.installation=installation".to_owned(),
                        "cdenv.workspace=workspace".to_owned(),
                        "cdenv.generation=2".to_owned(),
                    ]
                )]),
            }]
        );
    }

    #[tokio::test]
    async fn installation_discovery_correlates_many_workspaces_with_one_call_and_preserves_matches()
    {
        let api = FakeApi::new([BollardApiResponse::Containers(vec![
            summary(FIRST_ID, "workspace", "2"),
            summary(SECOND_ID, "workspace", "2"),
            summary(
                "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
                "workspace",
                "1",
            ),
        ])]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let (installation, workspace, _, generation) = identities();
        let other = WorkspaceName::parse("other").expect("other workspace");
        let recorded = ContainerId::parse(FIRST_ID).expect("recorded ID");
        let containers = adapter
            .discover(ContainerDiscoveryScope {
                installation: &installation,
                workspace: None,
                generation: None,
            })
            .await
            .expect("installation discovery");

        let correlated = correlate_containers(
            &containers,
            &[
                WorkspaceCorrelation {
                    workspace: &workspace,
                    generation,
                    recorded_container: Some(&recorded),
                },
                WorkspaceCorrelation {
                    workspace: &other,
                    generation,
                    recorded_container: None,
                },
            ],
        );

        assert_eq!(api.requests().len(), 1);
        assert_eq!(
            (
                correlated[0].current.len(),
                correlated[0].stale.len(),
                correlated[0].external_replacements.len()
            ),
            (2, 1, 1)
        );
        assert!(correlated[0].recorded_current);
        assert!(correlated[1].current.is_empty());
    }

    #[tokio::test]
    async fn inspect_maps_container_image_labels_state_and_all_architecture_aliases() {
        let api = FakeApi::new([
            BollardApiResponse::Container(Box::new(raw_container(true))),
            BollardApiResponse::Image(Box::new(raw_image("amd64"))),
            BollardApiResponse::Image(Box::new(raw_image("x86_64"))),
            BollardApiResponse::Image(Box::new(raw_image("arm64"))),
            BollardApiResponse::Image(Box::new(raw_image("aarch64"))),
        ]);
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let id = ContainerId::parse(FIRST_ID).expect("ID");
        let container = adapter
            .inspect_container(&id)
            .await
            .expect("container inspect");
        assert_eq!(
            (
                container.name.as_str(),
                container.running,
                container.image_reference.as_deref(),
                container.user.as_str(),
                container.working_directory.as_str(),
                container.mounts.len(),
                container.ports.len(),
            ),
            (
                "owned-name",
                true,
                Some("example:tag"),
                "developer",
                "/workspaces/project",
                1,
                1,
            )
        );

        let mut architectures = Vec::new();
        for alias in ["amd64", "x86_64", "arm64", "aarch64"] {
            architectures.push(
                adapter
                    .inspect_image(alias)
                    .await
                    .expect("image inspect")
                    .architecture,
            );
        }
        assert_eq!(
            architectures,
            [
                ContainerArchitecture::X86_64,
                ContainerArchitecture::X86_64,
                ContainerArchitecture::Aarch64,
                ContainerArchitecture::Aarch64,
            ]
        );
    }

    #[test]
    fn verification_rejects_every_identity_project_service_and_state_mismatch() {
        let (installation, workspace, profile, generation) = identities();
        let id = ContainerId::parse(FIRST_ID).expect("ID");
        let image_id = ImageId::parse(IMAGE_ID).expect("image ID");
        let expected = expectation(
            &id,
            &image_id,
            &installation,
            &workspace,
            &profile,
            generation,
        );
        let baseline = map_container(raw_container(false)).expect("inspection");
        BollardAdapter::<FakeApi>::verify_container(&baseline, expected).expect("baseline");

        let mut mismatches = Vec::new();
        let mut changed = baseline.clone();
        changed.id = ContainerId::parse(SECOND_ID).expect("ID");
        mismatches.push(changed);
        let mut changed = baseline.clone();
        changed.name = "other".to_owned();
        mismatches.push(changed);
        let mut changed = baseline.clone();
        changed.image_id = ImageId::parse(
            "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
        )
        .expect("different image ID");
        mismatches.push(changed);
        for key in [
            INSTALLATION_LABEL,
            WORKSPACE_LABEL,
            GENERATION_LABEL,
            PROFILE_LABEL,
            COMPOSE_PROJECT_LABEL,
            COMPOSE_SERVICE_LABEL,
        ] {
            let mut changed = baseline.clone();
            changed.labels.insert(key.to_owned(), "other".to_owned());
            mismatches.push(changed);
        }
        let mut changed = baseline;
        changed.running = true;
        mismatches.push(changed);

        assert!(mismatches.into_iter().all(|inspection| {
            matches!(
                BollardAdapter::<FakeApi>::verify_container(&inspection, expected),
                Err(BollardAdapterError::VerificationMismatch { .. })
            )
        }));
    }

    #[tokio::test]
    async fn control_requests_are_narrow_and_api_failures_remain_precise() {
        let api = FakeApi::new([
            BollardApiResponse::Unit,
            BollardApiResponse::Unit,
            BollardApiResponse::Unit,
            BollardApiResponse::Unit,
        ]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let id = ContainerId::parse(FIRST_ID).expect("ID");
        adapter.start(&id).await.expect("start");
        adapter
            .stop(&id, Duration::from_secs(7))
            .await
            .expect("stop");
        adapter.rename(&id, "backup").await.expect("rename");
        adapter
            .upload_archive(&id, "/opt/cdenv", b"tar")
            .await
            .expect("upload");

        assert_eq!(
            api.requests(),
            [
                BollardApiRequest::StartContainer {
                    id: FIRST_ID.to_owned()
                },
                BollardApiRequest::StopContainer {
                    id: FIRST_ID.to_owned(),
                    seconds: 7
                },
                BollardApiRequest::RenameContainer {
                    id: FIRST_ID.to_owned(),
                    name: "backup".to_owned()
                },
                BollardApiRequest::UploadArchive {
                    id: FIRST_ID.to_owned(),
                    path: "/opt/cdenv".to_owned(),
                    archive: b"tar".to_vec()
                },
            ]
        );

        let failing = BollardAdapter::with_api(
            endpoint(),
            FakeApi::failing("denied"),
            Duration::from_secs(1),
        );
        let error = failing.start(&id).await.expect_err("control failure");
        assert!(matches!(
            error,
            BollardAdapterError::Api {
                operation: "start container",
                ..
            }
        ));

        let slow = FakeApi {
            delay: Duration::from_millis(50),
            ..FakeApi::new([BollardApiResponse::Unit])
        };
        let adapter = BollardAdapter::with_api(endpoint(), slow, Duration::from_millis(1));
        let error = adapter.start(&id).await.expect_err("control timeout");
        assert!(matches!(
            error,
            BollardAdapterError::TimedOut {
                operation: "start container",
                ..
            }
        ));
    }

    #[tokio::test]
    async fn container_cleanup_removes_only_stopped_exactly_labeled_operation_claim() {
        let (installation, workspace, profile, generation) = identities();
        let id = ContainerId::parse(FIRST_ID).expect("ID");
        let image_id = ImageId::parse(IMAGE_ID).expect("image ID");
        let api = FakeApi::new([
            BollardApiResponse::Container(Box::new(raw_container(false))),
            BollardApiResponse::Unit,
        ]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        adapter
            .cleanup_container(expectation(
                &id,
                &image_id,
                &installation,
                &workspace,
                &profile,
                generation,
            ))
            .await
            .expect("cleanup");
        assert!(matches!(
            api.requests().as_slice(),
            [
                BollardApiRequest::InspectContainer { .. },
                BollardApiRequest::RemoveContainer { .. }
            ]
        ));

        let mut wrong = raw_container(false);
        wrong
            .config
            .as_mut()
            .expect("config")
            .labels
            .as_mut()
            .expect("labels")
            .insert(WORKSPACE_LABEL.to_owned(), "external".to_owned());
        let api = FakeApi::new([BollardApiResponse::Container(Box::new(wrong))]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        adapter
            .cleanup_container(expectation(
                &id,
                &image_id,
                &installation,
                &workspace,
                &profile,
                generation,
            ))
            .await
            .expect_err("wrong labels");
        assert_eq!(api.requests().len(), 1);
    }

    #[tokio::test]
    async fn image_cleanup_requires_exact_id_and_every_generation_label() {
        let (installation, workspace, profile, generation) = identities();
        let image_id = ImageId::parse(IMAGE_ID).expect("image ID");
        let expected = ImageCleanupExpectation {
            id: &image_id,
            installation: &installation,
            workspace: &workspace,
            generation,
            profile: &profile,
        };
        let api = FakeApi::new([
            BollardApiResponse::Image(Box::new(raw_image("amd64"))),
            BollardApiResponse::Unit,
        ]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        adapter
            .cleanup_image(expected)
            .await
            .expect("image cleanup");
        assert!(matches!(
            api.requests().as_slice(),
            [
                BollardApiRequest::InspectImage { .. },
                BollardApiRequest::RemoveImage { .. }
            ]
        ));

        let mut wrong = raw_image("amd64");
        wrong
            .config
            .as_mut()
            .expect("config")
            .labels
            .as_mut()
            .expect("labels")
            .remove(GENERATION_LABEL);
        let api = FakeApi::new([BollardApiResponse::Image(Box::new(wrong))]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        adapter
            .cleanup_image(expected)
            .await
            .expect_err("missing generation");
        assert_eq!(api.requests().len(), 1);

        let mut not_generated = raw_image("amd64");
        not_generated
            .config
            .as_mut()
            .expect("config")
            .labels
            .as_mut()
            .expect("labels")
            .remove(GENERATED_IMAGE_LABEL);
        let api = FakeApi::new([BollardApiResponse::Image(Box::new(not_generated))]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        adapter
            .cleanup_image(expected)
            .await
            .expect_err("not generated by cdenv");
        assert_eq!(api.requests().len(), 1);
    }

    #[tokio::test]
    async fn unsupported_image_architecture_fails_clearly() {
        let api = FakeApi::new([BollardApiResponse::Image(Box::new(raw_image("riscv64")))]);
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let error = adapter
            .inspect_image(IMAGE_ID)
            .await
            .expect_err("unsupported architecture");
        assert!(matches!(
            error,
            BollardAdapterError::UnsupportedArchitecture(_)
        ));
    }
}
