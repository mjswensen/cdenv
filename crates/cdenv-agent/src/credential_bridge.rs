//! Container-only credential endpoints multiplexed over protocol-only stdio.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io;
use std::io::Write as _;
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use cdenv_core::credential_broker::{
    BROKER_FRAME_HEADER_BYTES, BROKER_HANDSHAKE_TIMEOUT, BROKER_IDLE_TIMEOUT, BrokerFrame,
    BrokerFrameKind, BrokerProtocolError, CredentialLeaseIdentity, MAX_BROKER_FRAME_BYTES,
    MAX_BROKER_QUEUED_BYTES, MAX_BROKER_QUEUED_FRAMES, MAX_BROKER_STREAMS,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};

/// Stable HTTPS helper endpoint filename.
pub const CREDENTIAL_SOCKET_NAME: &str = "git-credential.sock";
/// Stable SSH-agent endpoint filename.
pub const SSH_AGENT_SOCKET_NAME: &str = "ssh-agent.sock";
const LEASE_MARKER_NAME: &str = "lease.json";

/// Paths provisioned for one selected user and generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialEndpointPaths {
    /// Private mode-0700 directory outside the checkout.
    pub runtime_directory: PathBuf,
    /// Owner-only Git credential endpoint.
    pub credential_socket: PathBuf,
    /// Owner-only SSH-agent endpoint.
    pub ssh_agent_socket: PathBuf,
}

/// Verified, managed `SSH_AUTH_SOCK` enrollment applied after generic sanitization.
///
/// Construction checks the exact owner-only endpoint. The value cannot be made
/// from inherited `SSH_` environment or an arbitrary caller-provided socket.
#[derive(Clone, Debug)]
pub struct ManagedSshAgentEnrollment {
    socket: PathBuf,
    uid: u32,
}

impl ManagedSshAgentEnrollment {
    /// Verifies the bridge-owned runtime directory and stable agent socket.
    ///
    /// # Errors
    ///
    /// Rejects a replaced directory/socket or ownership, type, and mode drift.
    pub fn verify(
        paths: &CredentialEndpointPaths,
        uid: u32,
    ) -> Result<Self, CredentialBridgeError> {
        let directory = fs::symlink_metadata(&paths.runtime_directory)
            .map_err(|_| CredentialBridgeError::UnsafePath("runtime directory unavailable"))?;
        validate_directory(&directory, uid)?;
        if paths.ssh_agent_socket.parent() != Some(paths.runtime_directory.as_path())
            || paths.ssh_agent_socket.file_name() != Some(OsStr::new(SSH_AGENT_SOCKET_NAME))
        {
            return Err(CredentialBridgeError::UnsafePath(
                "managed SSH-agent endpoint",
            ));
        }
        validate_managed_socket(&paths.ssh_agent_socket, uid)?;
        Ok(Self {
            socket: paths.ssh_agent_socket.clone(),
            uid,
        })
    }

    /// Inserts only the verified managed socket after callers sanitize snapshots.
    ///
    /// # Errors
    ///
    /// Rechecks endpoint identity properties immediately before enrollment.
    pub fn enroll_environment(
        &self,
        environment: &mut BTreeMap<OsString, OsString>,
    ) -> Result<(), CredentialBridgeError> {
        validate_managed_socket(&self.socket, self.uid)?;
        environment.insert(
            OsString::from("SSH_AUTH_SOCK"),
            self.socket.clone().into_os_string(),
        );
        Ok(())
    }

    /// Borrows the verified managed value for non-map process enrollment.
    ///
    /// # Errors
    ///
    /// Rechecks that the endpoint remains an owner-only Unix socket.
    pub fn value(&self) -> Result<&OsStr, CredentialBridgeError> {
        validate_managed_socket(&self.socket, self.uid)?;
        Ok(self.socket.as_os_str())
    }
}

