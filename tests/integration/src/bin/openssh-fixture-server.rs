//! Stdio SSH server used only by the OpenSSH release gate.

use std::path::PathBuf;
use std::process::ExitCode;

use cdenv_agent::{SshServerConfig, SshServerRequest, serve_ssh_stream};

/// Starts one agent SSH transport over standard I/O.
#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let [host_key, authorized_key, environment, workspace] = arguments.as_slice() else {
        eprintln!("usage: openssh-fixture-server HOST_KEY AUTHORIZED_KEY ENVIRONMENT WORKSPACE");
        return ExitCode::FAILURE;
    };
    let request = SshServerRequest {
        host_key: PathBuf::from(host_key),
        authorized_key: PathBuf::from(authorized_key),
        environment: PathBuf::from(environment),
        workspace: PathBuf::from(workspace),
    };
    let result = match SshServerConfig::load(&request) {
        Ok(config) => {
            let stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
            serve_ssh_stream(stream, config).await
        }
        Err(error) => Err(error),
    };
    if let Err(error) = result {
        eprintln!("openssh fixture server: {error}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
