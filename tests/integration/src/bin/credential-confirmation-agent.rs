//! Simulated confirmation policy in front of a real OpenSSH signing agent.
//!
//! This is protocol evidence, not an observation of a platform confirmation UI.
//! A host-owned policy file controls signing requests; other packets pass through.

use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

async fn packet(stream: &mut UnixStream) -> io::Result<Vec<u8>> {
    let length = stream.read_u32().await?;
    if length == 0 || length as usize > cdenv_cli::MAX_SSH_AGENT_PACKET_BYTES - 4 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent frame bound",
        ));
    }
    let mut payload = vec![0; length as usize];
    stream.read_exact(&mut payload).await?;
    Ok(payload)
}

async fn respond(stream: &mut UnixStream, payload: &[u8]) -> io::Result<()> {
    let length = u32::try_from(payload.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "agent frame bound"))?;
    stream.write_u32(length).await?;
    stream.write_all(payload).await
}

async fn serve(
    mut client: UnixStream,
    upstream: &Path,
    policy: &Path,
    pending: &Path,
) -> io::Result<()> {
    let mut agent = UnixStream::connect(upstream).await?;
    loop {
        let request = packet(&mut client).await?;
        // SSH2_AGENTC_SIGN_REQUEST; identity and extension requests pass through.
        if request[0] == 13 {
            let decision = std::fs::read_to_string(policy)?;
            if decision == "reject" {
                respond(&mut client, &[5]).await?; // SSH_AGENT_FAILURE
                continue;
            }
            if decision == "timeout" {
                std::fs::write(pending, b"simulated confirmation pending")?;
                // Wait for this connection's cancellation, without blocking any
                // other client or forwarding a signature after the broker timeout.
                let mut byte = [0];
                let _ = client.read(&mut byte).await?;
                return Ok(());
            }
        }
        respond(&mut agent, &request).await?;
        respond(&mut client, &packet(&mut agent).await?).await?;
    }
}

#[tokio::main]
async fn main() {
    let arguments = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let [socket, upstream, policy, pending] = arguments.as_slice() else {
        panic!("usage: credential-confirmation-agent SOCKET UPSTREAM POLICY PENDING");
    };
    let listener = UnixListener::bind(socket).expect("bind simulated confirmation agent");
    loop {
        let (client, _) = listener.accept().await.expect("accept agent client");
        let upstream = upstream.clone();
        let policy = policy.clone();
        let pending = pending.clone();
        tokio::spawn(async move {
            // Normal disconnects and broker cancellation are expected.
            let _ = serve(client, &upstream, &policy, &pending).await;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn simulated_rejection_and_pending_request_leave_other_clients_working() {
        let root = tempfile::tempdir().expect("fixture directory");
        let upstream = root.path().join("upstream");
        let pending = root.path().join("pending");
        let policy = root.path().join("policy");
        std::fs::write(&policy, "timeout").expect("policy");
        let listener = UnixListener::bind(&upstream).expect("upstream socket");
        let server = tokio::spawn(async move {
            loop {
                let (mut stream, _) = listener.accept().await.expect("accept");
                tokio::spawn(async move {
                    while let Ok(request) = packet(&mut stream).await {
                        // Echo lets us assert byte-preserving forwarding.
                        respond(&mut stream, &request).await.expect("echo");
                    }
                });
            }
        });
        let (mut stalled, peer) = UnixStream::pair().expect("client");
        let path = upstream.clone();
        let marker = pending.clone();
        let first_policy = policy.clone();
        let task = tokio::spawn(async move { serve(peer, &path, &first_policy, &marker).await });
        let request = vec![13];
        respond(&mut stalled, &request).await.expect("pending sign");
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while !pending.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("pending acknowledgement");
        assert!(!task.is_finished());

        let (mut client, peer) = UnixStream::pair().expect("other client");
        let other_policy = policy.clone();
        let other =
            tokio::spawn(async move { serve(peer, &upstream, &other_policy, &pending).await });
        std::fs::write(&policy, "reject").expect("policy");
        let reject = vec![13];
        respond(&mut client, &reject).await.expect("reject sign");
        assert_eq!(packet(&mut client).await.expect("rejection"), [5]);
        std::fs::write(&policy, "allow").expect("policy");
        for request in [vec![11], vec![13, 1, 2, 3]] {
            respond(&mut client, &request)
                .await
                .expect("allowed request");
            assert_eq!(
                packet(&mut client).await.expect("forwarded response"),
                request
            );
        }
        drop(stalled);
        task.await
            .expect("cancelled task")
            .expect("cancelled stream");
        drop(client);
        let _ = other.await;
        server.abort();
    }
}
