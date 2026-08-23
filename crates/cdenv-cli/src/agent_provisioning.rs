//! Tool-free upload, identity discovery, provisioning, and final agent verification.

use std::collections::BTreeSet;
use std::future::Future;
use std::io;
use std::path::{Component, Path};
use std::pin::Pin;
use std::task::{Context, Poll};

use cdenv_core::{AgentBuildId, ContainerArchitecture, ContainerId, ProtocolVersion};
use getrandom::fill;
use serde::{Deserialize, Serialize};
use tar::{Builder, Header};
use thiserror::Error;
use tokio::io::AsyncWrite;

use crate::bollard::BollardApi;
use crate::{
    AgentArtifactError, AgentArtifactIdentity, AgentArtifactProvider, BollardAdapter,
    BollardAdapterError, CancellationToken, ExecCommand, ExecStreamError,
};

const STAGING_PARENTS: [&str; 2] = ["/tmp", "/var/tmp"];
const MAXIMUM_MACHINE_OUTPUT_BYTES: usize = 64 * 1024;
const AGENT_MODE: u32 = 0o555;
const MANIFEST_MODE: u32 = 0o400;

/// One private provision asset uploaded and atomically installed on every pass.
pub struct AgentProvisionAsset<'a> {
    /// Exact host bytes. They are never logged or included in errors.
    pub contents: &'a [u8],
    /// Absolute final container path outside the checkout.
    pub destination: &'a str,
    /// Exact final Unix mode.
    pub mode: u32,
}

/// Complete inputs for one always-upload provisioning pass.
pub struct AgentProvisionRequest<'a> {
    /// Independently verified running container.
    pub container: &'a ContainerId,
    /// Docker-selected Dev Container user used for identity and final verification.
    pub remote_user: &'a str,
    /// Architecture reported by authoritative container/image inspection.
    pub architecture: ContainerArchitecture,
    /// Embedded host/agent identity that final output must match.
    pub expected: &'a AgentArtifactIdentity,
    /// Container checkout folder, which no destination may enter.
    pub checkout: &'a str,
    /// Private assets installed for the effective selected user.
    pub assets: &'a [AgentProvisionAsset<'a>],
}

/// Effective selected-user identity returned by the staging agent.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RemoteAgentIdentity {
    /// Effective UID.
    pub uid: u32,
    /// Effective primary GID.
    pub gid: u32,
    /// Passwd-database home.
    pub home: String,
    /// Passwd-database shell.
    pub shell: String,
}

/// Facts safe to persist only after final executable verification succeeds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentProvisioningFacts {
    /// Effective selected-user identity.
    pub identity: RemoteAgentIdentity,
    /// Actual secure path selected by root provisioning.
    pub agent_path: String,
    /// Verified build identity.
    pub build_id: AgentBuildId,
    /// Verified protocol.
    pub protocol_version: ProtocolVersion,
}

/// Captured result of one non-TTY agent command.
#[doc(hidden)]
pub struct AgentCommandOutput {
    /// Bounded stdout bytes.
    pub stdout: Vec<u8>,
    /// Bounded stderr bytes.
    pub stderr: Vec<u8>,
}

/// Narrow static Docker seam for deterministic provisioning tests.
#[doc(hidden)]
pub trait AgentProvisioningEngine: Send + Sync {
    /// Extracts one operation-owned tar archive at an existing absolute parent.
    fn upload<'a>(
        &'a self,
        container: &'a ContainerId,
        parent: &'a str,
        archive: &'a [u8],
    ) -> impl Future<Output = Result<(), AgentProvisionTransportError>> + Send + 'a;

    /// Runs one attached, non-TTY command and captures bounded separated output.
    fn execute<'a>(
        &'a self,
        command: ExecCommand<'a>,
        input: &'a [u8],
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<AgentCommandOutput, AgentProvisionTransportError>> + Send + 'a;
}

/// Docker upload/Exec transport failure with bounded diagnostics.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentProvisionTransportError {
    /// Docker archive upload failed.
    #[error(transparent)]
    Docker(#[from] BollardAdapterError),
    /// Docker Exec failed; stderr is bounded and retained for actionable diagnostics.
    #[error("agent Exec failed: {source}; stderr: {stderr}")]
    Exec {
        /// Typed Exec failure.
        #[source]
        source: ExecStreamError,
        /// Bounded UTF-8-lossy agent diagnostic.
        stderr: String,
    },
}

