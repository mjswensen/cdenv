//! Binary-clean host proxy from system OpenSSH to one verified container agent.

use std::future::Future;
use std::path::Path;

use cdenv_core::{ContainerId, GenerationId, InstallationId, ProfileId, WorkspaceName};
use serde::Deserialize;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

use crate::{
    ActiveGeneration, AgentArtifactProvider, AttachedExec, BollardAdapter, BollardAdapterError,
    CancellationToken, CdenvRoot, ContainerDiscoveryScope, ContainerExpectation, DockerEndpoint,
    DockerEndpointError, ExecCommand, ExecStreamError, ImageId, Installation, InstallationError,
    LockBehavior, LockError, LockGuard, LockMode, ProcessDockerEnvironment, ProvisionedState,
    WorkspaceStateError, load_workspace_state,
};

const AGENT_PROTOCOL_VERSION: u32 = 1;
const MAXIMUM_PROXY_DIAGNOSTIC_BYTES: usize = 16 * 1024;

/// Immutable, verified state needed to establish one SSH transport.
#[doc(hidden)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProxyTarget {
    installation: InstallationId,
    workspace: WorkspaceName,
    profile: ProfileId,
    generation: GenerationId,
    active: ActiveGeneration,
    state_root: String,
}

impl ProxyTarget {
    /// Exact recorded container.
    #[must_use]
    pub const fn container(&self) -> &ContainerId {
        self.active.container_id()
    }

    /// Recorded remote user.
    #[must_use]
    pub fn remote_user(&self) -> &str {
        self.active.provisioned().remote_user()
    }

    /// Recorded remote workspace folder.
    #[must_use]
    pub fn workspace_folder(&self) -> &str {
        self.active.provisioned().remote_workspace_folder()
    }

    fn provisioned(&self) -> &ProvisionedState {
        self.active.provisioned()
    }

    fn post_attach_manifest(&self) -> String {
        format!(
            "{}/lifecycle/.cdenv-post-attach-{}.json",
            self.state_root, self.generation
        )
    }

    fn host_key(&self) -> String {
        format!("{}/host_key", self.state_root)
    }

    fn authorized_key(&self) -> String {
        format!("{}/authorized_keys", self.state_root)
    }
}

/// Static runtime seam for deterministic proxy coordination tests.
#[doc(hidden)]
pub trait ProxyEngine: Send + Sync {
    /// Created attached transport retained after lifecycle-lock release.
    type Transport: Send;

    /// Verifies exact live container identity, uniqueness, labels, and running state.
    fn verify_live(
        &self,
        target: &ProxyTarget,
    ) -> impl Future<Output = Result<(), ProxyRuntimeError>> + Send;

