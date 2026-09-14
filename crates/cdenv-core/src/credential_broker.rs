//! Versioned, bounded workspace credential-broker wire protocol and lease model.
//!
//! The protocol is intentionally not an extensible command protocol. Every frame kind is
//! enumerated here and unknown kinds or versions fail closed. Payloads may contain secrets, so
//! frames have a redacted `Debug` implementation and are never serializable through Serde.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io::{self, Read, Write};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    AgentBuildId, ContainerId, GenerationId, InstallationId, ProtocolVersion, WorkspaceName,
};

/// Credential bridge wire protocol implemented by this release.
pub const CREDENTIAL_BROKER_PROTOCOL: u16 = 1;
/// Maximum payload in one broker frame.
pub const MAX_BROKER_FRAME_BYTES: usize = 64 * 1024;
/// Maximum simultaneously active multiplexed streams.
pub const MAX_BROKER_STREAMS: usize = 32;
/// Maximum frames waiting at either end of a bridge.
pub const MAX_BROKER_QUEUED_FRAMES: usize = 64;
/// Maximum aggregate payload bytes waiting at either end of a bridge.
pub const MAX_BROKER_QUEUED_BYTES: usize = 512 * 1024;
/// Maximum concurrent host helper operations.
pub const MAX_BROKER_HELPERS: usize = 4;
/// Maximum reconnect attempts to the same reverified target.
pub const MAX_BROKER_RETRIES: usize = 3;
/// Deadline for the authenticated hello exchange.
pub const BROKER_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Deadline for one backend operation.
pub const BROKER_OPERATION_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum period without a protocol health exchange.
pub const BROKER_IDLE_TIMEOUT: Duration = Duration::from_mins(1);
/// Initial reconnect delay; subsequent attempts use capped exponential backoff.
pub const BROKER_RETRY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
/// Maximum reconnect delay.
pub const BROKER_RETRY_MAX_BACKOFF: Duration = Duration::from_secs(2);

const MAGIC: &[u8; 4] = b"CDBR";
/// Bytes in the fixed frame header.
pub const BROKER_FRAME_HEADER_BYTES: usize = 16;
const HEADER_BYTES: usize = BROKER_FRAME_HEADER_BYTES;
const MAX_IDENTITY_BYTES: usize = 8 * 1024;

/// Exact host-verified authority bound to one bridge lease.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialLeaseIdentity {
    /// Stable host installation identity.
    pub installation: InstallationId,
    /// Validated workspace name.
    pub workspace: WorkspaceName,
    /// Private receipt identifying the exact workspace record.
    pub workspace_receipt: String,
    /// Exact full Docker container ID selected by the host.
    pub container: ContainerId,
    /// Container generation.
    pub generation: GenerationId,
    /// Effective numeric remote user.
    pub user: CredentialUserIdentity,
    /// Current host build.
    pub host_build: AgentBuildId,
    /// Installed agent build verified by the host.
    pub agent_build: AgentBuildId,
    /// Installed agent compatibility protocol.
    pub agent_protocol: ProtocolVersion,
    /// Credential wire protocol.
    pub broker_protocol: u16,
    /// Current positive permission revision.
    pub grant_revision: u64,
}