/// Private endpoint or bridge failure. Errors never contain protocol payloads.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CredentialBridgeError {
    /// Runtime path is unsafe, replaced, or has unexpected ownership/type/mode.
    #[error("unsafe credential runtime path: {0}")]
    UnsafePath(&'static str),
    /// Authenticated lease does not select this effective user.
    #[error("credential lease selected-user identity mismatch")]
    User,
    /// Wire protocol failed closed.
    #[error(transparent)]
    Protocol(#[from] BrokerProtocolError),
    /// Private endpoint or stream I/O failed without exposing payload.
    #[error("credential bridge private I/O failed")]
    Io,
    /// Handshake or idle deadline elapsed.
    #[error("credential bridge timed out")]
    Timeout,
    /// Bridge queue or stream admission is saturated.
    #[error("credential bridge admission limit reached")]
    Saturated,
}

/// Prepares stable endpoints, serves until stop/EOF, and removes only exact owned resources.
///
/// The host must send an authoritative `Hello` first. The agent echoes it in `HelloAck`; the host
/// still has to compare it with the host-selected Exec target. `runtime_directory` is selected by
/// the host and must have a trusted existing parent.
///
/// # Errors
///
/// Rejects unsafe paths/identity, malformed or stalled protocol, saturation, and private I/O.
pub async fn serve_credential_bridge<S>(
    stream: S,
    runtime_directory: &Path,
) -> Result<(), CredentialBridgeError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut reader, mut writer) = tokio::io::split(stream);
    let hello = tokio::time::timeout(BROKER_HANDSHAKE_TIMEOUT, read_frame(&mut reader))
        .await
        .map_err(|_| CredentialBridgeError::Timeout)??
        .ok_or(BrokerProtocolError::Truncated)?;
    if hello.kind != BrokerFrameKind::Hello || hello.stream_id != 0 {
        return Err(BrokerProtocolError::Framing.into());
    }
    let identity = CredentialLeaseIdentity::decode(hello.payload())?;
    verify_effective_user(&identity)?;
    let paths = prepare_endpoints(runtime_directory, &identity)?;
    let cleanup = EndpointCleanup {
        paths: paths.clone(),
        identity: identity.clone(),
    };
    let credential =
        UnixListener::bind(&paths.credential_socket).map_err(|_| CredentialBridgeError::Io)?;
    let agent =
        UnixListener::bind(&paths.ssh_agent_socket).map_err(|_| CredentialBridgeError::Io)?;
    set_socket_permissions(&paths.credential_socket)?;
    set_socket_permissions(&paths.ssh_agent_socket)?;
    write_frame(
        &mut writer,
        &BrokerFrame::new(BrokerFrameKind::HelloAck, 0, identity.encode()?)?,
    )
    .await?;

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<QueuedFrame>(MAX_BROKER_QUEUED_FRAMES);
    let byte_admission = Arc::new(Semaphore::new(MAX_BROKER_QUEUED_BYTES));
    let stream_admission = Arc::new(Semaphore::new(MAX_BROKER_STREAMS));
    let recipients = Arc::new(Mutex::new(HashMap::<u32, mpsc::Sender<BrokerFrame>>::new()));
    let next_stream = Arc::new(AtomicU32::new(1));

    let credential_accept = accept_loop(
        credential,
        EndpointKind::Credential,
        outgoing_tx.clone(),
        byte_admission.clone(),
        stream_admission.clone(),
        recipients.clone(),
        next_stream.clone(),
    );
    let agent_accept = accept_loop(
        agent,
        EndpointKind::Agent,
        outgoing_tx.clone(),
        byte_admission.clone(),
        stream_admission,
        recipients.clone(),
        next_stream,
    );
    tokio::pin!(credential_accept);
    tokio::pin!(agent_accept);

    let result = loop {
        tokio::select! {
            accepted = &mut credential_accept => break accepted,
            accepted = &mut agent_accept => break accepted,
            queued = outgoing_rx.recv() => {
                let Some(queued) = queued else { break Ok(()); };
                write_frame(&mut writer, &queued.frame).await?;
            }
            incoming = tokio::time::timeout(BROKER_IDLE_TIMEOUT, read_frame(&mut reader)) => {
                let incoming = incoming.map_err(|_| CredentialBridgeError::Timeout)??;
                let Some(frame) = incoming else { break Ok(()); };
                match frame.kind {
                    BrokerFrameKind::Stop => break Ok(()),
                    BrokerFrameKind::Health if frame.stream_id == 0 => {
                        send_queued(&outgoing_tx, &byte_admission, BrokerFrame::new(BrokerFrameKind::HealthAck, 0, Vec::new())?).await?;
                    }
                    BrokerFrameKind::CredentialResult | BrokerFrameKind::StreamData | BrokerFrameKind::StreamClose | BrokerFrameKind::Error => {
                        let recipient = recipients.lock().await.get(&frame.stream_id).cloned().ok_or(BrokerProtocolError::UnknownStream)?;
                        recipient.send(frame).await.map_err(|_| BrokerProtocolError::UnknownStream)?;
                    }
                    _ => break Err(BrokerProtocolError::Kind.into()),
                }
            }
        }
    };
    drop(cleanup);
    result
}