    /// Runs the generation-owned attach hook. The container agent serializes invocations.
    fn run_post_attach<E>(
        &self,
        target: &ProxyTarget,
        diagnostics: &mut E,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<(), ProxyRuntimeError>> + Send
    where
        E: AsyncWrite + Unpin + Send;

    /// Verifies the live agent executable as the recorded user and folder.
    fn verify_agent(
        &self,
        target: &ProxyTarget,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<(), ProxyRuntimeError>> + Send;

    /// Creates the attached SSH-server Exec before lifecycle-lock release.
    fn create_transport(
        &self,
        target: &ProxyTarget,
    ) -> impl Future<Output = Result<Self::Transport, ProxyRuntimeError>> + Send;

    /// Streams one already-created attached transport without a lifecycle lock.
    fn stream<R, O, E>(
        &self,
        transport: Self::Transport,
        stdin: &mut R,
        stdout: &mut O,
        stderr: &mut E,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<(), ProxyRuntimeError>> + Send
    where
        R: AsyncRead + Unpin + Send,
        O: AsyncWrite + Unpin + Send,
        E: AsyncWrite + Unpin + Send;
}

/// Focused live-runtime failure safe for concise proxy diagnostics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProxyRuntimeError {
    /// No exact current-generation container exists.
    #[error("the recorded container is missing")]
    Missing,
    /// More than one current-generation container exists.
    #[error("the active generation is ambiguous")]
    Ambiguous,
    /// The exact recorded container was externally replaced.
    #[error("the recorded container was replaced")]
    Replaced,
    /// The exact container is not running.
    #[error("the recorded container is stopped")]
    Stopped,
    /// Labels, image, user, workspace, agent, or protocol no longer match.
    #[error("the provisioned container identity no longer matches")]
    IdentityMismatch,
    /// Docker discovery, inspection, or Exec control failed.
    #[error("Docker transport failed: {0}")]
    Docker(String),
    /// The transport-specific attach hook definitely failed.
    #[error("postAttachCommand failed; a later connection will retry")]
    PostAttachFailed,
    /// Attached stream setup or routing failed.
    #[error("SSH transport failed: {0}")]
    Stream(String),
}

/// Host proxy setup or streaming failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProxyError {
    /// Shared lifecycle coordination failed, including fail-fast contention.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Installation identity could not be loaded safely.
    #[error(transparent)]
    Installation(#[from] InstallationError),
    /// Workspace state could not be loaded safely.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// The host build identity is malformed.
    #[error("this cdenv build has an invalid agent identity")]
    HostIdentity,
    /// Persisted provision or live runtime no longer supports attachment.
    #[error("workspace `{workspace}` is unavailable ({reason}); run `cdenv up {workspace}`")]
    Unavailable {
        /// Normalized workspace name.
        workspace: WorkspaceName,
        /// Concise non-secret reason.
        #[source]
        reason: ProxyRuntimeError,
    },
}

/// Establishes and streams one proxy transport.
///
/// The lifecycle lock is fail-fast and is never retained while protocol bytes stream. The attach
/// hook runs outside that host lock, after which state and live identity are reverified under a
/// fresh shared lock before the attached Exec is created.
///
/// # Errors
///
/// Returns locking, state, installation, identity, hook, Docker, or stream failures.
pub async fn run_proxy_transport<E, R, O, D>(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    engine: &E,
    stdin: &mut R,
    stdout: &mut O,
    stderr: &mut D,
    cancellation: &CancellationToken,
) -> Result<(), ProxyError>
where
    E: ProxyEngine,
    R: AsyncRead + Unpin + Send,
    O: AsyncWrite + Unpin + Send,
    D: AsyncWrite + Unpin + Send,
{
    let initial = resolve_under_lock(root, workspace, engine).await?;
    engine
        .run_post_attach(&initial, stderr, cancellation)
        .await
        .map_err(|reason| unavailable(workspace, reason))?;

    let transport = attach_under_lock(root, workspace, &initial, engine, cancellation).await?;
    engine
        .stream(transport, stdin, stdout, stderr, cancellation)
        .await
        .map_err(|reason| unavailable(workspace, reason))
}

async fn resolve_under_lock<E: ProxyEngine>(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    engine: &E,
) -> Result<ProxyTarget, ProxyError> {
    let paths = root.workspace(workspace);
    let guard = LockGuard::acquire(&paths.lock_file(), LockMode::Shared, LockBehavior::FailFast)?;
    let target = load_target(root, workspace)?;
    engine
        .verify_live(&target)
        .await
        .map_err(|reason| unavailable(workspace, reason))?;
    guard.release()?;
    Ok(target)
}

async fn attach_under_lock<E: ProxyEngine>(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    expected: &ProxyTarget,
    engine: &E,
    cancellation: &CancellationToken,
) -> Result<E::Transport, ProxyError> {
    let paths = root.workspace(workspace);
    let guard = LockGuard::acquire(&paths.lock_file(), LockMode::Shared, LockBehavior::FailFast)?;
    let target = load_target(root, workspace)?;
    if &target != expected {
        return Err(unavailable(workspace, ProxyRuntimeError::Replaced));
    }
    engine
        .verify_live(&target)
        .await
        .map_err(|reason| unavailable(workspace, reason))?;
    engine
        .verify_agent(&target, cancellation)
        .await
        .map_err(|reason| unavailable(workspace, reason))?;
    let transport = engine
        .create_transport(&target)
        .await
        .map_err(|reason| unavailable(workspace, reason))?;
    guard.release()?;
    Ok(transport)
}

fn load_target(root: &CdenvRoot, workspace: &WorkspaceName) -> Result<ProxyTarget, ProxyError> {
    let installation = Installation::load_record_read_only(root)?;
    let state = load_workspace_state(&root.workspace(workspace).state_file())?;
    let state = state.state();
    if state.name() != workspace || state.installation_id() != installation.installation_id() {
        return Err(unavailable(workspace, ProxyRuntimeError::IdentityMismatch));
    }
    let active = state
        .active()
        .ok_or_else(|| unavailable(workspace, ProxyRuntimeError::Missing))?;
    if active.lifecycle().indeterminate() {
        return Err(unavailable(workspace, ProxyRuntimeError::IdentityMismatch));
    }
    let host_build =
        AgentArtifactProvider::embedded_identity().map_err(|_| ProxyError::HostIdentity)?;
    if active.provisioned().agent_build_id() != &host_build
        || active.provisioned().protocol_version().get() != AGENT_PROTOCOL_VERSION
    {
        return Err(unavailable(workspace, ProxyRuntimeError::IdentityMismatch));
    }
    let state_root = state_root(active.provisioned().environment_path())
        .ok_or_else(|| unavailable(workspace, ProxyRuntimeError::IdentityMismatch))?;
    Ok(ProxyTarget {
        installation: installation.installation_id().clone(),
        workspace: workspace.clone(),
        profile: state.devcontainer_profile().clone(),
        generation: active.generation(),
        active: active.clone(),
        state_root,
    })
}

fn state_root(environment_path: &str) -> Option<String> {
    let path = Path::new(environment_path);
    let environment_directory = path.parent()?;
    let root = environment_directory.parent()?;
    (path.is_absolute()
        && environment_directory.file_name()?.to_str()? == "environment"
        && root.file_name()?.to_str()? == ".cdenv"
        && root != Path::new("/")
        && !environment_path.contains('\0')
        && !root.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::CurDir
                    | std::path::Component::Prefix(_)
            )
        }))
    .then(|| root.display().to_string())
}