impl CredentialLeaseIdentity {
    /// Validates non-newtype identity invariants and release compatibility.
    ///
    /// # Errors
    ///
    /// Rejects an unsafe receipt, zero revision, unsupported protocol, or build mismatch.
    pub fn validate(&self) -> Result<(), BrokerProtocolError> {
        let receipt_valid = (16..=128).contains(&self.workspace_receipt.len())
            && self
                .workspace_receipt
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric());
        if !receipt_valid {
            return Err(BrokerProtocolError::Identity("workspace receipt"));
        }
        if self.grant_revision == 0 {
            return Err(BrokerProtocolError::Identity("grant revision"));
        }
        if self.broker_protocol != CREDENTIAL_BROKER_PROTOCOL || self.agent_protocol.get() != 1 {
            return Err(BrokerProtocolError::Version);
        }
        if self.host_build != self.agent_build {
            return Err(BrokerProtocolError::Identity("build"));
        }
        Ok(())
    }

    /// Encodes identity metadata for the hello exchange. It contains no credential payload.
    ///
    /// # Errors
    ///
    /// Returns a bounded, value-free protocol error.
    pub fn encode(&self) -> Result<Vec<u8>, BrokerProtocolError> {
        self.validate()?;
        let bytes =
            serde_json::to_vec(self).map_err(|_| BrokerProtocolError::Identity("encoding"))?;
        if bytes.len() > MAX_IDENTITY_BYTES {
            return Err(BrokerProtocolError::Bounds);
        }
        Ok(bytes)
    }

    /// Decodes and validates exact hello identity metadata.
    ///
    /// # Errors
    ///
    /// Rejects malformed, oversized, unknown-field, or incompatible metadata.
    pub fn decode(bytes: &[u8]) -> Result<Self, BrokerProtocolError> {
        if bytes.len() > MAX_IDENTITY_BYTES {
            return Err(BrokerProtocolError::Bounds);
        }
        let value = serde_json::from_slice::<Self>(bytes)
            .map_err(|_| BrokerProtocolError::Identity("encoding"))?;
        value.validate()?;
        Ok(value)
    }
}

/// Numeric selected-user identity. Names from container claims are not authority.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialUserIdentity {
    /// Effective user ID.
    pub uid: u32,
    /// Effective primary group ID.
    pub gid: u32,
}

/// Narrow wire operations; there is deliberately no execute or plugin kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BrokerFrameKind {
    /// Agent identity offered at connection start.
    Hello = 1,
    /// Host acceptance of the exact identity.
    HelloAck = 2,
    /// Bounded Git credential lookup body.
    CredentialLookup = 3,
    /// Bounded Git credential result or empty unavailable result.
    CredentialResult = 4,
    /// Opens an approved SSH-agent byte stream.
    AgentOpen = 5,
    /// Binary data for an approved stream.
    StreamData = 6,
    /// Graceful stream EOF.
    StreamClose = 7,
    /// Cancels one operation or stream.
    Cancel = 8,
    /// Requests separately authorized identity metadata.
    IdentityRequest = 9,
    /// Returns bounded identity metadata.
    IdentityResult = 10,
    /// Liveness/readiness probe.
    Health = 11,
    /// Liveness/readiness response.
    HealthAck = 12,
    /// Revokes this generation lease and terminates the bridge.
    Stop = 13,
    /// Value-free operation rejection.
    Error = 14,
}

impl TryFrom<u8> for BrokerFrameKind {
    type Error = BrokerProtocolError;

    fn try_from(value: u8) -> Result<Self, BrokerProtocolError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::HelloAck),
            3 => Ok(Self::CredentialLookup),
            4 => Ok(Self::CredentialResult),
            5 => Ok(Self::AgentOpen),
            6 => Ok(Self::StreamData),
            7 => Ok(Self::StreamClose),
            8 => Ok(Self::Cancel),
            9 => Ok(Self::IdentityRequest),
            10 => Ok(Self::IdentityResult),
            11 => Ok(Self::Health),
            12 => Ok(Self::HealthAck),
            13 => Ok(Self::Stop),
            14 => Ok(Self::Error),
            _ => Err(BrokerProtocolError::Kind),
        }
    }
}

/// One bounded multiplexed frame. Payload contents are always redacted from diagnostics.
#[derive(PartialEq, Eq)]
pub struct BrokerFrame {
    /// Typed operation.
    pub kind: BrokerFrameKind,
    /// Nonzero for operation/stream frames and zero for connection control.
    pub stream_id: u32,
    payload: Vec<u8>,
}

impl fmt::Debug for BrokerFrame {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BrokerFrame")
            .field("kind", &self.kind)
            .field("stream_id", &self.stream_id)
            .field("payload", &"[REDACTED]")
            .field("payload_bytes", &self.payload.len())
            .finish()
    }
}