#[derive(Clone, Copy)]
enum EndpointKind {
    Credential,
    Agent,
}

async fn accept_loop(
    listener: UnixListener,
    kind: EndpointKind,
    outgoing: mpsc::Sender<QueuedFrame>,
    bytes: Arc<Semaphore>,
    streams: Arc<Semaphore>,
    recipients: Arc<Mutex<HashMap<u32, mpsc::Sender<BrokerFrame>>>>,
    next: Arc<AtomicU32>,
) -> Result<(), CredentialBridgeError> {
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            Some(completed) = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_err() { return Err(CredentialBridgeError::Io); }
            }
            accepted = listener.accept() => {
                let (socket, _) = accepted.map_err(|_| CredentialBridgeError::Io)?;
                let Ok(permit) = streams.clone().try_acquire_owned() else {
                    drop(socket);
                    continue;
                };
                let stream_id = next.fetch_add(1, Ordering::Relaxed);
                if stream_id == 0 { return Err(BrokerProtocolError::StreamId.into()); }
                let (tx, rx) = mpsc::channel(8);
                if recipients.lock().await.insert(stream_id, tx).is_some() { return Err(BrokerProtocolError::StreamLimit.into()); }
                let outgoing = outgoing.clone();
                let bytes = bytes.clone();
                let recipients = recipients.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let cancel_outgoing = outgoing.clone();
                    let cancel_bytes = bytes.clone();
                    let result = serve_local_socket(socket, kind, stream_id, outgoing, bytes, rx).await;
                    if result.is_err()
                        && let Ok(frame) = BrokerFrame::new(BrokerFrameKind::Cancel, stream_id, Vec::new())
                    {
                        let _ = send_queued(&cancel_outgoing, &cancel_bytes, frame).await;
                    }
                    recipients.lock().await.remove(&stream_id);
                });
            }
        }
    }
}

async fn serve_local_socket(
    socket: UnixStream,
    kind: EndpointKind,
    stream_id: u32,
    outgoing: mpsc::Sender<QueuedFrame>,
    bytes: Arc<Semaphore>,
    incoming: mpsc::Receiver<BrokerFrame>,
) -> Result<(), CredentialBridgeError> {
    let (local_read, local_write) = socket.into_split();
    match kind {
        EndpointKind::Credential => {
            serve_credential_socket(
                local_read,
                local_write,
                stream_id,
                outgoing,
                bytes,
                incoming,
            )
            .await
        }
        EndpointKind::Agent => {
            serve_agent_socket(
                local_read,
                local_write,
                stream_id,
                outgoing,
                bytes,
                incoming,
            )
            .await
        }
    }
}