fn unavailable(workspace: &WorkspaceName, reason: ProxyRuntimeError) -> ProxyError {
    ProxyError::Unavailable {
        workspace: workspace.clone(),
        reason,
    }
}

struct BollardProxyEngine {
    adapter: BollardAdapter,
}

impl BollardProxyEngine {
    fn new(adapter: BollardAdapter) -> Self {
        Self { adapter }
    }

    async fn execute_capture(
        &self,
        target: &ProxyTarget,
        command: &[String],
        cancellation: &CancellationToken,
    ) -> Result<(Vec<u8>, Vec<u8>), ProxyRuntimeError> {
        let exec = self
            .adapter
            .create_attached_exec(
                &ExecCommand {
                    container: target.container(),
                    command,
                    user: Some(target.remote_user()),
                    working_directory: Some(target.workspace_folder()),
                    environment: &[],
                },
                false,
            )
            .await
            .map_err(|error| runtime_exec(&error))?;
        let mut input = tokio::io::empty();
        let mut stdout = BoundedBytes::default();
        let mut stderr = BoundedBytes::default();
        self.adapter
            .run_attached_exec(&exec, &mut input, &mut stdout, &mut stderr, cancellation)
            .await
            .map_err(|error| runtime_exec(&error))?;
        Ok((stdout.bytes, stderr.bytes))
    }
}

impl ProxyEngine for BollardProxyEngine {
    type Transport = AttachedExec;