impl BrokerFrame {
    /// Creates a frame after checking frame-kind, stream, and payload invariants.
    ///
    /// # Errors
    ///
    /// Rejects oversized payloads and invalid control/stream identifiers.
    pub fn new(
        kind: BrokerFrameKind,
        stream_id: u32,
        payload: Vec<u8>,
    ) -> Result<Self, BrokerProtocolError> {
        if payload.len() > MAX_BROKER_FRAME_BYTES {
            return Err(BrokerProtocolError::Bounds);
        }
        let connection_control = matches!(
            kind,
            BrokerFrameKind::Hello
                | BrokerFrameKind::HelloAck
                | BrokerFrameKind::Health
                | BrokerFrameKind::HealthAck
                | BrokerFrameKind::Stop
        );
        if connection_control != (stream_id == 0) {
            return Err(BrokerProtocolError::StreamId);
        }
        Ok(Self {
            kind,
            stream_id,
            payload,
        })
    }

    /// Borrows private payload bytes for an explicitly selected typed handler.
    #[must_use]
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Consumes the frame and returns private payload bytes.
    #[must_use]
    pub fn into_payload(self) -> Vec<u8> {
        self.payload
    }

    /// Encodes one complete frame for an asynchronous private writer.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_BYTES + self.payload.len());
        // Writing to a Vec is infallible.
        let _ = self.write_to(&mut bytes);
        bytes
    }

    /// Parses a fixed header and returns its kind, stream ID, and bounded payload length.
    ///
    /// # Errors
    ///
    /// Rejects malformed, incompatible, unknown, or oversized headers before allocation.
    pub fn parse_header(
        header: &[u8; BROKER_FRAME_HEADER_BYTES],
    ) -> Result<(BrokerFrameKind, u32, usize), BrokerProtocolError> {
        if &header[..4] != MAGIC || header[7] != 0 {
            return Err(BrokerProtocolError::Framing);
        }
        if u16::from_be_bytes([header[4], header[5]]) != CREDENTIAL_BROKER_PROTOCOL {
            return Err(BrokerProtocolError::Version);
        }
        let kind = BrokerFrameKind::try_from(header[6])?;
        let stream_id = u32::from_be_bytes(
            header[8..12]
                .try_into()
                .map_err(|_| BrokerProtocolError::Framing)?,
        );
        let length = u32::from_be_bytes(
            header[12..16]
                .try_into()
                .map_err(|_| BrokerProtocolError::Framing)?,
        ) as usize;
        if length > MAX_BROKER_FRAME_BYTES {
            return Err(BrokerProtocolError::Bounds);
        }
        Ok((kind, stream_id, length))
    }

    /// Creates a frame from a previously parsed header and its exact payload.
    ///
    /// # Errors
    ///
    /// Rejects inconsistent stream identity or an oversized payload.
    pub fn from_parsed_header(
        kind: BrokerFrameKind,
        stream_id: u32,
        payload: Vec<u8>,
    ) -> Result<Self, BrokerProtocolError> {
        Self::new(kind, stream_id, payload)
    }

    /// Writes one frame to a private stream.
    ///
    /// # Errors
    ///
    /// Returns value-free framing or I/O errors.
    pub fn write_to(&self, output: &mut impl Write) -> Result<(), BrokerProtocolError> {
        let mut header = [0_u8; HEADER_BYTES];
        header[..4].copy_from_slice(MAGIC);
        header[4..6].copy_from_slice(&CREDENTIAL_BROKER_PROTOCOL.to_be_bytes());
        header[6] = self.kind as u8;
        header[8..12].copy_from_slice(&self.stream_id.to_be_bytes());
        let length = u32::try_from(self.payload.len()).map_err(|_| BrokerProtocolError::Bounds)?;
        header[12..16].copy_from_slice(&length.to_be_bytes());
        output
            .write_all(&header)
            .map_err(|_| BrokerProtocolError::Io)?;
        output
            .write_all(&self.payload)
            .map_err(|_| BrokerProtocolError::Io)
    }

    /// Reads exactly one frame, returning `None` only for clean EOF before a header.
    ///
    /// # Errors
    ///
    /// Rejects truncation, bad magic/reserved bytes, unknown versions/kinds, and oversized frames.
    pub fn read_from(input: &mut impl Read) -> Result<Option<Self>, BrokerProtocolError> {
        let mut header = [0_u8; HEADER_BYTES];
        let read = read_full(input, &mut header)?;
        if read == 0 {
            return Ok(None);
        }
        if read != HEADER_BYTES {
            return Err(BrokerProtocolError::Truncated);
        }
        let (kind, stream_id, length) = Self::parse_header(&header)?;
        let mut payload = vec![0; length];
        if read_full(input, &mut payload)? != length {
            return Err(BrokerProtocolError::Truncated);
        }
        Self::new(kind, stream_id, payload).map(Some)
    }
}

