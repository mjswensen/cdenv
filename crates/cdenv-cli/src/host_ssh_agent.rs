//! Explicitly selected, owner-verified host SSH-agent connections.
//!
//! Selection is performed only by a host mutating invocation. The resulting
//! adapter is bound to one path: container traffic can neither replace it nor
//! request another endpoint. Connections are reverified on every open so an
//! agent recreated at the same path is picked up without widening authority.

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use cdenv_core::credential_broker::{BrokerFrameKind, CredentialLeaseIdentity};
use cdenv_core::credentials::SshAgentSelector;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use crate::{BrokerBackendError, BrokerBackendFuture, BrokerByteStream, CredentialBrokerBackend};

/// Maximum complete SSH-agent packet, including its four-byte length prefix.
///
/// V1 relays every standard and extension message opaquely up to this limit.
pub const MAX_SSH_AGENT_PACKET_BYTES: usize = cdenv_core::credential_broker::MAX_BROKER_FRAME_BYTES;

/// Host launch environment consulted only when explicitly resolving `auto`.
pub trait HostSshAgentEnvironment {
    /// Returns the named host variable without consulting a shell.
    fn variable(&self, name: &str) -> Option<OsString>;
}

/// Current process environment for an explicit host mutating invocation.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessHostSshAgentEnvironment;

impl HostSshAgentEnvironment for ProcessHostSshAgentEnvironment {
    fn variable(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

/// Value-free current state of the independently enabled SSH-agent backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostSshAgentHealth {
    /// The selected endpoint accepted a connection.
    Available,
    /// `auto` had no nonempty `SSH_AUTH_SOCK` in the explicit invocation.
    Absent,
    /// The selected endpoint is not currently reachable.
    Unreachable,
    /// The selected endpoint is not a Unix socket (including a symlink).
    WrongType,
    /// The socket is not owned by the invoking host user.
    WrongOwner,
    /// A successful identity query reported no keys.
    Empty,
    /// A bounded agent operation did not complete, commonly pending host confirmation.
    ConfirmationRequired,
}

impl HostSshAgentHealth {
    /// Value-free remediation suitable for status and doctor output.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::Available => "host SSH agent is available",
            Self::Absent => "set SSH_AUTH_SOCK on the host and explicitly reconcile credentials",
            Self::Unreachable => {
                "start or repair the selected host agent, then explicitly reconcile"
            }
            Self::WrongType | Self::WrongOwner => {
                "select an owner-controlled Unix SSH-agent socket, then explicitly reconcile"
            }
            Self::Empty => "load a key into the selected host agent, then retry",
            Self::ConfirmationRequired => {
                "confirm the signing request on the host agent or hardware key, then retry"
            }
        }
    }
}

/// Failure to resolve an automatic selector. It contains no environment value.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("selected host SSH agent is unavailable: {health:?}")]
pub struct HostSshAgentSelectionError {
    /// Safe backend fact.
    pub health: HostSshAgentHealth,
}

/// A host-selected endpoint fixed for the lifetime of one reconciled adapter.
#[derive(Clone, Debug)]
pub struct SelectedHostSshAgent {
    path: PathBuf,
}

impl SelectedHostSshAgent {
    /// Resolves a persisted selector during an explicit host mutation.
    ///
    /// Explicit selectors never consult or fall back to `SSH_AUTH_SOCK`. Automatic
    /// selection reads that one variable only; callers continue to persist `auto`,
    /// not this resolved path.
    ///
    /// # Errors
    ///
    /// Returns `Absent` for missing, empty, non-Unicode, or unsafe automatic values.
    pub fn resolve_for_mutation(
        selector: &SshAgentSelector,
        environment: &impl HostSshAgentEnvironment,
    ) -> Result<Self, HostSshAgentSelectionError> {
        let path = if let Some(path) = selector.explicit_path() {
            PathBuf::from(path)
        } else {
            let value =
                environment
                    .variable("SSH_AUTH_SOCK")
                    .ok_or(HostSshAgentSelectionError {
                        health: HostSshAgentHealth::Absent,
                    })?;
            let text = value.to_str().filter(|value| !value.is_empty()).ok_or(
                HostSshAgentSelectionError {
                    health: HostSshAgentHealth::Absent,
                },
            )?;
            let parsed =
                text.parse::<SshAgentSelector>()
                    .map_err(|_| HostSshAgentSelectionError {
                        health: HostSshAgentHealth::Absent,
                    })?;
            PathBuf::from(parsed.explicit_path().ok_or(HostSshAgentSelectionError {
                health: HostSshAgentHealth::Absent,
            })?)
        };
        Ok(Self { path })
    }