impl<A: BollardApi> AgentProvisioningEngine for BollardAdapter<A> {
    async fn upload(
        &self,
        container: &ContainerId,
        parent: &str,
        archive: &[u8],
    ) -> Result<(), AgentProvisionTransportError> {
        self.upload_archive(container, parent, archive).await?;
        Ok(())
    }

    async fn execute(
        &self,
        command: ExecCommand<'_>,
        input: &[u8],
        cancellation: &CancellationToken,
    ) -> Result<AgentCommandOutput, AgentProvisionTransportError> {
        let exec = self
            .create_attached_exec(&command, !input.is_empty())
            .await
            .map_err(|source| AgentProvisionTransportError::Exec {
                source,
                stderr: String::new(),
            })?;
        let mut stdin = input;
        let mut stdout = BoundedWriter::default();
        let mut stderr = BoundedWriter::default();
        if let Err(source) = self
            .run_attached_exec(&exec, &mut stdin, &mut stdout, &mut stderr, cancellation)
            .await
        {
            return Err(AgentProvisionTransportError::Exec {
                source,
                stderr: String::from_utf8_lossy(&stderr.bytes).into_owned(),
            });
        }
        Ok(AgentCommandOutput {
            stdout: stdout.bytes,
            stderr: stderr.bytes,
        })
    }
}

/// Precise host-side provisioning failure. No method in this module writes workspace state.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentProvisioningError {
    /// Embedded artifact selection or architecture validation failed.
    #[error(transparent)]
    Artifact(#[from] AgentArtifactError),
    /// A random operation identifier could not be generated.
    #[error("cannot generate an agent staging identity: {0}")]
    Random(getrandom::Error),
    /// A staging tar archive could not be encoded.
    #[error("cannot encode the agent staging archive: {0}")]
    Archive(io::Error),
    /// An asset path or mode would weaken isolation or enter the checkout.
    #[error("invalid provision asset {field}")]
    InvalidAsset {
        /// Safe invalid field.
        field: &'static str,
    },
    /// Docker upload or command transport failed.
    #[error("agent provisioning {operation} failed: {source}")]
    Transport {
        /// Safe flow stage.
        operation: &'static str,
        /// Preserved transport failure.
        #[source]
        source: AgentProvisionTransportError,
    },
    /// Successful command output was malformed, oversized, or emitted stderr.
    #[error("agent {operation} returned invalid machine output")]
    InvalidOutput {
        /// Safe command stage.
        operation: &'static str,
    },
    /// Root provisioning selected a path outside the supplied ordered candidates.
    #[error("agent provision returned an unrecognized executable path")]
    UnexpectedAgentPath,
    /// Final version output disagreed with the embedded host identity.
    #[error("final agent {field} does not match the embedded host agent")]
    IdentityMismatch {
        /// `name`, `version`, `buildId`, or `protocolVersion`.
        field: &'static str,
    },
    /// Cancellation was observed between provisioning operations.
    #[error("agent provisioning was cancelled during {operation}")]
    Cancelled {
        /// Safe interrupted stage.
        operation: &'static str,
    },
}

/// Always-upload tool-free provisioning coordinator.
pub struct AgentProvisioner<E> {
    engine: E,
    artifacts: AgentArtifactProvider,
}

impl<E> AgentProvisioner<E> {
    /// Constructs a coordinator from a Docker seam and embedded/injected artifacts.
    #[must_use]
    pub const fn new(engine: E, artifacts: AgentArtifactProvider) -> Self {
        Self { engine, artifacts }
    }
}