    async fn verify_live(&self, target: &ProxyTarget) -> Result<(), ProxyRuntimeError> {
        let matches = self
            .adapter
            .discover(ContainerDiscoveryScope {
                installation: &target.installation,
                workspace: Some(&target.workspace),
                generation: Some(target.generation),
            })
            .await
            .map_err(|error| runtime_docker(&error))?;
        if matches.is_empty() {
            return Err(ProxyRuntimeError::Missing);
        }
        if matches.len() != 1 {
            return Err(ProxyRuntimeError::Ambiguous);
        }
        if matches[0].id != *target.container() {
            return Err(ProxyRuntimeError::Replaced);
        }
        if !matches[0].is_running() {
            return Err(ProxyRuntimeError::Stopped);
        }
        let inspection = self
            .adapter
            .inspect_container(target.container())
            .await
            .map_err(|error| runtime_docker(&error))?;
        let image = ImageId::parse(target.active.image_id())
            .map_err(|_| ProxyRuntimeError::IdentityMismatch)?;
        let compose = target.active.scenario().compose();
        let (project, service) = if let Some((project, managed)) = compose {
            let service = inspection
                .labels
                .get("com.docker.compose.service")
                .filter(|service| managed.contains(service))
                .ok_or(ProxyRuntimeError::IdentityMismatch)?;
            (Some(project), Some(service.as_str()))
        } else {
            (None, None)
        };
        crate::bollard::verify_container(
            &inspection,
            ContainerExpectation {
                id: target.container(),
                name: &inspection.name,
                image_id: &image,
                installation: &target.installation,
                workspace: &target.workspace,
                generation: target.generation,
                profile: &target.profile,
                project,
                service,
                running: Some(true),
            },
        )
        .map_err(|error| runtime_docker(&error))
    }

    async fn run_post_attach<E>(
        &self,
        target: &ProxyTarget,
        diagnostics: &mut E,
        cancellation: &CancellationToken,
    ) -> Result<(), ProxyRuntimeError>
    where
        E: AsyncWrite + Unpin + Send,
    {
        let command = vec![
            target.provisioned().agent_path().to_owned(),
            "post-attach".to_owned(),
            target.post_attach_manifest(),
        ];
        let exec = self
            .adapter
            .create_attached_exec(
                &ExecCommand {
                    container: target.container(),
                    command: &command,
                    user: Some(target.remote_user()),
                    working_directory: Some(target.workspace_folder()),
                    environment: &[],
                },
                false,
            )
            .await
            .map_err(|_| ProxyRuntimeError::PostAttachFailed)?;
        let mut input = tokio::io::empty();
        let mut stdout = BoundedBytes::default();
        let mut stderr = BoundedBytes::default();
        let result = self
            .adapter
            .run_attached_exec(&exec, &mut input, &mut stdout, &mut stderr, cancellation)
            .await;
        diagnostics
            .write_all(&stdout.bytes)
            .await
            .map_err(|error| ProxyRuntimeError::Stream(bounded(error.to_string())))?;
        diagnostics
            .write_all(&stderr.bytes)
            .await
            .map_err(|error| ProxyRuntimeError::Stream(bounded(error.to_string())))?;
        result.map_err(|_| ProxyRuntimeError::PostAttachFailed)
    }

    async fn verify_agent(
        &self,
        target: &ProxyTarget,
        cancellation: &CancellationToken,
    ) -> Result<(), ProxyRuntimeError> {
        let command = vec![
            target.provisioned().agent_path().to_owned(),
            "version".to_owned(),
        ];
        let (stdout, stderr) = self.execute_capture(target, &command, cancellation).await?;
        if !stderr.is_empty() {
            return Err(ProxyRuntimeError::IdentityMismatch);
        }
        let version: AgentVersion =
            serde_json::from_slice(&stdout).map_err(|_| ProxyRuntimeError::IdentityMismatch)?;
        if version.name != "cdenv-agent"
            || version.build_id != target.provisioned().agent_build_id().as_str()
            || version.protocol_version != target.provisioned().protocol_version().get()
        {
            return Err(ProxyRuntimeError::IdentityMismatch);
        }
        Ok(())
    }

