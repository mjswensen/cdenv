//! Host end of the authenticated, bounded workspace credential bridge.

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use cdenv_core::credential_broker::{
    BROKER_FRAME_HEADER_BYTES, BROKER_HANDSHAKE_TIMEOUT, BROKER_IDLE_TIMEOUT,
    BROKER_OPERATION_TIMEOUT, BrokerFrame, BrokerFrameKind, BrokerProtocolError,
    CredentialLeaseIdentity, MAX_BROKER_HELPERS, MAX_BROKER_QUEUED_BYTES, MAX_BROKER_QUEUED_FRAMES,
    MAX_BROKER_STREAMS,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio::task::JoinSet;

/// Boxed secret-bearing backend future, used only at the narrow handler boundary.
pub type BrokerBackendFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, BrokerBackendError>> + Send + 'a>>;

/// Async byte stream returned only for a previously approved SSH-agent capability.
pub trait BrokerByteStream: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> BrokerByteStream for T {}

/// Narrow host backend surface. It cannot receive commands, paths, environment, or config.
pub trait CredentialBrokerBackend: Send + Sync + 'static {
    /// Rechecks current authority at dispatch and immediately before release.
    fn is_authorized(&self, identity: &CredentialLeaseIdentity, operation: BrokerFrameKind)
    -> bool;
    /// Returns the current monotonic policy epoch for targeted queued-result suppression.
    ///
    /// Backends with live policy should override this. The default preserves the
    /// immutable-lease behavior used by simple and test backends.
    fn authorization_epoch(&self, _operation: BrokerFrameKind) -> u64 {
        0
    }
    /// Handles one bounded raw Git lookup body over private memory/pipes.
    fn credential_lookup(&self, body: Vec<u8>) -> BrokerBackendFuture<'_, Vec<u8>>;
    /// Connects one independently approved host SSH-agent stream.
    fn connect_agent(&self) -> BrokerBackendFuture<'_, Pin<Box<dyn BrokerByteStream>>>;
    /// Records a bounded in-flight agent operation timeout as a value-free health fact.
    fn agent_operation_timed_out(&self) {}
    /// Returns bounded, nonsecret author-identity metadata.
    fn identity_metadata(&self) -> BrokerBackendFuture<'_, Vec<u8>>;
}

/// Value-free backend rejection safe for protocol diagnostics.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("credential backend is unavailable")]
pub struct BrokerBackendError;

/// Host broker transport failure. It never contains request or response bodies.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HostCredentialBrokerError {
    /// Protocol or admission failure.
    #[error(transparent)]
    Protocol(#[from] BrokerProtocolError),
    /// Private transport failed.
    #[error("credential broker private transport failed")]
    Io,
    /// Handshake, idle, or backend operation timed out.
    #[error("credential broker operation timed out")]
    Timeout,
    /// Backend queue, helper, or stream admission was saturated.
    #[error("credential broker admission limit reached")]
    Saturated,
    /// Agent hello did not echo the host-selected identity exactly.
    #[error("credential broker authenticated identity mismatch")]
    Identity,
}