impl<E: AgentProvisioningEngine> AgentProvisioner<E> {
    /// Uploads, discovers effective identity, provisions as root, and verifies final version.
    ///
    /// This method deliberately uploads on every invocation and returns facts only after final
    /// verification. It never checks an existing agent as authority and never persists state.
    ///
    /// # Errors
    ///
    /// Returns artifact, archive, asset, upload, Exec, output, path, identity, or cancellation
    /// failures.
    #[expect(
        clippy::too_many_lines,
        reason = "the security-sensitive upload and Exec sequence remains visible in exact order"
    )]
    pub async fn provision(
        &self,
        request: &AgentProvisionRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<AgentProvisioningFacts, AgentProvisioningError> {
        check_cancelled(cancellation, "staging upload")?;
        validate_request(request)?;
        let artifact = self.artifacts.artifact(request.architecture)?;
        let staging_parent = staging_parent(request.checkout)?;
        let mut random = [0_u8; 16];
        fill(&mut random).map_err(AgentProvisioningError::Random)?;
        let token = hex::encode(random);
        let stage_name = format!(".cdenv-stage-{token}");
        let stage = format!("{staging_parent}/{stage_name}");
        let staged_agent_name = format!("agent-{token}");
        let staged_agent = format!("{stage}/{staged_agent_name}");
        let initial_archive =
            staging_archive(&stage_name, &staged_agent_name, artifact, request.assets)?;
        if let Err(source) = self
            .engine
            .upload(request.container, staging_parent, &initial_archive)
            .await
        {
            self.cleanup_stage(request.container, &staged_agent, &stage)
                .await;
            return Err(AgentProvisioningError::Transport {
                operation: "staging upload",
                source,
            });
        }

        if let Err(error) = check_cancelled(cancellation, "identity") {
            self.cleanup_stage(request.container, &staged_agent, &stage)
                .await;
            return Err(error);
        }
        let identity_command = vec![staged_agent.clone(), "identity".to_owned()];
        let identity_output = match self
            .engine
            .execute(
                ExecCommand {
                    container: request.container,
                    command: &identity_command,
                    user: Some(request.remote_user),
                    working_directory: None,
                    environment: &[],
                },
                b"",
                cancellation,
            )
            .await
        {
            Ok(output) => output,
            Err(source) => {
                self.cleanup_stage(request.container, &staged_agent, &stage)
                    .await;
                return Err(AgentProvisioningError::Transport {
                    operation: "selected-user identity",
                    source,
                });
            }
        };
        let identity: RemoteAgentIdentity = match parse_machine("identity", &identity_output)
            .and_then(|identity| {
                validate_identity(&identity)?;
                Ok(identity)
            }) {
            Ok(identity) => identity,
            Err(error) => {
                self.cleanup_stage(request.container, &staged_agent, &stage)
                    .await;
                return Err(error);
            }
        };

        let destinations = executable_locations(identity.uid, request.checkout);
        let manifest = ProvisionManifest {
            build_id: request.expected.build_id().as_str(),
            protocol_version: request.expected.protocol_version().get(),
            uid: identity.uid,
            gid: identity.gid,
            staging_directory: &stage,
            agent_source: &staged_agent,
            agent_destinations: &destinations,
            files: request
                .assets
                .iter()
                .enumerate()
                .map(|(index, asset)| ManifestFile {
                    source: format!("{stage}/asset-{index}"),
                    destination: asset.destination,
                    mode: asset.mode,
                })
                .collect(),
        };
        let manifest_bytes = serde_json::to_vec(&manifest)
            .map_err(|source| AgentProvisioningError::Archive(io::Error::other(source)))?;
        let manifest_archive =
            single_file_archive("manifest.json", &manifest_bytes, MANIFEST_MODE)?;
        if let Err(source) = self
            .engine
            .upload(request.container, &stage, &manifest_archive)
            .await
        {
            self.cleanup_stage(request.container, &staged_agent, &stage)
                .await;
            return Err(AgentProvisioningError::Transport {
                operation: "manifest upload",
                source,
            });
        }

        if let Err(error) = check_cancelled(cancellation, "root provision") {
            self.cleanup_stage(request.container, &staged_agent, &stage)
                .await;
            return Err(error);
        }
        let manifest_path = format!("{stage}/manifest.json");
        let provision_command = vec![staged_agent, "provision".to_owned(), manifest_path];
        let provision_output = self
            .engine
            .execute(
                ExecCommand {
                    container: request.container,
                    command: &provision_command,
                    user: Some("0:0"),
                    working_directory: None,
                    environment: &[],
                },
                b"",
                cancellation,
            )
            .await
            .map_err(|source| AgentProvisioningError::Transport {
                operation: "root provision",
                source,
            })?;
        let provisioned: ProvisionOutput = parse_machine("provision", &provision_output)?;
        if !destinations
            .iter()
            .any(|path| path == &provisioned.agent_path)
        {
            return Err(AgentProvisioningError::UnexpectedAgentPath);
        }

        check_cancelled(cancellation, "final version verification")?;
        let version_command = vec![provisioned.agent_path.clone(), "version".to_owned()];
        let version_output = self
            .engine
            .execute(
                ExecCommand {
                    container: request.container,
                    command: &version_command,
                    user: Some(request.remote_user),
                    working_directory: None,
                    environment: &[],
                },
                b"",
                cancellation,
            )
            .await
            .map_err(|source| AgentProvisioningError::Transport {
                operation: "final version verification",
                source,
            })?;
        let version: VersionOutput = parse_machine("version", &version_output)?;
        verify_version(&version, request.expected)?;

        Ok(AgentProvisioningFacts {
            identity,
            agent_path: provisioned.agent_path,
            build_id: request.expected.build_id().clone(),
            protocol_version: request.expected.protocol_version(),
        })
    }

    async fn cleanup_stage(&self, container: &ContainerId, agent: &str, stage: &str) {
        let command = vec![
            agent.to_owned(),
            "cleanup-staging".to_owned(),
            stage.to_owned(),
        ];
        let _ = self
            .engine
            .execute(
                ExecCommand {
                    container,
                    command: &command,
                    user: Some("0:0"),
                    working_directory: None,
                    environment: &[],
                },
                b"",
                &CancellationToken::default(),
            )
            .await;
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProvisionManifest<'a> {
    build_id: &'a str,
    protocol_version: u32,
    uid: u32,
    gid: u32,
    staging_directory: &'a str,
    agent_source: &'a str,
    agent_destinations: &'a [String],
    files: Vec<ManifestFile<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestFile<'a> {
    source: String,
    destination: &'a str,
    mode: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ProvisionOutput {
    agent_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct VersionOutput {
    name: String,
    version: String,
    protocol_version: u32,
    build_id: String,
}

fn validate_request(request: &AgentProvisionRequest<'_>) -> Result<(), AgentProvisioningError> {
    if request.remote_user.is_empty() || request.remote_user.contains('\0') {
        return Err(AgentProvisioningError::InvalidAsset {
            field: "remote user",
        });
    }
    let checkout = safe_absolute(request.checkout)
        .ok_or(AgentProvisioningError::InvalidAsset { field: "checkout" })?;
    let mut destinations = BTreeSet::new();
    for asset in request.assets {
        let destination =
            safe_absolute(asset.destination).ok_or(AgentProvisioningError::InvalidAsset {
                field: "destination",
            })?;
        if !destinations.insert(destination)
            || destination.starts_with(checkout)
            || asset.mode == 0
            || asset.mode & !0o777 != 0
            || asset.mode & 0o700 == 0
        {
            return Err(AgentProvisioningError::InvalidAsset {
                field: "destination or mode",
            });
        }
    }
    Ok(())
}

fn validate_identity(identity: &RemoteAgentIdentity) -> Result<(), AgentProvisioningError> {
    if safe_absolute(&identity.home).is_none()
        || identity.home == "/"
        || identity.shell.contains('\0')
    {
        Err(AgentProvisioningError::InvalidOutput {
            operation: "identity",
        })
    } else {
        Ok(())
    }
}

fn verify_version(
    version: &VersionOutput,
    expected: &AgentArtifactIdentity,
) -> Result<(), AgentProvisioningError> {
    if version.name != "cdenv-agent" {
        return Err(AgentProvisioningError::IdentityMismatch { field: "name" });
    }
    if version.version != env!("CARGO_PKG_VERSION") {
        return Err(AgentProvisioningError::IdentityMismatch { field: "version" });
    }
    if version.build_id != expected.build_id().as_str() {
        return Err(AgentProvisioningError::IdentityMismatch { field: "buildId" });
    }
    if version.protocol_version != expected.protocol_version().get() {
        return Err(AgentProvisioningError::IdentityMismatch {
            field: "protocolVersion",
        });
    }
    Ok(())
}

fn parse_machine<T: for<'de> Deserialize<'de>>(
    operation: &'static str,
    output: &AgentCommandOutput,
) -> Result<T, AgentProvisioningError> {
    if !output.stderr.is_empty()
        || output.stdout.len() > MAXIMUM_MACHINE_OUTPUT_BYTES
        || output.stdout.is_empty()
    {
        return Err(AgentProvisioningError::InvalidOutput { operation });
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|_| AgentProvisioningError::InvalidOutput { operation })
}

/// Documented secure executable location order. The final `/tmp` candidate supports minimal,
/// read-only-root containers when `/tmp` remains executable; its root-owned child cannot be
/// replaced by the selected user under the sticky parent.
fn executable_locations(uid: u32, checkout: &str) -> Vec<String> {
    let checkout = Path::new(checkout);
    [
        "/usr/local/libexec/cdenv/cdenv-agent".to_owned(),
        "/usr/libexec/cdenv/cdenv-agent".to_owned(),
        "/opt/cdenv/bin/cdenv-agent".to_owned(),
        "/var/lib/cdenv/bin/cdenv-agent".to_owned(),
        format!("/tmp/cdenv-{uid}/bin/cdenv-agent"),
    ]
    .into_iter()
    .filter(|candidate| !Path::new(candidate).starts_with(checkout))
    .collect()
}

fn staging_parent(checkout: &str) -> Result<&'static str, AgentProvisioningError> {
    let checkout = Path::new(checkout);
    STAGING_PARENTS
        .into_iter()
        .find(|parent| !Path::new(parent).starts_with(checkout))
        .ok_or(AgentProvisioningError::InvalidAsset {
            field: "checkout leaves no isolated staging location",
        })
}

fn staging_archive(
    stage_name: &str,
    agent_name: &str,
    artifact: &[u8],
    assets: &[AgentProvisionAsset<'_>],
) -> Result<Vec<u8>, AgentProvisioningError> {
    let mut bytes = Vec::new();
    {
        let mut builder = Builder::new(&mut bytes);
        append_directory(&mut builder, stage_name, 0o755)?;
        append_file(
            &mut builder,
            &format!("{stage_name}/{agent_name}"),
            artifact,
            AGENT_MODE,
        )?;
        for (index, asset) in assets.iter().enumerate() {
            append_file(
                &mut builder,
                &format!("{stage_name}/asset-{index}"),
                asset.contents,
                0o400,
            )?;
        }
        builder.finish().map_err(AgentProvisioningError::Archive)?;
    }
    Ok(bytes)
}

fn single_file_archive(
    name: &str,
    contents: &[u8],
    mode: u32,
) -> Result<Vec<u8>, AgentProvisioningError> {
    let mut bytes = Vec::new();
    {
        let mut builder = Builder::new(&mut bytes);
        append_file(&mut builder, name, contents, mode)?;
        builder.finish().map_err(AgentProvisioningError::Archive)?;
    }
    Ok(bytes)
}

fn append_directory(
    builder: &mut Builder<&mut Vec<u8>>,
    path: &str,
    mode: u32,
) -> Result<(), AgentProvisioningError> {
    let mut header = Header::new_gnu();
    header.set_entry_type(tar::EntryType::Directory);
    header.set_size(0);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, io::empty())
        .map_err(AgentProvisioningError::Archive)
}

fn append_file(
    builder: &mut Builder<&mut Vec<u8>>,
    path: &str,
    contents: &[u8],
    mode: u32,
) -> Result<(), AgentProvisioningError> {
    let mut header = Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(mode);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, path, contents)
        .map_err(AgentProvisioningError::Archive)
}

fn safe_absolute(value: &str) -> Option<&Path> {
    let path = Path::new(value);
    (path.is_absolute()
        && !value.contains('\0')
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        }))
    .then_some(path)
}

