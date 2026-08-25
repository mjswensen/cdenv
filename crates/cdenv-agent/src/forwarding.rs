//! Container-side TCP forwarding bridge.

use std::net::IpAddr;

use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;

use crate::{BUILD_ID, PROTOCOL_VERSION};

/// Maximum accepted forwarding target host length.
pub const MAXIMUM_FORWARD_HOST_BYTES: usize = 253;

/// A validated container-local or Compose-service forwarding target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardTarget {
    host: String,
    port: u16,
}

impl ForwardTarget {
    /// Validates a `localhost` or Compose DNS service target and a nonzero port.
    ///
    /// # Errors
    ///
    /// Returns [`ForwardingError::InvalidHost`] or [`ForwardingError::InvalidPort`].
    pub fn new(host: &str, port: u16) -> Result<Self, ForwardingError> {
        if port == 0 {
            return Err(ForwardingError::InvalidPort);
        }
        if !valid_host(host) {
            return Err(ForwardingError::InvalidHost);
        }
        Ok(Self {
            host: host.to_owned(),
            port,
        })
    }

    /// Borrows the validated target host.
    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// Returns the validated target port.
    #[must_use]
    pub const fn port(&self) -> u16 {
        self.port
    }
}

/// A forwarding target, identity, connection, or stream failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ForwardingError {
    /// The host is not `localhost` or a valid Compose DNS service name.
    #[error("forward target must be `localhost` or a valid Compose service hostname")]
    InvalidHost,
    /// TCP port zero is never a forwarding target.
    #[error("forward target port must be nonzero")]
    InvalidPort,
    /// The invoking host build does not match this agent.
    #[error("forwarding agent build identity mismatch")]
    BuildMismatch,
    /// The invoking protocol does not match this agent.
    #[error("forwarding agent protocol identity mismatch")]
    ProtocolMismatch,
    /// The target could not be reached from the container.
    #[error("cannot connect to forward target {host}:{port}: {source}")]
    Connect {
        /// Validated host.
        host: String,
        /// Validated port.
        port: u16,
        /// Socket failure.
        #[source]
        source: std::io::Error,
    },
    /// Bidirectional binary streaming failed.
    #[error("forwarding stream failed: {0}")]
    Stream(#[source] std::io::Error),
}

/// Verifies host/agent compatibility before any target connection.
///
/// # Errors
///
/// Returns a focused build or protocol mismatch.
pub fn verify_forwarding_identity(
    build_id: &str,
    protocol_version: u32,
) -> Result<(), ForwardingError> {
    if build_id != BUILD_ID {
        return Err(ForwardingError::BuildMismatch);
    }
    if protocol_version != PROTOCOL_VERSION {
        return Err(ForwardingError::ProtocolMismatch);
    }
    Ok(())
}

/// Connects to a validated target and copies exact bytes in both directions.
///
/// Tokio's bidirectional copy uses fixed-size buffers, providing bounded
/// backpressure. EOF on either writer is propagated as a half-close.
///
/// # Errors
///
/// Returns a target connection or stream I/O failure.
pub async fn bridge_forwarding_stream<S>(
    stream: &mut S,
    target: &ForwardTarget,
) -> Result<(), ForwardingError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut target_stream = TcpStream::connect((target.host(), target.port()))
        .await
        .map_err(|source| ForwardingError::Connect {
            host: target.host().to_owned(),
            port: target.port(),
            source,
        })?;
    tokio::io::copy_bidirectional(stream, &mut target_stream)
        .await
        .map_err(ForwardingError::Stream)?;
    Ok(())
}

fn valid_host(host: &str) -> bool {
    if host == "localhost" {
        return true;
    }
    if host.is_empty()
        || host.len() > MAXIMUM_FORWARD_HOST_BYTES
        || host.parse::<IpAddr>().is_ok()
        || host.starts_with('-')
        || host.ends_with('-')
    {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_rejects_numeric_and_option_shaped_hosts() {
        assert!(ForwardTarget::new("127.0.0.1", 80).is_err());
        assert!(ForwardTarget::new("--help", 80).is_err());
    }

    #[test]
    fn target_accepts_localhost_and_compose_service_dns() {
        assert!(ForwardTarget::new("localhost", 80).is_ok());
        assert!(ForwardTarget::new("database-1.internal", 5432).is_ok());
    }

    #[tokio::test]
    async fn bridge_preserves_binary_bytes_and_eof() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let port = listener.local_addr().expect("address").port();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            stream.read_to_end(&mut bytes).await.expect("read EOF");
            stream.write_all(&bytes).await.expect("echo");
        });
        let target = ForwardTarget::new("localhost", port).expect("target");
        let (mut client, mut bridge) = tokio::io::duplex(64);
        let bridge_task = tokio::spawn(async move {
            bridge_forwarding_stream(&mut bridge, &target)
                .await
                .expect("bridge");
        });
        let expected = (0_u8..=255).cycle().take(128 * 1024).collect::<Vec<_>>();
        client.write_all(&expected).await.expect("write");
        client.shutdown().await.expect("half close");
        let mut actual = Vec::new();
        client.read_to_end(&mut actual).await.expect("read");
        bridge_task.await.expect("bridge task");
        server.await.expect("server task");

        assert_eq!(actual, expected);
    }
}
