//! Typed discovery, verification, and control for cdenv-owned Docker resources.

mod exec;
mod inspection;
mod mutation;

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use bollard::Docker;
use bollard::exec::{CreateExecOptions, StartExecOptions, StartExecResults};
use bollard::models::{ContainerInspectResponse, ContainerSummary, ImageInspect};
use cdenv_core::{ContainerId, UnsupportedContainerArchitecture};
use futures_util::StreamExt;
use thiserror::Error;

pub use exec::{
    AttachedExec, DetachedExec, ExecCommand, ExecId, ExecInspect, ExecStreamError,
    MAXIMUM_EXEC_FRAME_BYTES, decode_docker_multiplexed,
};
pub use inspection::{
    ComposePrimaryExpectation, ContainerDiscoveryScope, ContainerExpectation, ContainerInspection,
    CorrelatedContainers, DiscoveredContainer, ImageInspection, InspectedMount,
    InspectedPortBinding, WorkspaceCorrelation, correlate_containers, verify_container,
    verify_port_bindings,
};
pub use mutation::ImageCleanupExpectation;

#[cfg(test)]
use inspection::map_container;

use exec::{ExecApiConfiguration, ExecApiIo, ExecOutput};

use crate::{BollardConnector, DockerEndpoint};

/// Default bound applied to each Docker discovery and control API call.
pub const BOLLARD_CONTROL_TIMEOUT: Duration = Duration::from_secs(15);

pub(crate) const INSTALLATION_LABEL: &str = "cdenv.installation";
pub(crate) const WORKSPACE_LABEL: &str = "cdenv.workspace";
pub(crate) const GENERATION_LABEL: &str = "cdenv.generation";
pub(crate) const PROFILE_LABEL: &str = "cdenv.profile";
const GENERATED_IMAGE_LABEL: &str = "cdenv.generated";
pub(crate) const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";
pub(crate) const COMPOSE_SERVICE_LABEL: &str = "com.docker.compose.service";

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
    async fn execute(
        &self,
        request: BollardApiRequest,
    ) -> Result<BollardApiResponse, BollardApiError> {
        match request {
            BollardApiRequest::Ping => inspection::execute_ping(&self.client).await,
            BollardApiRequest::ListContainers { all, filters } => {
                inspection::execute_list_containers(&self.client, all, filters).await
            }
            BollardApiRequest::InspectContainer { id } => {
                inspection::execute_inspect_container(&self.client, &id).await
            }
            BollardApiRequest::InspectImage { id } => {
                inspection::execute_inspect_image(&self.client, &id).await
            }
            BollardApiRequest::StartContainer { id } => {
                mutation::execute_start_container(&self.client, &id).await
            }
            BollardApiRequest::StopContainer { id, seconds } => {
                mutation::execute_stop_container(&self.client, &id, seconds).await
            }
            BollardApiRequest::RenameContainer { id, name } => {
                mutation::execute_rename_container(&self.client, &id, &name).await
            }
            BollardApiRequest::UploadArchive { id, path, archive } => {
                mutation::execute_upload_archive(&self.client, &id, &path, archive).await
            }
            BollardApiRequest::RemoveContainer { id } => {
                mutation::execute_remove_container(&self.client, &id).await
            }
            BollardApiRequest::RemoveImage { id } => {
                mutation::execute_remove_image(&self.client, &id).await
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
    /// Docker's authoritative bindings differ from the validated create plan.
    #[error("Docker port binding verification mismatch")]
    PortBindingMismatch {
        /// Exact validated Docker publication arguments.
        expected: Vec<String>,
        /// Typed daemon bindings.
        actual: Vec<InspectedPortBinding>,
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
    use cdenv_core::{
        ContainerArchitecture, GenerationId, InstallationId, ProfileId, WorkspaceName,
    };
    use cdenv_devcontainer::{
        PortNumber, PortPlan, PortRange, PublicationBinding, PublicationProtocol,
        PublicationRequest,
    };

    use super::*;
    use crate::{DockerEnvironment, DockerSocketProbe, ImageId};

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

    fn publication(binding: PublicationBinding) -> PortPlan {
        let port = PortNumber::new(3000).expect("fixture port");
        PortPlan {
            publications: vec![PublicationRequest {
                argument: "127.0.0.1:3000:3000".to_owned(),
                binding,
                host_ports: Some(PortRange {
                    start: port,
                    end: port,
                }),
                container_ports: PortRange {
                    start: port,
                    end: port,
                },
                protocol: PublicationProtocol::Tcp,
            }],
            forwards: Vec::new(),
            warnings: Vec::new(),
        }
    }

    #[test]
    fn port_binding_verification_accepts_exact_loopback_binding() {
        let inspection = map_container(raw_container(false)).expect("container inspection");
        let plan = publication(PublicationBinding::Loopback(std::net::IpAddr::from([
            127, 0, 0, 1,
        ])));

        verify_port_bindings(&inspection, &plan).expect("exact binding");
    }

    #[test]
    fn port_binding_verification_returns_typed_mismatch_for_wrong_interface() {
        let inspection = map_container(raw_container(false)).expect("container inspection");
        let plan = publication(PublicationBinding::NonLoopback(std::net::IpAddr::from([
            0, 0, 0, 0,
        ])));

        let error = verify_port_bindings(&inspection, &plan).expect_err("binding mismatch");

        assert!(matches!(
            error,
            BollardAdapterError::PortBindingMismatch { .. }
        ));
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
    async fn invalid_rename_is_rejected_before_any_api_request() {
        let api = FakeApi::new(Vec::new());
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let id = ContainerId::parse(FIRST_ID).expect("ID");

        let error = adapter.rename(&id, "-external").await.expect_err("rename");

        assert!(matches!(error, BollardAdapterError::InvalidContainerName));
        assert!(api.requests().is_empty());
    }

    #[tokio::test]
    async fn invalid_upload_path_is_rejected_before_any_api_request() {
        let api = FakeApi::new(Vec::new());
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let id = ContainerId::parse(FIRST_ID).expect("ID");

        let error = adapter
            .upload_archive(&id, "relative", b"tar")
            .await
            .expect_err("upload");

        assert!(matches!(error, BollardAdapterError::InvalidArchivePath));
        assert!(api.requests().is_empty());
    }

    #[tokio::test]
    async fn oversized_stop_timeout_is_rejected_before_any_api_request() {
        let api = FakeApi::new(Vec::new());
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let id = ContainerId::parse(FIRST_ID).expect("ID");

        let error = adapter
            .stop(&id, Duration::from_secs(u64::MAX))
            .await
            .expect_err("stop");

        assert!(matches!(error, BollardAdapterError::InvalidStopTimeout));
        assert!(api.requests().is_empty());
    }

    #[tokio::test]
    async fn container_cleanup_refuses_running_claim_without_removal() {
        let (installation, workspace, profile, generation) = identities();
        let id = ContainerId::parse(FIRST_ID).expect("ID");
        let image_id = ImageId::parse(IMAGE_ID).expect("image ID");
        let api = FakeApi::new([BollardApiResponse::Container(Box::new(raw_container(true)))]);
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let mut expected = expectation(
            &id,
            &image_id,
            &installation,
            &workspace,
            &profile,
            generation,
        );
        expected.running = None;

        let error = adapter
            .cleanup_container(expected)
            .await
            .expect_err("running cleanup");

        assert!(matches!(
            error,
            BollardAdapterError::CleanupRunningContainer { .. }
        ));
        assert!(matches!(
            api.requests().as_slice(),
            [BollardApiRequest::InspectContainer { .. }]
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