fn read_full(input: &mut impl Read, bytes: &mut [u8]) -> Result<usize, BrokerProtocolError> {
    let mut offset = 0;
    while offset < bytes.len() {
        match input.read(&mut bytes[offset..]) {
            Ok(0) => break,
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return Err(BrokerProtocolError::Io),
        }
    }
    Ok(offset)
}

/// Per-connection admission accounting for streams and bounded outgoing queues.
#[derive(Debug, Default)]
pub struct BrokerAdmission {
    streams: BTreeSet<u32>,
    queued_frames: usize,
    queued_bytes: usize,
}

impl BrokerAdmission {
    /// Admits a new unique nonzero stream.
    ///
    /// # Errors
    ///
    /// Rejects zero, duplicate, or saturated stream admission.
    pub fn open(&mut self, stream_id: u32) -> Result<(), BrokerProtocolError> {
        if stream_id == 0
            || self.streams.len() == MAX_BROKER_STREAMS
            || !self.streams.insert(stream_id)
        {
            return Err(BrokerProtocolError::StreamLimit);
        }
        Ok(())
    }
    /// Closes a stream. Unknown streams fail closed.
    ///
    /// # Errors
    ///
    /// Rejects an inactive stream.
    pub fn close(&mut self, stream_id: u32) -> Result<(), BrokerProtocolError> {
        if self.streams.remove(&stream_id) {
            Ok(())
        } else {
            Err(BrokerProtocolError::UnknownStream)
        }
    }
    /// Reserves bounded queue capacity before accepting payload ownership.
    ///
    /// # Errors
    ///
    /// Rejects a payload, frame count, or aggregate byte count above its bound.
    pub fn enqueue(&mut self, payload_bytes: usize) -> Result<(), BrokerProtocolError> {
        if payload_bytes > MAX_BROKER_FRAME_BYTES
            || self.queued_frames == MAX_BROKER_QUEUED_FRAMES
            || self.queued_bytes.saturating_add(payload_bytes) > MAX_BROKER_QUEUED_BYTES
        {
            return Err(BrokerProtocolError::QueueLimit);
        }
        self.queued_frames += 1;
        self.queued_bytes += payload_bytes;
        Ok(())
    }
    /// Releases queue capacity after delivery or cancellation.
    pub fn dequeue(&mut self, payload_bytes: usize) {
        self.queued_frames = self.queued_frames.saturating_sub(1);
        self.queued_bytes = self.queued_bytes.saturating_sub(payload_bytes);
    }
    /// Returns active stream count without exposing payload state.
    #[must_use]
    pub fn active_streams(&self) -> usize {
        self.streams.len()
    }
}

/// Whether a lease belongs to the active generation or an operation-owned candidate.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialLeaseRole {
    /// Host-authorized currently active generation.
    Active,
    /// Separately operation-owned candidate, not implicitly active.
    Candidate,
}