/// Serves a bridge after authenticating the exact host-selected lease identity.
///
/// EOF/cancellation aborts and joins all operation-owned tasks. Unknown operations fail closed,
/// and backend errors produce only an empty value-free error frame while leaving the transport
/// and unrelated streams alive.
///
/// # Errors
///
/// Returns identity, framing, bound, timeout, saturation, or private I/O failures.
#[expect(
    clippy::too_many_lines,
    reason = "the closed broker operation set stays visible in one fail-closed dispatch loop"
)]
pub async fn serve_host_credential_broker<S, B>(
    stream: S,
    expected: CredentialLeaseIdentity,
    backend: Arc<B>,
) -> Result<(), HostCredentialBrokerError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    B: CredentialBrokerBackend,
{
    expected.validate()?;
    let (mut reader, mut writer) = tokio::io::split(stream);
    write_frame(
        &mut writer,
        &BrokerFrame::new(BrokerFrameKind::Hello, 0, expected.encode()?)?,
    )
    .await?;
    let ack = tokio::time::timeout(BROKER_HANDSHAKE_TIMEOUT, read_frame(&mut reader))
        .await
        .map_err(|_| HostCredentialBrokerError::Timeout)??
        .ok_or(BrokerProtocolError::Truncated)?;
    if ack.kind != BrokerFrameKind::HelloAck
        || CredentialLeaseIdentity::decode(ack.payload())? != expected
    {
        return Err(HostCredentialBrokerError::Identity);
    }

    let (outgoing_tx, mut outgoing_rx) = mpsc::channel::<QueuedFrame>(MAX_BROKER_QUEUED_FRAMES);
    let byte_admission = Arc::new(Semaphore::new(MAX_BROKER_QUEUED_BYTES));
    let stream_admission = Arc::new(Semaphore::new(MAX_BROKER_STREAMS));
    let helper_admission = Arc::new(Semaphore::new(MAX_BROKER_HELPERS));
    let agent_inputs = Arc::new(Mutex::new(HashMap::<u32, mpsc::Sender<BrokerFrame>>::new()));
    let mut tasks = JoinSet::new();

    let result = loop {
        tokio::select! {
            Some(completed) = tasks.join_next(), if !tasks.is_empty() => {
                if completed.is_err() { break Err(HostCredentialBrokerError::Io); }
            }
            queued = outgoing_rx.recv() => {
                let Some(queued) = queued else { break Ok(()); };
                if let Some((operation, epoch)) = queued.authorization
                    && (backend.authorization_epoch(operation) != epoch
                        || !backend.is_authorized(&expected, operation))
                {
                    let kind = if operation == BrokerFrameKind::AgentOpen {
                        BrokerFrameKind::StreamClose
                    } else {
                        BrokerFrameKind::Error
                    };
                    let denied = BrokerFrame::new(kind, queued.frame.stream_id, Vec::new())?;
                    write_frame(&mut writer, &denied).await?;
                    continue;
                }
                write_frame(&mut writer, &queued.frame).await?;
            }
            incoming = tokio::time::timeout(BROKER_IDLE_TIMEOUT, read_frame(&mut reader)) => {
                let frame = incoming.map_err(|_| HostCredentialBrokerError::Timeout)??;
                let Some(frame) = frame else { break Ok(()); };
                match frame.kind {
                    BrokerFrameKind::CredentialLookup => {
                        if !backend.is_authorized(&expected, BrokerFrameKind::CredentialLookup) { send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?; continue; }
                        let permit = helper_admission.clone().try_acquire_owned().map_err(|_| HostCredentialBrokerError::Saturated)?;
                        let epoch = backend.authorization_epoch(BrokerFrameKind::CredentialLookup);
                        let backend = backend.clone(); let expected = expected.clone(); let outgoing = outgoing_tx.clone(); let bytes = byte_admission.clone();
                        let stream_id = frame.stream_id;
                        tasks.spawn(async move {
                            let _permit = permit;
                            let lookup = tokio::time::timeout(BROKER_OPERATION_TIMEOUT, backend.credential_lookup(frame.into_payload()));
                            tokio::pin!(lookup);
                            let response = tokio::select! {
                                result = &mut lookup => match result {
                                    Ok(Ok(payload)) if backend.is_authorized(&expected, BrokerFrameKind::CredentialLookup) => BrokerFrame::new(BrokerFrameKind::CredentialResult, stream_id, payload),
                                    _ => BrokerFrame::new(BrokerFrameKind::Error, stream_id, Vec::new()),
                                },
                                () = authority_revoked(&backend, &expected, BrokerFrameKind::CredentialLookup, epoch) => {
                                    BrokerFrame::new(BrokerFrameKind::Error, stream_id, Vec::new())
                                }
                            };
                            if let Ok(response) = response {
                                let authorization = (response.kind == BrokerFrameKind::CredentialResult)
                                    .then_some((BrokerFrameKind::CredentialLookup, epoch));
                                let _ = send_queued_scoped(&outgoing, &bytes, response, authorization).await;
                            }
                        });
                    }
                    BrokerFrameKind::AgentOpen => {
                        if !backend.is_authorized(&expected, BrokerFrameKind::AgentOpen) { send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?; continue; }
                        let Ok(permit) = stream_admission.clone().try_acquire_owned() else {
                            send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?;
                            continue;
                        };
                        let connection = tokio::time::timeout(BROKER_OPERATION_TIMEOUT, backend.connect_agent()).await;
                        let Ok(Ok(connection)) = connection else { send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?; continue; };
                        let (tx, rx) = mpsc::channel(8);
                        if agent_inputs.lock().await.insert(frame.stream_id, tx).is_some() { send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?; continue; }
                        let outgoing = outgoing_tx.clone(); let bytes = byte_admission.clone(); let inputs = agent_inputs.clone(); let stream_id = frame.stream_id;
                        let backend = backend.clone(); let identity = expected.clone();
                        tasks.spawn(async move {
                            let _permit = permit;
                            let close_outgoing = outgoing.clone();
                            let close_bytes = bytes.clone();
                            let result = relay_agent(connection, stream_id, rx, outgoing, bytes, backend, identity).await;
                            if result.is_err()
                                && let Ok(frame) = BrokerFrame::new(BrokerFrameKind::StreamClose, stream_id, Vec::new())
                            {
                                let _ = send_queued(&close_outgoing, &close_bytes, frame).await;
                            }
                            inputs.lock().await.remove(&stream_id);
                        });
                    }
                    BrokerFrameKind::StreamData | BrokerFrameKind::StreamClose | BrokerFrameKind::Cancel => {
                        let recipient = agent_inputs.lock().await.get(&frame.stream_id).cloned().ok_or(BrokerProtocolError::UnknownStream)?;
                        recipient.send(frame).await.map_err(|_| BrokerProtocolError::UnknownStream)?;
                    }
                    BrokerFrameKind::IdentityRequest => {
                        if !backend.is_authorized(&expected, BrokerFrameKind::IdentityRequest) { send_error(&outgoing_tx, &byte_admission, frame.stream_id).await?; continue; }
                        let epoch = backend.authorization_epoch(BrokerFrameKind::IdentityRequest);
                        let metadata = tokio::time::timeout(BROKER_OPERATION_TIMEOUT, backend.identity_metadata());
                        tokio::pin!(metadata);
                        let response = tokio::select! {
                            result = &mut metadata => match result {
                                Ok(Ok(payload)) if backend.is_authorized(&expected, BrokerFrameKind::IdentityRequest) => BrokerFrame::new(BrokerFrameKind::IdentityResult, frame.stream_id, payload)?,
                                _ => BrokerFrame::new(BrokerFrameKind::Error, frame.stream_id, Vec::new())?,
                            },
                            () = authority_revoked(&backend, &expected, BrokerFrameKind::IdentityRequest, epoch) => {
                                BrokerFrame::new(BrokerFrameKind::Error, frame.stream_id, Vec::new())?
                            }
                        };
                        let authorization = (response.kind == BrokerFrameKind::IdentityResult)
                            .then_some((BrokerFrameKind::IdentityRequest, epoch));
                        send_queued_scoped(&outgoing_tx, &byte_admission, response, authorization).await?;
                    }
                    BrokerFrameKind::Health => send_queued(&outgoing_tx, &byte_admission, BrokerFrame::new(BrokerFrameKind::HealthAck, 0, Vec::new())?).await?,
                    BrokerFrameKind::Stop => break Ok(()),
                    _ => break Err(BrokerProtocolError::Kind.into()),
                }
            }
        }
    };
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    result
}