    /// Borrows the host-authorized path. It must never be populated from a bridge request.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Reconnecting adapter for one exact selected host endpoint.
#[derive(Clone, Debug)]
pub struct HostSshAgentAdapter {
    selected: SelectedHostSshAgent,
    expected_uid: u32,
    health: Arc<Mutex<HostSshAgentHealth>>,
}

impl HostSshAgentAdapter {
    /// Binds an adapter to the invoking user's selected endpoint.
    #[must_use]
    pub fn new(selected: SelectedHostSshAgent) -> Self {
        Self {
            selected,
            expected_uid: nix::unistd::geteuid().as_raw(),
            health: Arc::new(Mutex::new(HostSshAgentHealth::Unreachable)),
        }
    }

    /// Returns the latest value-free connection/probe fact.
    #[must_use]
    pub fn health(&self) -> HostSshAgentHealth {
        self.health
            .lock()
            .map_or(HostSshAgentHealth::Unreachable, |health| *health)
    }

    /// Connects after checking type/owner and verifies that the pathname was not
    /// replaced during connect. Every call reopens the same authorized path.
    ///
    /// # Errors
    ///
    /// Returns only a value-free socket type, owner, absence, or reachability fact.
    pub async fn connect(&self) -> Result<UnixStream, HostSshAgentHealth> {
        let before = self.verified_metadata()?;
        let stream = UnixStream::connect(self.selected.path())
            .await
            .map_err(|_| {
                self.record(HostSshAgentHealth::Unreachable);
                HostSshAgentHealth::Unreachable
            })?;
        let after = self.verified_metadata()?;
        if before.dev() != after.dev() || before.ino() != after.ino() {
            self.record(HostSshAgentHealth::Unreachable);
            return Err(HostSshAgentHealth::Unreachable);
        }
        self.record(HostSshAgentHealth::Available);
        Ok(stream)
    }

    /// Performs the standard request-identities probe without reading key values.
    /// Responses and extension payloads are bounded and discarded.
    pub async fn probe(&self) -> HostSshAgentHealth {
        let Ok(mut stream) = self.connect().await else {
            return self.health();
        };
        // SSH2_AGENTC_REQUEST_IDENTITIES, encoded as an agent packet.
        if stream.write_all(&[0, 0, 0, 1, 11]).await.is_err() {
            self.record(HostSshAgentHealth::Unreachable);
            return HostSshAgentHealth::Unreachable;
        }
        let result = tokio::time::timeout(
            cdenv_core::credential_broker::BROKER_OPERATION_TIMEOUT,
            read_agent_packet(&mut stream),
        )
        .await;
        let health = match result {
            Ok(Ok(packet)) if packet.len() >= 9 && packet[4] == 12 => {
                let count = u32::from_be_bytes([packet[5], packet[6], packet[7], packet[8]]);
                if count == 0 {
                    HostSshAgentHealth::Empty
                } else {
                    HostSshAgentHealth::Available
                }
            }
            Err(_) | Ok(Err(_) | Ok(_)) => HostSshAgentHealth::Unreachable,
        };
        self.record(health);
        health
    }

    fn verified_metadata(&self) -> Result<fs::Metadata, HostSshAgentHealth> {
        let metadata = fs::symlink_metadata(self.selected.path()).map_err(|error| {
            let health = if error.kind() == std::io::ErrorKind::NotFound {
                HostSshAgentHealth::Absent
            } else {
                HostSshAgentHealth::Unreachable
            };
            self.record(health);
            health
        })?;
        if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
            self.record(HostSshAgentHealth::WrongType);
            return Err(HostSshAgentHealth::WrongType);
        }
        if metadata.uid() != self.expected_uid {
            self.record(HostSshAgentHealth::WrongOwner);
            return Err(HostSshAgentHealth::WrongOwner);
        }
        Ok(metadata)
    }