    async fn create_transport(
        &self,
        target: &ProxyTarget,
    ) -> Result<Self::Transport, ProxyRuntimeError> {
        let command = vec![
            target.provisioned().agent_path().to_owned(),
            "ssh-server".to_owned(),
            "--stdio".to_owned(),
            target.host_key(),
            target.authorized_key(),
            target.provisioned().environment_path().to_owned(),
            target.workspace_folder().to_owned(),
        ];
        self.adapter
            .create_attached_exec(
                &ExecCommand {
                    container: target.container(),
                    command: &command,
                    user: Some(target.remote_user()),
                    working_directory: Some(target.workspace_folder()),
                    environment: &[],
                },
                true,
            )
            .await
            .map_err(|error| runtime_exec(&error))
    }

    async fn stream<R, O, E>(
        &self,
        transport: Self::Transport,
        stdin: &mut R,
        stdout: &mut O,
        stderr: &mut E,
        cancellation: &CancellationToken,
    ) -> Result<(), ProxyRuntimeError>
    where
        R: AsyncRead + Unpin + Send,
        O: AsyncWrite + Unpin + Send,
        E: AsyncWrite + Unpin + Send,
    {
        self.adapter
            .run_attached_exec(&transport, stdin, stdout, stderr, cancellation)
            .await
            .map_err(|error| runtime_exec(&error))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct AgentVersion {
    name: String,
    #[serde(rename = "version")]
    _version: String,
    protocol_version: u32,
    build_id: String,
}

#[derive(Default)]
struct BoundedBytes {
    bytes: Vec<u8>,
}

impl AsyncWrite for BoundedBytes {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let remaining = MAXIMUM_PROXY_DIAGNOSTIC_BYTES.saturating_sub(self.bytes.len());
        let count = remaining.min(bytes.len());
        self.bytes.extend_from_slice(&bytes[..count]);
        std::task::Poll::Ready(Ok(bytes.len()))
    }

    fn poll_flush(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: std::pin::Pin<&mut Self>,
        _context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}

fn runtime_docker(error: &BollardAdapterError) -> ProxyRuntimeError {
    ProxyRuntimeError::Docker(bounded(error.to_string()))
}

fn runtime_exec(error: &ExecStreamError) -> ProxyRuntimeError {
    ProxyRuntimeError::Stream(bounded(error.to_string()))
}

fn bounded(mut value: String) -> String {
    value = value.replace(char::is_control, " ");
    if value.len() > MAXIMUM_PROXY_DIAGNOSTIC_BYTES {
        let mut boundary = MAXIMUM_PROXY_DIAGNOSTIC_BYTES;
        while !value.is_char_boundary(boundary) {
            boundary -= 1;
        }
        value.truncate(boundary);
    }
    value
}

/// Runs the production Docker-backed proxy on process standard streams.
///
/// # Errors
///
/// Returns endpoint, connector, setup, hook, attached-stream, or final-status failures.
pub async fn run_proxy_stdio(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    cancellation: &CancellationToken,
) -> Result<(), ProxyError> {
    let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment)
        .map_err(|error| unavailable(workspace, endpoint_error(&error)))?;
    let connector = endpoint
        .bollard_connector(crate::BOLLARD_CONTROL_TIMEOUT)
        .map_err(|error| {
            unavailable(
                workspace,
                ProxyRuntimeError::Docker(bounded(error.to_string())),
            )
        })?;
    let engine = BollardProxyEngine::new(BollardAdapter::from_connector(&connector));
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();
    run_proxy_transport(
        root,
        workspace,
        &engine,
        &mut stdin,
        &mut stdout,
        &mut stderr,
        cancellation,
    )
    .await
}

fn endpoint_error(error: &DockerEndpointError) -> ProxyRuntimeError {
    ProxyRuntimeError::Docker(bounded(error.to_string()))
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, VecDeque};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use cdenv_core::{ContainerArchitecture, ProtocolVersion};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::*;
    use crate::{
        ActiveForwarding, ActiveScenario, DesiredConfigPath, FingerprintKeyState,
        LifecycleCheckpoint, PlanFingerprints, ProcessEnvironment, ProvisionedState,
        SanitizedRepositorySource, StateTimestamp, WorkspaceState, ensure_lock_file,
        ensure_private_directory, persist_workspace_state,
    };