async fn serve_credential_socket(
    local_read: tokio::net::unix::OwnedReadHalf,
    mut local_write: tokio::net::unix::OwnedWriteHalf,
    stream_id: u32,
    outgoing: mpsc::Sender<QueuedFrame>,
    bytes: Arc<Semaphore>,
    mut incoming: mpsc::Receiver<BrokerFrame>,
) -> Result<(), CredentialBridgeError> {
    let mut payload = Vec::new();
    local_read
        .take((MAX_BROKER_FRAME_BYTES + 1) as u64)
        .read_to_end(&mut payload)
        .await
        .map_err(|_| CredentialBridgeError::Io)?;
    if payload.len() > MAX_BROKER_FRAME_BYTES {
        return Err(BrokerProtocolError::Bounds.into());
    }
    send_queued(
        &outgoing,
        &bytes,
        BrokerFrame::new(BrokerFrameKind::CredentialLookup, stream_id, payload)?,
    )
    .await?;
    let frame = incoming
        .recv()
        .await
        .ok_or(BrokerProtocolError::UnknownStream)?;
    if frame.kind == BrokerFrameKind::CredentialResult {
        local_write
            .write_all(frame.payload())
            .await
            .map_err(|_| CredentialBridgeError::Io)?;
    }
    local_write
        .shutdown()
        .await
        .map_err(|_| CredentialBridgeError::Io)
}

async fn serve_agent_socket(
    mut local_read: tokio::net::unix::OwnedReadHalf,
    mut local_write: tokio::net::unix::OwnedWriteHalf,
    stream_id: u32,
    outgoing: mpsc::Sender<QueuedFrame>,
    bytes: Arc<Semaphore>,
    mut incoming: mpsc::Receiver<BrokerFrame>,
) -> Result<(), CredentialBridgeError> {
    send_queued(
        &outgoing,
        &bytes,
        BrokerFrame::new(BrokerFrameKind::AgentOpen, stream_id, Vec::new())?,
    )
    .await?;
    loop {
        let packet = match tokio::time::timeout(
            cdenv_core::credential_broker::BROKER_OPERATION_TIMEOUT,
            read_agent_packet(&mut local_read),
        )
        .await
        {
            Ok(Ok(Some(packet))) => packet,
            Ok(Ok(None)) => {
                send_queued(
                    &outgoing,
                    &bytes,
                    BrokerFrame::new(BrokerFrameKind::StreamClose, stream_id, Vec::new())?,
                )
                .await?;
                return Ok(());
            }
            Ok(Err(error)) => return Err(error),
            Err(_) => return Err(CredentialBridgeError::Timeout),
        };
        send_queued(
            &outgoing,
            &bytes,
            BrokerFrame::new(BrokerFrameKind::StreamData, stream_id, packet)?,
        )
        .await?;
        let frame = tokio::time::timeout(
            cdenv_core::credential_broker::BROKER_OPERATION_TIMEOUT,
            incoming.recv(),
        )
        .await
        .map_err(|_| CredentialBridgeError::Timeout)?
        .ok_or(BrokerProtocolError::UnknownStream)?;
        match frame.kind {
            BrokerFrameKind::StreamData => {
                validate_agent_packet(frame.payload())?;
                tokio::time::timeout(
                    cdenv_core::credential_broker::BROKER_OPERATION_TIMEOUT,
                    local_write.write_all(frame.payload()),
                )
                .await
                .map_err(|_| CredentialBridgeError::Timeout)?
                .map_err(|_| CredentialBridgeError::Io)?;
            }
            BrokerFrameKind::StreamClose | BrokerFrameKind::Error => {
                local_write
                    .shutdown()
                    .await
                    .map_err(|_| CredentialBridgeError::Io)?;
                return Ok(());
            }
            _ => return Err(BrokerProtocolError::Kind.into()),
        }
    }
}