fn check_cancelled(
    cancellation: &CancellationToken,
    operation: &'static str,
) -> Result<(), AgentProvisioningError> {
    if cancellation.is_cancelled() {
        Err(AgentProvisioningError::Cancelled { operation })
    } else {
        Ok(())
    }
}

#[derive(Default)]
struct BoundedWriter {
    bytes: Vec<u8>,
}

impl AsyncWrite for BoundedWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _context: &mut Context<'_>,
        buffer: &[u8],
    ) -> Poll<Result<usize, io::Error>> {
        if self.bytes.len().saturating_add(buffer.len()) > MAXIMUM_MACHINE_OUTPUT_BYTES {
            return Poll::Ready(Err(io::Error::other(
                "agent machine output exceeded its bound",
            )));
        }
        self.bytes.extend_from_slice(buffer);
        Poll::Ready(Ok(buffer.len()))
    }

    fn poll_flush(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _context: &mut Context<'_>,
    ) -> Poll<Result<(), io::Error>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use cdenv_core::{AgentBuildId, ProtocolVersion};

    use super::*;

    const CONTAINER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Action {
        Upload(String),
        Execute { command: Vec<String>, user: String },
    }

    #[derive(Clone)]
    struct MemoryEngine {
        actions: Arc<Mutex<Vec<Action>>>,
        outputs: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl AgentProvisioningEngine for MemoryEngine {
        async fn upload(
            &self,
            _container: &ContainerId,
            parent: &str,
            _archive: &[u8],
        ) -> Result<(), AgentProvisionTransportError> {
            self.actions
                .lock()
                .expect("actions")
                .push(Action::Upload(parent.to_owned()));
            Ok(())
        }

        async fn execute(
            &self,
            command: ExecCommand<'_>,
            _input: &[u8],
            _cancellation: &CancellationToken,
        ) -> Result<AgentCommandOutput, AgentProvisionTransportError> {
            self.actions.lock().expect("actions").push(Action::Execute {
                command: command.command.to_vec(),
                user: command.user.expect("command user").to_owned(),
            });
            let stdout = if command.command.get(1).map(String::as_str) == Some("cleanup-staging") {
                b"{}".to_vec()
            } else {
                self.outputs
                    .lock()
                    .expect("outputs")
                    .pop_front()
                    .expect("queued output")
            };
            Ok(AgentCommandOutput {
                stdout,
                stderr: Vec::new(),
            })
        }
    }

    fn elf() -> &'static [u8] {
        let mut bytes = vec![0; 64 + 56];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&(62_u16).to_le_bytes());
        bytes[32..40].copy_from_slice(&(64_u64).to_le_bytes());
        bytes[54..56].copy_from_slice(&(56_u16).to_le_bytes());
        bytes[56..58].copy_from_slice(&(1_u16).to_le_bytes());
        Box::leak(bytes.into_boxed_slice())
    }

    fn output_queue(passes: usize, build: &str) -> VecDeque<Vec<u8>> {
        let mut outputs = VecDeque::new();
        for _ in 0..passes {
            outputs.push_back(
                br#"{"uid":1000,"gid":1000,"home":"/home/dev","shell":"/bin/sh"}"#.to_vec(),
            );
            outputs.push_back(br#"{"agentPath":"/usr/local/libexec/cdenv/cdenv-agent"}"#.to_vec());
            outputs.push_back(
                format!(
                    "{{\"name\":\"cdenv-agent\",\"version\":\"0.1.0\",\"protocolVersion\":1,\"buildId\":\"{build}\"}}"
                )
                .into_bytes(),
            );
        }
        outputs
    }

    #[tokio::test]
    async fn every_successful_pass_uploads_again_and_verifies_user_path_build_and_protocol() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let engine = MemoryEngine {
            actions: actions.clone(),
            outputs: Arc::new(Mutex::new(output_queue(2, "build-1"))),
        };
        let expected = AgentArtifactIdentity::new(
            AgentBuildId::parse("build-1").expect("build ID"),
            ProtocolVersion::new(1).expect("protocol"),
        );
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let assets = [AgentProvisionAsset {
            contents: b"private",
            destination: "/home/dev/.cdenv/private-key",
            mode: 0o600,
        }];
        let request = AgentProvisionRequest {
            container: &container,
            remote_user: "dev",
            architecture: ContainerArchitecture::X86_64,
            expected: &expected,
            checkout: "/workspaces/project",
            assets: &assets,
        };
        let provisioner = AgentProvisioner::new(engine, AgentArtifactProvider::staged(elf(), &[]));

        let first = provisioner
            .provision(&request, &CancellationToken::default())
            .await
            .expect("first provisioning pass");
        let second = provisioner
            .provision(&request, &CancellationToken::default())
            .await
            .expect("second provisioning pass");

        assert_eq!(first, second);
        let actions = actions.lock().expect("actions");
        assert_eq!(
            actions
                .iter()
                .filter(|action| matches!(action, Action::Upload(_)))
                .count(),
            4
        );
        let executions = actions
            .iter()
            .filter_map(|action| match action {
                Action::Execute { command, user } => Some((command, user)),
                Action::Upload(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(executions[0].1, "dev");
        assert_eq!(executions[1].1, "0:0");
        assert_eq!(executions[2].1, "dev");
        assert_eq!(executions[2].0[0], "/usr/local/libexec/cdenv/cdenv-agent");
    }

    #[tokio::test]
    async fn wrong_final_build_fails_without_returning_provisioned_facts() {
        let engine = MemoryEngine {
            actions: Arc::new(Mutex::new(Vec::new())),
            outputs: Arc::new(Mutex::new(output_queue(1, "wrong-build"))),
        };
        let expected = AgentArtifactIdentity::new(
            AgentBuildId::parse("build-1").expect("build ID"),
            ProtocolVersion::new(1).expect("protocol"),
        );
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let request = AgentProvisionRequest {
            container: &container,
            remote_user: "1000:1000",
            architecture: ContainerArchitecture::X86_64,
            expected: &expected,
            checkout: "/workspaces/project",
            assets: &[],
        };
        let provisioner = AgentProvisioner::new(engine, AgentArtifactProvider::staged(elf(), &[]));

        let error = provisioner
            .provision(&request, &CancellationToken::default())
            .await
            .expect_err("wrong final build must fail");

        assert!(matches!(
            error,
            AgentProvisioningError::IdentityMismatch { field: "buildId" }
        ));
    }

    #[tokio::test]
    async fn malformed_identity_attempts_root_cleanup_of_operation_staging() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let engine = MemoryEngine {
            actions: actions.clone(),
            outputs: Arc::new(Mutex::new(VecDeque::from([b"not-json".to_vec()]))),
        };
        let expected = AgentArtifactIdentity::new(
            AgentBuildId::parse("build-1").expect("build ID"),
            ProtocolVersion::new(1).expect("protocol"),
        );
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let request = AgentProvisionRequest {
            container: &container,
            remote_user: "dev",
            architecture: ContainerArchitecture::X86_64,
            expected: &expected,
            checkout: "/workspaces/project",
            assets: &[],
        };
        let provisioner = AgentProvisioner::new(engine, AgentArtifactProvider::staged(elf(), &[]));

        let error = provisioner
            .provision(&request, &CancellationToken::default())
            .await
            .expect_err("malformed identity");

        assert!(matches!(
            error,
            AgentProvisioningError::InvalidOutput {
                operation: "identity"
            }
        ));
        let actions = actions.lock().expect("actions");
        assert!(matches!(
            actions.last(),
            Some(Action::Execute { command, user })
                if command.get(1).map(String::as_str) == Some("cleanup-staging") && user == "0:0"
        ));
    }

    #[tokio::test]
    async fn cancellation_before_upload_performs_no_mutation() {
        let actions = Arc::new(Mutex::new(Vec::new()));
        let engine = MemoryEngine {
            actions: actions.clone(),
            outputs: Arc::new(Mutex::new(VecDeque::new())),
        };
        let expected = AgentArtifactIdentity::new(
            AgentBuildId::parse("build-1").expect("build ID"),
            ProtocolVersion::new(1).expect("protocol"),
        );
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let request = AgentProvisionRequest {
            container: &container,
            remote_user: "dev",
            architecture: ContainerArchitecture::X86_64,
            expected: &expected,
            checkout: "/workspaces/project",
            assets: &[],
        };
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let provisioner = AgentProvisioner::new(engine, AgentArtifactProvider::staged(elf(), &[]));

        let error = provisioner
            .provision(&request, &cancellation)
            .await
            .expect_err("cancelled pass");

        assert!(matches!(error, AgentProvisioningError::Cancelled { .. }));
        assert!(actions.lock().expect("actions").is_empty());
    }
}