    fn record(&self, value: HostSshAgentHealth) {
        if let Ok(mut health) = self.health.lock() {
            *health = value;
        }
    }
}

/// Reads one complete standard SSH-agent binary packet while preserving its
/// message type and extension body verbatim.
pub(crate) async fn read_agent_packet(
    input: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Vec<u8>, BrokerBackendError> {
    let mut header = [0_u8; 4];
    input
        .read_exact(&mut header)
        .await
        .map_err(|_| BrokerBackendError)?;
    let body = u32::from_be_bytes(header) as usize;
    let total = body.checked_add(4).ok_or(BrokerBackendError)?;
    if body == 0 || total > MAX_SSH_AGENT_PACKET_BYTES {
        return Err(BrokerBackendError);
    }
    let mut packet = vec![0_u8; total];
    packet[..4].copy_from_slice(&header);
    input
        .read_exact(&mut packet[4..])
        .await
        .map_err(|_| BrokerBackendError)?;
    Ok(packet)
}

/// Live authority used by the SSH-only broker backend.
pub trait SshAgentAuthorizer: Send + Sync + 'static {
    /// Rechecks the exact lease's independent SSH-agent capability.
    fn is_agent_authorized(&self, identity: &CredentialLeaseIdentity) -> bool;
}

impl<F> SshAgentAuthorizer for F
where
    F: Fn(&CredentialLeaseIdentity) -> bool + Send + Sync + 'static,
{
    fn is_agent_authorized(&self, identity: &CredentialLeaseIdentity) -> bool {
        self(identity)
    }
}

/// SSH-only backend, intentionally unable to perform HTTPS or identity work.
pub struct AuthorizedHostSshAgentBackend<A> {
    identity: CredentialLeaseIdentity,
    authorizer: Arc<A>,
    adapter: HostSshAgentAdapter,
}

impl<A: SshAgentAuthorizer> AuthorizedHostSshAgentBackend<A> {
    /// Binds exact lease authority to one already host-resolved adapter.
    #[must_use]
    pub fn new(
        identity: CredentialLeaseIdentity,
        authorizer: Arc<A>,
        adapter: HostSshAgentAdapter,
    ) -> Self {
        Self {
            identity,
            authorizer,
            adapter,
        }
    }

    /// Returns the latest value-free backend fact.
    #[must_use]
    pub fn health(&self) -> HostSshAgentHealth {
        self.adapter.health()
    }
}

impl<A: SshAgentAuthorizer> CredentialBrokerBackend for AuthorizedHostSshAgentBackend<A> {
    fn is_authorized(
        &self,
        identity: &CredentialLeaseIdentity,
        operation: BrokerFrameKind,
    ) -> bool {
        operation == BrokerFrameKind::AgentOpen
            && &self.identity == identity
            && self.authorizer.is_agent_authorized(identity)
    }