/// Readiness of one exact generation lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialLeaseReadiness {
    /// Transport has not completed authenticated readiness.
    Starting,
    /// Exact transport identity completed readiness.
    Ready,
}

#[derive(Clone, Debug)]
struct LeaseRecord {
    identity: CredentialLeaseIdentity,
    role: CredentialLeaseRole,
    readiness: CredentialLeaseReadiness,
    operation: Option<u64>,
}

/// Host-owned active/candidate generation registry. It contains identity metadata, never secrets.
#[derive(Debug, Default)]
pub struct CredentialLeaseSet {
    leases: BTreeMap<GenerationId, LeaseRecord>,
}

impl CredentialLeaseSet {
    /// Starts an active generation lease. Existing generations are never replaced implicitly.
    ///
    /// # Errors
    ///
    /// Rejects invalid identity or a duplicate generation.
    pub fn start_active(
        &mut self,
        identity: CredentialLeaseIdentity,
    ) -> Result<(), BrokerProtocolError> {
        self.insert(identity, CredentialLeaseRole::Active, None)
    }
    /// Starts an operation-owned candidate without granting it active authority.
    ///
    /// # Errors
    ///
    /// Rejects zero operation identity, invalid identity, or a duplicate generation.
    pub fn start_candidate(
        &mut self,
        identity: CredentialLeaseIdentity,
        operation: u64,
    ) -> Result<(), BrokerProtocolError> {
        if operation == 0 {
            return Err(BrokerProtocolError::Identity("operation"));
        }
        self.insert(identity, CredentialLeaseRole::Candidate, Some(operation))
    }
    fn insert(
        &mut self,
        identity: CredentialLeaseIdentity,
        role: CredentialLeaseRole,
        operation: Option<u64>,
    ) -> Result<(), BrokerProtocolError> {
        identity.validate()?;
        if self.leases.contains_key(&identity.generation) {
            return Err(BrokerProtocolError::GenerationExists);
        }
        self.leases.insert(
            identity.generation,
            LeaseRecord {
                identity,
                role,
                readiness: CredentialLeaseReadiness::Starting,
                operation,
            },
        );
        Ok(())
    }
    /// Marks the exact authenticated identity ready.
    ///
    /// # Errors
    ///
    /// Rejects absent generations and every scoped identity mismatch.
    pub fn mark_ready(
        &mut self,
        identity: &CredentialLeaseIdentity,
    ) -> Result<(), BrokerProtocolError> {
        let lease = self.exact_mut(identity)?;
        lease.readiness = CredentialLeaseReadiness::Ready;
        Ok(())
    }
    /// Promotes only a ready candidate owned by the surrounding operation.
    ///
    /// # Errors
    ///
    /// Rejects absent, unready, active, or differently owned leases.
    pub fn promote_candidate(
        &mut self,
        generation: GenerationId,
        operation: u64,
    ) -> Result<(), BrokerProtocolError> {
        let lease = self
            .leases
            .get_mut(&generation)
            .ok_or(BrokerProtocolError::OldGeneration)?;
        if lease.role != CredentialLeaseRole::Candidate
            || lease.operation != Some(operation)
            || lease.readiness != CredentialLeaseReadiness::Ready
        {
            return Err(BrokerProtocolError::CandidateAuthority);
        }
        lease.role = CredentialLeaseRole::Active;
        lease.operation = None;
        Ok(())
    }
    /// Cancels one exact authenticated lease, preventing replay.
    ///
    /// # Errors
    ///
    /// Rejects absent generations and every scoped identity mismatch.
    pub fn stop(&mut self, identity: &CredentialLeaseIdentity) -> Result<(), BrokerProtocolError> {
        self.exact(identity)?;
        self.leases.remove(&identity.generation);
        Ok(())
    }
    /// Returns role/readiness only after every scoped identity field matches.
    ///
    /// # Errors
    ///
    /// Rejects absent generations and every scoped identity mismatch.
    pub fn status(
        &self,
        identity: &CredentialLeaseIdentity,
    ) -> Result<(CredentialLeaseRole, CredentialLeaseReadiness), BrokerProtocolError> {
        let lease = self.exact(identity)?;
        Ok((lease.role, lease.readiness))
    }
    fn exact(
        &self,
        identity: &CredentialLeaseIdentity,
    ) -> Result<&LeaseRecord, BrokerProtocolError> {
        let lease = self
            .leases
            .get(&identity.generation)
            .ok_or(BrokerProtocolError::OldGeneration)?;
        if &lease.identity == identity {
            Ok(lease)
        } else {
            Err(BrokerProtocolError::Identity("lease"))
        }
    }
    fn exact_mut(
        &mut self,
        identity: &CredentialLeaseIdentity,
    ) -> Result<&mut LeaseRecord, BrokerProtocolError> {
        let lease = self
            .leases
            .get_mut(&identity.generation)
            .ok_or(BrokerProtocolError::OldGeneration)?;
        if &lease.identity == identity {
            Ok(lease)
        } else {
            Err(BrokerProtocolError::Identity("lease"))
        }
    }
}