async fn authority_revoked<B: CredentialBrokerBackend>(
    backend: &Arc<B>,
    identity: &CredentialLeaseIdentity,
    operation: BrokerFrameKind,
    epoch: u64,
) {
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        if backend.authorization_epoch(operation) != epoch
            || !backend.is_authorized(identity, operation)
        {
            return;
        }
    }
}

async fn relay_agent<B: CredentialBrokerBackend>(
    connection: Pin<Box<dyn BrokerByteStream>>,
    stream_id: u32,
    mut incoming: mpsc::Receiver<BrokerFrame>,
    outgoing: mpsc::Sender<QueuedFrame>,
    bytes: Arc<Semaphore>,
    backend: Arc<B>,
    identity: CredentialLeaseIdentity,
) -> Result<(), HostCredentialBrokerError> {
    let (mut agent_read, mut agent_write) = tokio::io::split(connection);
    loop {
        let frame = tokio::select! {
            frame = incoming.recv() => frame.ok_or(BrokerProtocolError::UnknownStream)?,
            () = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                if backend.is_authorized(&identity, BrokerFrameKind::AgentOpen) { continue; }
                send_queued(&outgoing, &bytes, BrokerFrame::new(BrokerFrameKind::StreamClose, stream_id, Vec::new())?).await?;
                return Ok(());
            }
        };
        match frame.kind {
            BrokerFrameKind::StreamData => {
                validate_agent_packet(frame.payload())?;
                let epoch = backend.authorization_epoch(BrokerFrameKind::AgentOpen);
                tokio::time::timeout(
                    BROKER_OPERATION_TIMEOUT,
                    agent_write.write_all(frame.payload()),
                )
                .await
                .map_err(|_| HostCredentialBrokerError::Timeout)?
                .map_err(|_| HostCredentialBrokerError::Io)?;
                let response = tokio::select! {
                    result = tokio::time::timeout(
                        BROKER_OPERATION_TIMEOUT,
                        crate::host_ssh_agent::read_agent_packet(&mut agent_read),
                    ) => if let Ok(result) = result {
                        result.map_err(|_| HostCredentialBrokerError::Io)?
                    } else {
                        backend.agent_operation_timed_out();
                        return Err(HostCredentialBrokerError::Timeout);
                    },
                    () = authority_revoked(&backend, &identity, BrokerFrameKind::AgentOpen, epoch) => {
                        send_queued(
                            &outgoing,
                            &bytes,
                            BrokerFrame::new(BrokerFrameKind::StreamClose, stream_id, Vec::new())?,
                        ).await?;
                        return Ok(());
                    }
                };
                if !backend.is_authorized(&identity, BrokerFrameKind::AgentOpen) {
                    send_queued(
                        &outgoing,
                        &bytes,
                        BrokerFrame::new(BrokerFrameKind::StreamClose, stream_id, Vec::new())?,
                    )
                    .await?;
                    return Ok(());
                }
                send_queued_scoped(
                    &outgoing,
                    &bytes,
                    BrokerFrame::new(BrokerFrameKind::StreamData, stream_id, response)?,
                    Some((BrokerFrameKind::AgentOpen, epoch)),
                )
                .await?;
            }
            BrokerFrameKind::StreamClose | BrokerFrameKind::Cancel => {
                agent_write
                    .shutdown()
                    .await
                    .map_err(|_| HostCredentialBrokerError::Io)?;
                return Ok(());
            }
            _ => return Err(BrokerProtocolError::Kind.into()),
        }
    }
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
    authorization: Option<(BrokerFrameKind, u64)>,
    _bytes: OwnedSemaphorePermit,
}
async fn send_queued(
    sender: &mpsc::Sender<QueuedFrame>,
    bytes: &Arc<Semaphore>,
    frame: BrokerFrame,
) -> Result<(), HostCredentialBrokerError> {
    send_queued_scoped(sender, bytes, frame, None).await
}