    const CONTAINER: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Clone)]
    struct FakeEngine {
        events: Arc<Mutex<Vec<&'static str>>>,
        failures: Arc<Mutex<VecDeque<Option<ProxyRuntimeError>>>>,
        stream_lock: Option<PathBuf>,
    }

    impl FakeEngine {
        fn success() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                failures: Arc::new(Mutex::new(VecDeque::new())),
                stream_lock: None,
            }
        }

        fn event(&self, event: &'static str) -> Result<(), ProxyRuntimeError> {
            self.events.lock().expect("events").push(event);
            self.failures
                .lock()
                .expect("failures")
                .pop_front()
                .flatten()
                .map_or(Ok(()), Err)
        }
    }

    impl ProxyEngine for FakeEngine {
        type Transport = Vec<u8>;

        async fn verify_live(&self, _: &ProxyTarget) -> Result<(), ProxyRuntimeError> {
            self.event("verify")
        }

        async fn run_post_attach<E>(
            &self,
            _: &ProxyTarget,
            diagnostics: &mut E,
            _: &CancellationToken,
        ) -> Result<(), ProxyRuntimeError>
        where
            E: AsyncWrite + Unpin + Send,
        {
            diagnostics
                .write_all(b"hook-diagnostic")
                .await
                .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))?;
            self.event("attach-hook")
        }

        async fn verify_agent(
            &self,
            _: &ProxyTarget,
            _: &CancellationToken,
        ) -> Result<(), ProxyRuntimeError> {
            self.event("agent")
        }

        async fn create_transport(
            &self,
            _: &ProxyTarget,
        ) -> Result<Self::Transport, ProxyRuntimeError> {
            self.event("create")?;
            Ok(b"ssh-protocol".to_vec())
        }

        async fn stream<R, O, E>(
            &self,
            transport: Self::Transport,
            stdin: &mut R,
            stdout: &mut O,
            _stderr: &mut E,
            _: &CancellationToken,
        ) -> Result<(), ProxyRuntimeError>
        where
            R: AsyncRead + Unpin + Send,
            O: AsyncWrite + Unpin + Send,
            E: AsyncWrite + Unpin + Send,
        {
            self.event("stream")?;
            if let Some(lock) = &self.stream_lock {
                LockGuard::acquire(lock, LockMode::Exclusive, LockBehavior::FailFast)
                    .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))?
                    .release()
                    .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))?;
            }
            let mut input = Vec::new();
            stdin
                .read_to_end(&mut input)
                .await
                .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))?;
            stdout
                .write_all(&transport)
                .await
                .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))?;
            stdout
                .write_all(&input)
                .await
                .map_err(|error| ProxyRuntimeError::Stream(error.to_string()))
        }
    }

    fn fixture() -> (tempfile::TempDir, CdenvRoot, WorkspaceName) {
        let temporary = tempfile::tempdir().expect("temporary root");
        let root = CdenvRoot::resolve(Some(&temporary.path().join("cdenv")), &ProcessEnvironment)
            .expect("root");
        let name = WorkspaceName::parse("project").expect("workspace");
        ensure_private_directory(root.as_path()).expect("root directory");
        let installation = Installation::open_or_create(&root).expect("installation");
        let paths = root.workspace(&name);
        ensure_private_directory(&root.workspaces_dir()).expect("workspaces");
        ensure_private_directory(&paths.root()).expect("workspace directory");
        ensure_lock_file(&paths.lock_file()).expect("lifecycle lock");
        let FingerprintKeyState::Available(key) = installation.fingerprint_key() else {
            panic!("new installation should have a fingerprint key");
        };
        let digest = |category| key.digest_plan(category, [b"fixture".as_slice()]);
        let fingerprints = PlanFingerprints::new(
            digest(crate::PlanFingerprintCategory::Build),
            digest(crate::PlanFingerprintCategory::Create),
            digest(crate::PlanFingerprintCategory::Runtime),
        );
        let build = AgentArtifactProvider::embedded_identity().expect("build identity");
        let active = ActiveGeneration::new(
            GenerationId::new(1).expect("generation"),
            ActiveScenario::Image,
            ContainerId::parse(CONTAINER).expect("container"),
            format!("sha256:{}", "b".repeat(64)),
            fingerprints.clone(),
            BTreeMap::new(),
            LifecycleCheckpoint::new(None, None, false),
            ActiveForwarding::new(None, Vec::new(), Vec::new()),
            ProvisionedState::new(
                "dev".to_owned(),
                "/workspace/project".to_owned(),
                ContainerArchitecture::X86_64,
                "/opt/cdenv/bin/cdenv-agent".to_owned(),
                build,
                ProtocolVersion::new(1).expect("protocol"),
                "/home/dev/.cdenv/environment/1.snapshot".to_owned(),
            ),
        );
        let mut state = WorkspaceState::new(
            installation.record().installation_id().clone(),
            name.clone(),
            SanitizedRepositorySource::sanitize("source"),
            ProfileId::parse("cdenv-v1").expect("profile"),
            DesiredConfigPath::parse(".devcontainer/devcontainer.json").expect("config"),
            fingerprints,
            StateTimestamp::parse("2026-01-01T00:00:00Z").expect("timestamp"),
        );
        state.commit_active(
            active,
            StateTimestamp::parse("2026-01-01T00:00:01Z").expect("timestamp"),
        );
        persist_workspace_state(&paths.state_file(), &state).expect("state");
        (temporary, root, name)
    }

    #[tokio::test]
    async fn proxy_runs_one_hook_reverifies_creates_under_lock_and_streams_exact_bytes() {
        let (_temporary, root, name) = fixture();
        let mut engine = FakeEngine::success();
        engine.stream_lock = Some(root.workspace(&name).lock_file());
        let (mut input_write, mut input_read) = tokio::io::duplex(64);
        input_write.write_all(b"client-bytes").await.expect("input");
        input_write.shutdown().await.expect("EOF");
        let (mut output_write, mut output_read) = tokio::io::duplex(64);
        let mut stderr = tokio::io::sink();

        run_proxy_transport(
            &root,
            &name,
            &engine,
            &mut input_read,
            &mut output_write,
            &mut stderr,
            &CancellationToken::default(),
        )
        .await
        .expect("proxy transport");
        drop(output_write);
        let mut output = Vec::new();
        output_read.read_to_end(&mut output).await.expect("output");

        assert_eq!(output, b"ssh-protocolclient-bytes");
        assert_eq!(
            *engine.events.lock().expect("events"),
            [
                "verify",
                "attach-hook",
                "verify",
                "agent",
                "create",
                "stream"
            ]
        );
    }

    #[tokio::test]
    async fn every_live_resolution_failure_is_stderr_only_and_points_to_up() {
        let (_temporary, root, name) = fixture();
        for reason in [
            ProxyRuntimeError::Missing,
            ProxyRuntimeError::Ambiguous,
            ProxyRuntimeError::Replaced,
            ProxyRuntimeError::Stopped,
            ProxyRuntimeError::IdentityMismatch,
        ] {
            let engine = FakeEngine::success();
            engine
                .failures
                .lock()
                .expect("failures")
                .push_back(Some(reason));
            let mut stdin = tokio::io::empty();
            let mut stdout = VecAsyncWriter::default();
            let mut stderr = tokio::io::sink();

            let error = run_proxy_transport(
                &root,
                &name,
                &engine,
                &mut stdin,
                &mut stdout,
                &mut stderr,
                &CancellationToken::default(),
            )
            .await
            .expect_err("unavailable target should fail");

            assert!(stdout.0.is_empty());
            assert!(error.to_string().contains("cdenv up project"));
        }
    }

    #[tokio::test]
    async fn failed_hook_diagnostic_never_enters_protocol_stdout_and_later_transport_retries() {
        let (_temporary, root, name) = fixture();
        let engine = FakeEngine::success();
        engine
            .failures
            .lock()
            .expect("failures")
            .extend([None, Some(ProxyRuntimeError::PostAttachFailed)]);
        let mut stdin = tokio::io::empty();
        let mut stdout = VecAsyncWriter::default();
        let mut stderr = VecAsyncWriter::default();

        let error = run_proxy_transport(
            &root,
            &name,
            &engine,
            &mut stdin,
            &mut stdout,
            &mut stderr,
            &CancellationToken::default(),
        )
        .await
        .expect_err("failed hook should reject transport");

        assert!(matches!(
            error,
            ProxyError::Unavailable {
                reason: ProxyRuntimeError::PostAttachFailed,
                ..
            }
        ));
        assert!(stdout.0.is_empty());
        assert_eq!(stderr.0, b"hook-diagnostic");

        let retry = FakeEngine::success();
        let mut retry_stdin = tokio::io::empty();
        let mut retry_stdout = VecAsyncWriter::default();
        let mut retry_stderr = VecAsyncWriter::default();
        run_proxy_transport(
            &root,
            &name,
            &retry,
            &mut retry_stdin,
            &mut retry_stdout,
            &mut retry_stderr,
            &CancellationToken::default(),
        )
        .await
        .expect("later transport should retry hook");
        assert_eq!(retry_stderr.0, b"hook-diagnostic");
    }

    #[tokio::test]
    async fn proxy_fails_fast_on_lifecycle_contention_without_touching_stdout() {
        let (_temporary, root, name) = fixture();
        let held = LockGuard::acquire(
            &root.workspace(&name).lock_file(),
            LockMode::Exclusive,
            LockBehavior::FailFast,
        )
        .expect("exclusive lifecycle lock");
        let engine = FakeEngine::success();
        let mut stdin = tokio::io::empty();
        let mut stdout = VecAsyncWriter::default();
        let mut stderr = tokio::io::sink();

        let error = run_proxy_transport(
            &root,
            &name,
            &engine,
            &mut stdin,
            &mut stdout,
            &mut stderr,
            &CancellationToken::default(),
        )
        .await
        .expect_err("proxy should fail fast");
        drop(held);

        assert!(matches!(
            error,
            ProxyError::Lock(LockError::Contended { .. })
        ));
        assert!(stdout.0.is_empty());
        assert!(engine.events.lock().expect("events").is_empty());
    }

    #[test]
    fn proxy_reachable_sources_do_not_print_to_stdout() {
        let main = include_str!("main.rs");
        let proxy_branch = main
            .split_once("if let CliCommand::Proxy")
            .and_then(|(_, suffix)| suffix.split_once("if let CliCommand::Ssh"))
            .map(|(branch, _)| branch)
            .expect("proxy dispatch branch");
        assert!(!proxy_branch.lines().any(stdout_print_macro));
        let source = include_str!("proxy.rs")
            .rsplit_once("#[cfg(test)]\nmod tests")
            .map(|(production, _)| production)
            .expect("production proxy source");
        assert!(!source.lines().any(stdout_print_macro));
    }

    fn stdout_print_macro(line: &str) -> bool {
        let line = line.trim_start();
        line.starts_with("print!(") || line.starts_with("println!(")
    }

    #[derive(Default)]
    struct VecAsyncWriter(Vec<u8>);

    impl AsyncWrite for VecAsyncWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            bytes: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.0.extend_from_slice(bytes);
            std::task::Poll::Ready(Ok(bytes.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[test]
    fn proxy_errors_are_send_sync_and_static() {
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}
        assert_error::<ProxyError>();
        assert_error::<ProxyRuntimeError>();
    }
}