async fn read_agent_packet(
    input: &mut (impl AsyncRead + Unpin),
) -> Result<Option<Vec<u8>>, CredentialBridgeError> {
    let mut header = [0_u8; 4];
    let first = input
        .read(&mut header[..1])
        .await
        .map_err(|_| CredentialBridgeError::Io)?;
    if first == 0 {
        return Ok(None);
    }
    input
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| BrokerProtocolError::Truncated)?;
    let body = u32::from_be_bytes(header) as usize;
    let total = body.checked_add(4).ok_or(BrokerProtocolError::Bounds)?;
    if body == 0 || total > MAX_BROKER_FRAME_BYTES {
        return Err(BrokerProtocolError::Bounds.into());
    }
    let mut packet = vec![0_u8; total];
    packet[..4].copy_from_slice(&header);
    input
        .read_exact(&mut packet[4..])
        .await
        .map_err(|_| BrokerProtocolError::Truncated)?;
    Ok(Some(packet))
}

fn validate_agent_packet(packet: &[u8]) -> Result<(), BrokerProtocolError> {
    let header: [u8; 4] = packet
        .get(..4)
        .and_then(|bytes| bytes.try_into().ok())
        .ok_or(BrokerProtocolError::Framing)?;
    let body = u32::from_be_bytes(header) as usize;
    if body == 0 || body.checked_add(4) != Some(packet.len()) {
        return Err(BrokerProtocolError::Framing);
    }
    Ok(())
}

struct QueuedFrame {
    frame: BrokerFrame,
    _bytes: OwnedSemaphorePermit,
}

async fn send_queued(
    sender: &mpsc::Sender<QueuedFrame>,
    bytes: &Arc<Semaphore>,
    frame: BrokerFrame,
) -> Result<(), CredentialBridgeError> {
    let amount =
        u32::try_from(frame.payload().len().max(1)).map_err(|_| BrokerProtocolError::Bounds)?;
    let permit = bytes
        .clone()
        .acquire_many_owned(amount)
        .await
        .map_err(|_| CredentialBridgeError::Saturated)?;
    sender
        .send(QueuedFrame {
            frame,
            _bytes: permit,
        })
        .await
        .map_err(|_| CredentialBridgeError::Io)
}

async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<BrokerFrame>, CredentialBridgeError> {
    let mut header = [0_u8; BROKER_FRAME_HEADER_BYTES];
    let first = reader
        .read(&mut header[..1])
        .await
        .map_err(|_| CredentialBridgeError::Io)?;
    if first == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut header[1..])
        .await
        .map_err(|_| BrokerProtocolError::Truncated)?;
    let (kind, stream_id, length) = BrokerFrame::parse_header(&header)?;
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|_| BrokerProtocolError::Truncated)?;
    Ok(Some(BrokerFrame::from_parsed_header(
        kind, stream_id, payload,
    )?))
}

async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    frame: &BrokerFrame,
) -> Result<(), CredentialBridgeError> {
    writer
        .write_all(&frame.encode())
        .await
        .map_err(|_| CredentialBridgeError::Io)?;
    writer.flush().await.map_err(|_| CredentialBridgeError::Io)
}

fn verify_effective_user(identity: &CredentialLeaseIdentity) -> Result<(), CredentialBridgeError> {
    let uid = nix::unistd::geteuid().as_raw();
    let gid = nix::unistd::getegid().as_raw();
    if identity.user.uid != uid || identity.user.gid != gid {
        return Err(CredentialBridgeError::User);
    }
    if identity.agent_build.as_str() != crate::BUILD_ID
        || identity.agent_protocol.get() != crate::PROTOCOL_VERSION
    {
        return Err(BrokerProtocolError::Identity("agent compatibility").into());
    }
    Ok(())
}