/// Value-free broker failures safe for diagnostics.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BrokerProtocolError {
    /// Stream ended within a frame.
    #[error("truncated credential broker frame")]
    Truncated,
    /// Header is malformed.
    #[error("malformed credential broker frame")]
    Framing,
    /// Protocol version is unsupported.
    #[error("unsupported credential broker protocol")]
    Version,
    /// Operation kind is unknown.
    #[error("unsupported credential broker operation")]
    Kind,
    /// Payload exceeds a published bound.
    #[error("credential broker payload exceeded its bound")]
    Bounds,
    /// Stream ID is inappropriate for the operation.
    #[error("invalid credential broker stream identity")]
    StreamId,
    /// Active-stream admission is saturated or duplicate.
    #[error("credential broker active stream limit reached")]
    StreamLimit,
    /// A frame references no active stream.
    #[error("credential broker frame references an inactive stream")]
    UnknownStream,
    /// Bounded queue is saturated.
    #[error("credential broker queue limit reached")]
    QueueLimit,
    /// Scoped identity does not match host authority.
    #[error("credential broker {0} identity mismatch")]
    Identity(&'static str),
    /// Generation is absent or was cancelled.
    #[error("credential broker generation is inactive")]
    OldGeneration,
    /// A generation cannot be replaced implicitly.
    #[error("credential broker generation already has a lease")]
    GenerationExists,
    /// Candidate is not ready or not owned by this operation.
    #[error("credential broker candidate authority mismatch")]
    CandidateAuthority,
    /// Private transport failed; payload details are suppressed.
    #[error("credential broker private transport failed")]
    Io,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(generation: u64) -> CredentialLeaseIdentity {
        CredentialLeaseIdentity {
            installation: InstallationId::parse("install-1").expect("installation"),
            workspace: WorkspaceName::parse("project").expect("workspace"),
            workspace_receipt: "0123456789abcdef0123456789abcdef".to_owned(),
            container: ContainerId::parse(&format!("{generation:064x}")).expect("container"),
            generation: GenerationId::new(generation).expect("generation"),
            user: CredentialUserIdentity {
                uid: 1000,
                gid: 1000,
            },
            host_build: AgentBuildId::parse("build-1").expect("build"),
            agent_build: AgentBuildId::parse("build-1").expect("build"),
            agent_protocol: ProtocolVersion::new(1).expect("protocol"),
            broker_protocol: CREDENTIAL_BROKER_PROTOCOL,
            grant_revision: 7,
        }
    }

    #[test]
    fn frames_round_trip_binary_payload_without_debug_disclosure() {
        let frame = BrokerFrame::new(BrokerFrameKind::StreamData, 9, b"SECRET\0bytes".to_vec())
            .expect("frame");
        let mut wire = Vec::new();
        frame.write_to(&mut wire).expect("write");
        let decoded = BrokerFrame::read_from(&mut wire.as_slice())
            .expect("read")
            .expect("frame");
        assert_eq!(decoded.payload(), b"SECRET\0bytes");
        assert!(!format!("{decoded:?}").contains("SECRET"));
    }

    #[test]
    fn malformed_truncated_oversized_unknown_and_wrong_version_frames_fail_closed() {
        let valid = BrokerFrame::new(BrokerFrameKind::Health, 0, Vec::new()).expect("frame");
        let mut wire = Vec::new();
        valid.write_to(&mut wire).expect("write");
        for bytes in [&wire[..3], &wire[..HEADER_BYTES - 1]] {
            let mut input = bytes;
            assert_eq!(
                BrokerFrame::read_from(&mut input),
                Err(BrokerProtocolError::Truncated)
            );
        }
        let mut bad = wire.clone();
        bad[4] = 0xff;
        assert_eq!(
            BrokerFrame::read_from(&mut bad.as_slice()),
            Err(BrokerProtocolError::Version)
        );
        let mut bad = wire.clone();
        bad[6] = 99;
        assert_eq!(
            BrokerFrame::read_from(&mut bad.as_slice()),
            Err(BrokerProtocolError::Kind)
        );
        let mut bad = wire;
        bad[12..16].copy_from_slice(
            &u32::try_from(MAX_BROKER_FRAME_BYTES + 1)
                .expect("bound fits u32")
                .to_be_bytes(),
        );
        assert_eq!(
            BrokerFrame::read_from(&mut bad.as_slice()),
            Err(BrokerProtocolError::Bounds)
        );
    }

    #[test]
    fn admission_enforces_stream_frame_and_total_byte_limits() {
        let mut admission = BrokerAdmission::default();
        for stream in 1..=u32::try_from(MAX_BROKER_STREAMS).expect("stream bound fits u32") {
            admission.open(stream).expect("within limit");
        }
        assert_eq!(admission.open(99), Err(BrokerProtocolError::StreamLimit));
        for _ in 0..MAX_BROKER_QUEUED_FRAMES {
            admission.enqueue(1).expect("within queue");
        }
        assert_eq!(admission.enqueue(1), Err(BrokerProtocolError::QueueLimit));
        for _ in 0..MAX_BROKER_QUEUED_FRAMES {
            admission.dequeue(1);
        }
        for _ in 0..(MAX_BROKER_QUEUED_BYTES / MAX_BROKER_FRAME_BYTES) {
            admission
                .enqueue(MAX_BROKER_FRAME_BYTES)
                .expect("within bytes");
        }
        assert_eq!(admission.enqueue(1), Err(BrokerProtocolError::QueueLimit));
    }

    #[test]
    fn candidates_require_exact_operation_and_cancelled_generations_cannot_replay() {
        let active = identity(1);
        let candidate = identity(2);
        let mut leases = CredentialLeaseSet::default();
        leases.start_active(active.clone()).expect("active");
        leases
            .start_candidate(candidate.clone(), 44)
            .expect("candidate");
        assert_eq!(
            leases.promote_candidate(candidate.generation, 44),
            Err(BrokerProtocolError::CandidateAuthority)
        );
        leases.mark_ready(&candidate).expect("ready");
        assert_eq!(
            leases.promote_candidate(candidate.generation, 45),
            Err(BrokerProtocolError::CandidateAuthority)
        );
        leases
            .promote_candidate(candidate.generation, 44)
            .expect("promote");
        leases.stop(&active).expect("stop");
        assert_eq!(
            leases.mark_ready(&active),
            Err(BrokerProtocolError::OldGeneration)
        );
    }

    #[test]
    fn every_scoped_identity_and_revision_must_match() {
        let mut expected = identity(1);
        let mut leases = CredentialLeaseSet::default();
        leases.start_active(expected.clone()).expect("start");
        expected.grant_revision += 1;
        let wrong = expected;
        assert_eq!(
            leases.mark_ready(&wrong),
            Err(BrokerProtocolError::Identity("lease"))
        );
    }
}