    fn credential_lookup(&self, _body: Vec<u8>) -> BrokerBackendFuture<'_, Vec<u8>> {
        Box::pin(async { Err(BrokerBackendError) })
    }

    fn connect_agent(&self) -> BrokerBackendFuture<'_, Pin<Box<dyn BrokerByteStream>>> {
        Box::pin(async move {
            self.adapter
                .connect()
                .await
                .map(|stream| Box::pin(stream) as Pin<Box<dyn BrokerByteStream>>)
                .map_err(|_| BrokerBackendError)
        })
    }

    fn agent_operation_timed_out(&self) {
        self.adapter
            .record(HostSshAgentHealth::ConfirmationRequired);
    }

    fn identity_metadata(&self) -> BrokerBackendFuture<'_, Vec<u8>> {
        Box::pin(async { Err(BrokerBackendError) })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::os::unix::fs::symlink;

    use super::*;

    #[derive(Default)]
    struct Environment(BTreeMap<String, OsString>);
    impl HostSshAgentEnvironment for Environment {
        fn variable(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    #[test]
    fn automatic_selection_is_explicit_and_fixed_while_explicit_never_falls_back() {
        let environment = Environment(BTreeMap::from([(
            "SSH_AUTH_SOCK".to_owned(),
            OsString::from("/tmp/selected-agent"),
        )]));
        let automatic = SelectedHostSshAgent::resolve_for_mutation(
            &SshAgentSelector::automatic(),
            &environment,
        )
        .expect("automatic");
        assert_eq!(automatic.path(), Path::new("/tmp/selected-agent"));
        let explicit: SshAgentSelector = "/tmp/explicit-agent".parse().expect("selector");
        let selected =
            SelectedHostSshAgent::resolve_for_mutation(&explicit, &environment).expect("explicit");
        assert_eq!(selected.path(), Path::new("/tmp/explicit-agent"));
    }

    #[tokio::test]
    async fn wrong_type_and_symlink_endpoints_are_rejected() {
        let temporary = tempfile::tempdir().expect("temporary");
        let file = temporary.path().join("file");
        fs::write(&file, b"not a socket").expect("file");
        let adapter = HostSshAgentAdapter::new(SelectedHostSshAgent { path: file.clone() });
        assert_eq!(
            adapter.connect().await.expect_err("type"),
            HostSshAgentHealth::WrongType
        );
        let link = temporary.path().join("link");
        symlink(file, &link).expect("link");
        let adapter = HostSshAgentAdapter::new(SelectedHostSshAgent { path: link });
        assert_eq!(
            adapter.connect().await.expect_err("symlink"),
            HostSshAgentHealth::WrongType
        );
    }

    #[tokio::test]
    async fn same_path_restart_and_empty_agent_probe_are_supported() {
        let temporary = tempfile::tempdir().expect("temporary");
        let path = temporary.path().join("agent.sock");
        let selector: SshAgentSelector = path.to_str().expect("utf8").parse().expect("selector");
        let selected =
            SelectedHostSshAgent::resolve_for_mutation(&selector, &Environment::default())
                .expect("selected");
        let adapter = HostSshAgentAdapter::new(selected);
        for _ in 0..2 {
            let listener = tokio::net::UnixListener::bind(&path).expect("bind");
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.expect("accept");
                assert_eq!(
                    read_agent_packet(&mut socket).await.expect("request"),
                    [0, 0, 0, 1, 11]
                );
                socket
                    .write_all(&[0, 0, 0, 5, 12, 0, 0, 0, 0])
                    .await
                    .expect("response");
            });
            assert_eq!(adapter.probe().await, HostSshAgentHealth::Empty);
            server.await.expect("server");
            fs::remove_file(&path).expect("remove");
        }
    }

    #[tokio::test]
    async fn independent_clients_connect_concurrently_and_partial_extension_io_is_preserved() {
        let temporary = tempfile::tempdir().expect("temporary");
        let path = temporary.path().join("agent.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind");
        let selector: SshAgentSelector = path.to_str().expect("utf8").parse().expect("selector");
        let selected =
            SelectedHostSshAgent::resolve_for_mutation(&selector, &Environment::default())
                .expect("selected");
        let adapter = HostSshAgentAdapter::new(selected);
        let (first, second) = tokio::join!(adapter.connect(), adapter.connect());
        let mut clients = [first.expect("first"), second.expect("second")];
        let extension = [0, 0, 0, 5, 27, 0, 255, 1, 2];
        for client in &mut clients {
            for chunk in extension.chunks(2) {
                client.write_all(chunk).await.expect("partial write");
            }
        }
        for _ in 0..2 {
            let (mut server, _) = listener.accept().await.expect("accept");
            assert_eq!(
                read_agent_packet(&mut server).await.expect("packet"),
                extension
            );
        }
    }

    #[tokio::test]
    async fn packet_reader_preserves_extensions_and_rejects_oversize() {
        let extension = [0, 0, 0, 5, 27, 0, 255, 1, 2];
        assert_eq!(
            read_agent_packet(&mut extension.as_slice())
                .await
                .expect("packet"),
            extension
        );
        let oversized = u32::try_from(MAX_SSH_AGENT_PACKET_BYTES)
            .expect("bound")
            .to_be_bytes();
        assert!(read_agent_packet(&mut oversized.as_slice()).await.is_err());
    }
}