fn prepare_endpoints(
    path: &Path,
    identity: &CredentialLeaseIdentity,
) -> Result<CredentialEndpointPaths, CredentialBridgeError> {
    if !path.is_absolute() {
        return Err(CredentialBridgeError::UnsafePath(
            "runtime directory is relative",
        ));
    }
    let parent = path.parent().ok_or(CredentialBridgeError::UnsafePath(
        "runtime directory has no parent",
    ))?;
    let parent_metadata = fs::symlink_metadata(parent)
        .map_err(|_| CredentialBridgeError::UnsafePath("runtime parent is unavailable"))?;
    if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
        return Err(CredentialBridgeError::UnsafePath("runtime parent type"));
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) => validate_directory(&metadata, identity.user.uid)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(path).map_err(|_| CredentialBridgeError::Io)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))
                .map_err(|_| CredentialBridgeError::Io)?;
            validate_directory(
                &fs::symlink_metadata(path).map_err(|_| CredentialBridgeError::Io)?,
                identity.user.uid,
            )?;
        }
        Err(_) => return Err(CredentialBridgeError::Io),
    }
    let marker = path.join(LEASE_MARKER_NAME);
    let encoded = identity.encode()?;
    if marker.exists() {
        let metadata = fs::symlink_metadata(&marker).map_err(|_| CredentialBridgeError::Io)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.uid() != identity.user.uid
            || metadata.permissions().mode() & 0o777 != 0o600
            || metadata.len() > (MAX_BROKER_FRAME_BYTES as u64)
        {
            return Err(CredentialBridgeError::UnsafePath("lease marker"));
        }
        let existing = fs::read(&marker).map_err(|_| CredentialBridgeError::Io)?;
        if existing != encoded {
            return Err(CredentialBridgeError::UnsafePath("stale generation"));
        }
        for endpoint in [
            path.join(CREDENTIAL_SOCKET_NAME),
            path.join(SSH_AGENT_SOCKET_NAME),
        ] {
            remove_verified_socket(&endpoint, identity.user.uid)?;
        }
    } else {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&marker)
            .map_err(|_| CredentialBridgeError::Io)?;
        file.write_all(&encoded)
            .map_err(|_| CredentialBridgeError::Io)?;
        file.sync_all().map_err(|_| CredentialBridgeError::Io)?;
    }
    Ok(CredentialEndpointPaths {
        runtime_directory: path.to_path_buf(),
        credential_socket: path.join(CREDENTIAL_SOCKET_NAME),
        ssh_agent_socket: path.join(SSH_AGENT_SOCKET_NAME),
    })
}

fn validate_directory(metadata: &fs::Metadata, uid: u32) -> Result<(), CredentialBridgeError> {
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o777 != 0o700
    {
        Err(CredentialBridgeError::UnsafePath(
            "runtime directory ownership, type, or mode",
        ))
    } else {
        Ok(())
    }
}

fn remove_verified_socket(path: &Path, uid: u32) -> Result<(), CredentialBridgeError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_socket()
                && metadata.uid() == uid
                && metadata.permissions().mode() & 0o777 == 0o600 =>
        {
            fs::remove_file(path).map_err(|_| CredentialBridgeError::Io)
        }
        Ok(_) => Err(CredentialBridgeError::UnsafePath(
            "stale endpoint replacement",
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(CredentialBridgeError::Io),
    }
}

fn set_socket_permissions(path: &Path) -> Result<(), CredentialBridgeError> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .map_err(|_| CredentialBridgeError::Io)
}

fn validate_managed_socket(path: &Path, uid: u32) -> Result<(), CredentialBridgeError> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| CredentialBridgeError::UnsafePath("managed SSH-agent endpoint unavailable"))?;
    if metadata.file_type().is_symlink()
        || !metadata.file_type().is_socket()
        || metadata.uid() != uid
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(CredentialBridgeError::UnsafePath(
            "managed SSH-agent endpoint",
        ));
    }
    Ok(())
}

