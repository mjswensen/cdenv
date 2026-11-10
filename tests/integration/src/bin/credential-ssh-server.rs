//! Controlled TCP Git-over-SSH origin using the cdenv SSH implementation.
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use cdenv_agent::{SshServerConfig, SshServerRequest, serve_ssh_stream};

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [host_key, authorized_key, environment, workspace, ready] = arguments.as_slice() else {
        panic!("usage: credential-ssh-server HOST_KEY AUTHORIZED_KEY ENVIRONMENT WORKSPACE READY");
    };
    let request = SshServerRequest {
        host_key: PathBuf::from(host_key),
        authorized_key: PathBuf::from(authorized_key),
        environment: PathBuf::from(environment),
        workspace: PathBuf::from(workspace),
        credential_runtime: None,
        credential_grants: cdenv_core::credentials::CredentialGrants::default(),
    };
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0")
        .await
        .expect("bind fixture");
    std::fs::write(
        ready,
        listener
            .local_addr()
            .expect("fixture address")
            .port()
            .to_string(),
    )
    .expect("publish readiness");
    loop {
        let (stream, _) = listener.accept().await.expect("accept SSH fixture");
        let config = SshServerConfig::load(&request).expect("load fixture identity");
        tokio::spawn(async move {
            // Trust-rejection clients deliberately disconnect before authentication.
            let _ = tokio::time::timeout(
                std::time::Duration::from_secs(30),
                serve_ssh_stream(stream, config),
            )
            .await;
        });
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    panic!("credential SSH fixture requires Linux");
}