async fn send_queued_scoped(
    sender: &mpsc::Sender<QueuedFrame>,
    bytes: &Arc<Semaphore>,
    frame: BrokerFrame,
    authorization: Option<(BrokerFrameKind, u64)>,
) -> Result<(), HostCredentialBrokerError> {
    let amount =
        u32::try_from(frame.payload().len().max(1)).map_err(|_| BrokerProtocolError::Bounds)?;
    let permit = bytes
        .clone()
        .acquire_many_owned(amount)
        .await
        .map_err(|_| HostCredentialBrokerError::Saturated)?;
    sender
        .send(QueuedFrame {
            frame,
            authorization,
            _bytes: permit,
        })
        .await
        .map_err(|_| HostCredentialBrokerError::Io)
}
async fn send_error(
    sender: &mpsc::Sender<QueuedFrame>,
    bytes: &Arc<Semaphore>,
    stream_id: u32,
) -> Result<(), HostCredentialBrokerError> {
    send_queued(
        sender,
        bytes,
        BrokerFrame::new(BrokerFrameKind::Error, stream_id, Vec::new())?,
    )
    .await
}
async fn read_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<BrokerFrame>, HostCredentialBrokerError> {
    let mut header = [0_u8; BROKER_FRAME_HEADER_BYTES];
    if reader
        .read(&mut header[..1])
        .await
        .map_err(|_| HostCredentialBrokerError::Io)?
        == 0
    {
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
) -> Result<(), HostCredentialBrokerError> {
    writer
        .write_all(&frame.encode())
        .await
        .map_err(|_| HostCredentialBrokerError::Io)?;
    writer
        .flush()
        .await
        .map_err(|_| HostCredentialBrokerError::Io)
}

/// Returns capped exponential delay for a retry to the same reverified target.
#[must_use]
pub fn credential_retry_delay(attempt: usize) -> Option<std::time::Duration> {
    if attempt >= cdenv_core::credential_broker::MAX_BROKER_RETRIES {
        return None;
    }
    let multiplier = 1_u32
        .checked_shl(u32::try_from(attempt).ok()?)
        .unwrap_or(u32::MAX);
    Some(
        cdenv_core::credential_broker::BROKER_RETRY_INITIAL_BACKOFF
            .saturating_mul(multiplier)
            .min(cdenv_core::credential_broker::BROKER_RETRY_MAX_BACKOFF),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use cdenv_core::{
        AgentBuildId, ContainerId, GenerationId, InstallationId, ProtocolVersion, WorkspaceName,
    };

    #[derive(Default)]
    struct Stub;
    impl CredentialBrokerBackend for Stub {
        fn is_authorized(&self, _: &CredentialLeaseIdentity, _: BrokerFrameKind) -> bool {
            true
        }
        fn credential_lookup(&self, mut body: Vec<u8>) -> BrokerBackendFuture<'_, Vec<u8>> {
            Box::pin(async move {
                body.reverse();
                Ok(body)
            })
        }
        fn connect_agent(&self) -> BrokerBackendFuture<'_, Pin<Box<dyn BrokerByteStream>>> {
            Box::pin(async {
                let (stream, _) = tokio::io::duplex(64);
                Ok(Box::pin(stream) as Pin<Box<dyn BrokerByteStream>>)
            })
        }
        fn identity_metadata(&self) -> BrokerBackendFuture<'_, Vec<u8>> {
            Box::pin(async { Ok(b"available".to_vec()) })
        }
    }
    fn identity() -> CredentialLeaseIdentity {
        CredentialLeaseIdentity {
            installation: InstallationId::parse("install-1").expect("installation"),
            workspace: WorkspaceName::parse("project").expect("workspace"),
            workspace_receipt: "0123456789abcdef0123456789abcdef".to_owned(),
            container: ContainerId::parse(&"a".repeat(64)).expect("container"),
            generation: GenerationId::new(1).expect("generation"),
            user: cdenv_core::credential_broker::CredentialUserIdentity {
                uid: 1000,
                gid: 1000,
            },
            host_build: AgentBuildId::parse("build-1").expect("build"),
            agent_build: AgentBuildId::parse("build-1").expect("build"),
            agent_protocol: ProtocolVersion::new(1).expect("protocol"),
            broker_protocol: 1,
            grant_revision: 2,
        }
    }

    #[tokio::test]
    async fn fixture_authenticates_uses_disconnects_and_stops_without_tcp_or_ssh() {
        let expected = identity();
        let (host, mut agent) = tokio::io::duplex(1024);
        let task = tokio::spawn(serve_host_credential_broker(
            host,
            expected.clone(),
            Arc::new(Stub),
        ));
        let hello = read_frame(&mut agent).await.expect("read").expect("hello");
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::HelloAck, 0, hello.into_payload()).expect("ack"),
        )
        .await
        .expect("write");
        write_frame(
            &mut agent,
            &BrokerFrame::new(
                BrokerFrameKind::CredentialLookup,
                1,
                b"secret-marker".to_vec(),
            )
            .expect("lookup"),
        )
        .await
        .expect("write");
        let response = read_frame(&mut agent)
            .await
            .expect("read")
            .expect("response");
        assert_eq!(response.payload(), b"rekram-terces");
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::Stop, 0, Vec::new()).expect("stop"),
        )
        .await
        .expect("write");
        task.await.expect("task").expect("broker");
    }

    struct PausedBackend {
        epoch: std::sync::atomic::AtomicU64,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl CredentialBrokerBackend for PausedBackend {
        fn is_authorized(&self, _: &CredentialLeaseIdentity, operation: BrokerFrameKind) -> bool {
            operation == BrokerFrameKind::CredentialLookup
        }
        fn authorization_epoch(&self, _: BrokerFrameKind) -> u64 {
            self.epoch.load(std::sync::atomic::Ordering::SeqCst)
        }
        fn credential_lookup(&self, _: Vec<u8>) -> BrokerBackendFuture<'_, Vec<u8>> {
            Box::pin(async move {
                self.started.notify_one();
                self.release.notified().await;
                Ok(b"SECRET-MARKER".to_vec())
            })
        }
        fn connect_agent(&self) -> BrokerBackendFuture<'_, Pin<Box<dyn BrokerByteStream>>> {
            Box::pin(async { Err(BrokerBackendError) })
        }
        fn identity_metadata(&self) -> BrokerBackendFuture<'_, Vec<u8>> {
            Box::pin(async { Err(BrokerBackendError) })
        }
    }

    #[tokio::test]
    async fn queued_lookup_result_is_suppressed_when_policy_epoch_changes() {
        let expected = identity();
        let backend = Arc::new(PausedBackend {
            epoch: std::sync::atomic::AtomicU64::new(1),
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (host, mut agent) = tokio::io::duplex(1024);
        let task = tokio::spawn(serve_host_credential_broker(
            host,
            expected,
            backend.clone(),
        ));
        let hello = read_frame(&mut agent).await.expect("read").expect("hello");
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::HelloAck, 0, hello.into_payload()).expect("ack"),
        )
        .await
        .expect("write");
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::CredentialLookup, 7, b"request".to_vec())
                .expect("lookup"),
        )
        .await
        .expect("write");
        backend.started.notified().await;
        backend.epoch.store(2, std::sync::atomic::Ordering::SeqCst);
        backend.release.notify_one();
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::Health, 0, Vec::new()).expect("health"),
        )
        .await
        .expect("write");
        let denied = read_frame(&mut agent).await.expect("read").expect("denied");
        assert_eq!(denied.kind, BrokerFrameKind::Error);
        assert!(denied.payload().is_empty());
        let response = read_frame(&mut agent)
            .await
            .expect("read")
            .expect("response");
        assert_eq!(response.kind, BrokerFrameKind::HealthAck);
        write_frame(
            &mut agent,
            &BrokerFrame::new(BrokerFrameKind::Stop, 0, Vec::new()).expect("stop"),
        )
        .await
        .expect("write");
        task.await.expect("task").expect("broker");
    }

    #[test]
    fn retries_are_finite_and_capped() {
        assert_eq!(
            credential_retry_delay(0),
            Some(std::time::Duration::from_millis(100))
        );
        assert_eq!(credential_retry_delay(3), None);
    }
}