struct EndpointCleanup {
    paths: CredentialEndpointPaths,
    identity: CredentialLeaseIdentity,
}
impl Drop for EndpointCleanup {
    fn drop(&mut self) {
        let marker = self.paths.runtime_directory.join(LEASE_MARKER_NAME);
        if fs::read(&marker).ok().as_deref() != self.identity.encode().ok().as_deref() {
            return;
        }
        let _ = remove_verified_socket(&self.paths.credential_socket, self.identity.user.uid);
        let _ = remove_verified_socket(&self.paths.ssh_agent_socket, self.identity.user.uid);
        let _ = fs::remove_file(marker);
        let _ = fs::remove_dir(&self.paths.runtime_directory);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdenv_core::{
        AgentBuildId, ContainerId, GenerationId, InstallationId, ProtocolVersion, WorkspaceName,
    };

    fn identity(generation: u64) -> CredentialLeaseIdentity {
        CredentialLeaseIdentity {
            installation: InstallationId::parse("install-1").expect("installation"),
            workspace: WorkspaceName::parse("project").expect("workspace"),
            workspace_receipt: "0123456789abcdef0123456789abcdef".to_owned(),
            container: ContainerId::parse(&format!("{generation:064x}")).expect("container"),
            generation: GenerationId::new(generation).expect("generation"),
            user: cdenv_core::credential_broker::CredentialUserIdentity {
                uid: nix::unistd::geteuid().as_raw(),
                gid: nix::unistd::getegid().as_raw(),
            },
            host_build: AgentBuildId::parse(crate::BUILD_ID).expect("build"),
            agent_build: AgentBuildId::parse(crate::BUILD_ID).expect("build"),
            agent_protocol: ProtocolVersion::new(1).expect("protocol"),
            broker_protocol: 1,
            grant_revision: 1,
        }
    }

    #[test]
    fn stable_reconnect_replaces_only_exact_owned_sockets_and_rejects_generation_change() {
        let temporary = tempfile::tempdir().expect("temporary");
        let runtime = temporary.path().join("credential-runtime");
        let first = identity(1);
        let paths = prepare_endpoints(&runtime, &first).expect("prepare");
        let _credential =
            std::os::unix::net::UnixListener::bind(&paths.credential_socket).expect("socket");
        fs::set_permissions(&paths.credential_socket, fs::Permissions::from_mode(0o600))
            .expect("mode");
        prepare_endpoints(&runtime, &first).expect("same lease reconnect");
        assert!(prepare_endpoints(&runtime, &identity(2)).is_err());
    }

    #[test]
    fn managed_agent_enrollment_adds_only_the_verified_socket_after_sanitization() {
        let temporary = tempfile::tempdir().expect("temporary");
        let runtime = temporary.path().join("runtime");
        let lease = identity(1);
        let paths = prepare_endpoints(&runtime, &lease).expect("prepare");
        let _agent =
            std::os::unix::net::UnixListener::bind(&paths.ssh_agent_socket).expect("agent socket");
        fs::set_permissions(&paths.ssh_agent_socket, fs::Permissions::from_mode(0o600))
            .expect("mode");
        let enrollment =
            ManagedSshAgentEnrollment::verify(&paths, lease.user.uid).expect("verified enrollment");
        let mut environment = BTreeMap::from([
            (OsString::from("KEEP"), OsString::from("value")),
            (
                OsString::from("SSH_STALE"),
                OsString::from("removed earlier"),
            ),
        ]);
        environment.retain(|name, _| !name.as_encoded_bytes().starts_with(b"SSH_"));
        enrollment
            .enroll_environment(&mut environment)
            .expect("enroll");
        assert_eq!(environment.len(), 2);
        assert_eq!(
            environment.get(OsStr::new("SSH_AUTH_SOCK")),
            Some(&paths.ssh_agent_socket.into_os_string())
        );
    }

    #[test]
    fn unsafe_symlink_and_non_socket_replacements_are_rejected() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().expect("temporary");
        let target = temporary.path().join("target");
        fs::create_dir(&target).expect("target");
        let linked = temporary.path().join("linked");
        symlink(&target, &linked).expect("link");
        assert!(prepare_endpoints(&linked, &identity(1)).is_err());
        let runtime = temporary.path().join("runtime");
        let paths = prepare_endpoints(&runtime, &identity(1)).expect("prepare");
        fs::write(&paths.credential_socket, b"replacement").expect("file");
        assert!(prepare_endpoints(&runtime, &identity(1)).is_err());
    }
}
