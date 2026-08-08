use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UnixListener, UnixStream};

use crate::docker_api::{DockerEndpoint, bridge_exec};
use crate::error::{Result, SpikeError, read, write};
use crate::profile::canonical_json;

#[derive(Debug, Clone)]
pub(crate) struct SupervisorConfig {
    pub(crate) endpoint: DockerEndpoint,
    pub(crate) workspace: String,
    pub(crate) generation: String,
    pub(crate) container: String,
    pub(crate) agent_path: String,
    pub(crate) target_host: String,
    pub(crate) target_port: u16,
    pub(crate) bind: String,
    pub(crate) control_socket: PathBuf,
    pub(crate) token_file: PathBuf,
    pub(crate) ready_file: PathBuf,
}

#[derive(Debug, Serialize)]
struct ReadyRecord<'a> {
    workspace: &'a str,
    generation: &'a str,
    bind: String,
    #[serde(rename = "targetHost")]
    target_host: &'a str,
    #[serde(rename = "targetPort")]
    target_port: u16,
    #[serde(rename = "controlSocket")]
    control_socket: String,
}

#[expect(
    clippy::too_many_lines,
    reason = "the disposable supervisor spike keeps listener and authenticated-control lifecycle together"
)]
pub(crate) async fn run(config: SupervisorConfig) -> Result<()> {
    let token = load_token(&config.token_file)?;
    if let Some(parent) = config.control_socket.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SpikeError::Write {
            path: parent.to_path_buf(),
            source,
        })?;
        std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).map_err(
            |source| SpikeError::Write {
                path: parent.to_path_buf(),
                source,
            },
        )?;
    }
    if config.control_socket.exists() {
        return Err(SpikeError::Profile {
            path: config.control_socket.display().to_string(),
            detail: "control socket already exists; refusing to replace an unverified supervisor"
                .to_string(),
        });
    }
    let listener =
        TcpListener::bind(&config.bind)
            .await
            .map_err(|source| SpikeError::Dependency {
                name: "declared forwarding listener",
                detail: format!("{}: {source}", config.bind),
            })?;
    let assigned = listener
        .local_addr()
        .map_err(|source| SpikeError::Dependency {
            name: "declared forwarding listener address",
            detail: source.to_string(),
        })?;
    let control =
        UnixListener::bind(&config.control_socket).map_err(|source| SpikeError::Dependency {
            name: "forwarding supervisor control socket",
            detail: format!("{}: {source}", config.control_socket.display()),
        })?;
    std::fs::set_permissions(
        &config.control_socket,
        std::fs::Permissions::from_mode(0o600),
    )
    .map_err(|source| SpikeError::Write {
        path: config.control_socket.clone(),
        source,
    })?;
    let ready = ReadyRecord {
        workspace: &config.workspace,
        generation: &config.generation,
        bind: assigned.to_string(),
        target_host: &config.target_host,
        target_port: config.target_port,
        control_socket: config.control_socket.display().to_string(),
    };
    let ready_bytes = canonical_json(&ready)?;
    let temporary = config.ready_file.with_extension("tmp");
    write(&temporary, &ready_bytes)?;
    std::fs::rename(&temporary, &config.ready_file).map_err(|source| SpikeError::Write {
        path: config.ready_file.clone(),
        source,
    })?;

    let endpoint = Arc::new(config.endpoint);
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted.map_err(|source| SpikeError::Dependency {
                    name: "declared forwarding listener",
                    detail: source.to_string(),
                })?;
                let endpoint = Arc::clone(&endpoint);
                let container = config.container.clone();
                let agent_path = config.agent_path.clone();
                let target_host = config.target_host.clone();
                let target_port = config.target_port;
                tokio::spawn(async move {
                    if let Err(error) = bridge_exec(
                        &endpoint,
                        &container,
                        &agent_path,
                        &target_host,
                        target_port,
                        stream,
                    ).await {
                        eprintln!("forward target temporarily unavailable: {error}");
                    }
                });
            }
            accepted = control.accept() => {
                let (stream, _) = accepted.map_err(|source| SpikeError::Dependency {
                    name: "forwarding supervisor control socket",
                    detail: source.to_string(),
                })?;
                if authenticate_down(stream, &token, &config.workspace, &config.generation).await? {
                    break;
                }
            }
        }
    }
    let _ = std::fs::remove_file(&config.control_socket);
    let _ = std::fs::remove_file(&config.ready_file);
    Ok(())
}

fn load_token(path: &Path) -> Result<String> {
    let metadata = std::fs::metadata(path).map_err(|source| SpikeError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.permissions().mode() & 0o077 != 0 {
        return Err(SpikeError::Profile {
            path: path.display().to_string(),
            detail: "supervisor token must not be group/world accessible".to_string(),
        });
    }
    let token = String::from_utf8(read(path)?).map_err(|error| SpikeError::Profile {
        path: path.display().to_string(),
        detail: format!("token is not UTF-8: {error}"),
    })?;
    let token = token.trim().to_string();
    if token.len() < 16 || token.len() > 256 {
        return Err(SpikeError::Profile {
            path: path.display().to_string(),
            detail: "supervisor token must contain 16..=256 bytes".to_string(),
        });
    }
    Ok(token)
}

async fn authenticate_down(
    stream: UnixStream,
    token: &str,
    workspace: &str,
    generation: &str,
) -> Result<bool> {
    let mut reader = BufReader::new(stream);
    let mut request = String::new();
    let read = (&mut reader)
        .take(1024)
        .read_line(&mut request)
        .await
        .map_err(|source| SpikeError::Read {
            path: Path::new("supervisor control request").to_path_buf(),
            source,
        })?;
    let expected = format!("down {workspace} {generation} {token}\n");
    let authenticated = read == request.len() && request == expected;
    let response: &[u8] = if authenticated { b"ok\n" } else { b"denied\n" };
    reader
        .get_mut()
        .write_all(response)
        .await
        .map_err(|source| SpikeError::Write {
            path: Path::new("supervisor control response").to_path_buf(),
            source,
        })?;
    Ok(authenticated)
}

pub(crate) async fn stop(
    socket: &Path,
    token_file: &Path,
    workspace: &str,
    generation: &str,
) -> Result<()> {
    let token = load_token(token_file)?;
    let mut stream =
        UnixStream::connect(socket)
            .await
            .map_err(|source| SpikeError::Dependency {
                name: "running forwarding supervisor",
                detail: format!("{}: {source}", socket.display()),
            })?;
    stream
        .write_all(format!("down {workspace} {generation} {token}\n").as_bytes())
        .await
        .map_err(|source| SpikeError::Write {
            path: socket.to_path_buf(),
            source,
        })?;
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .await
        .map_err(|source| SpikeError::Read {
            path: socket.to_path_buf(),
            source,
        })?;
    if response != "ok\n" {
        return Err(SpikeError::Profile {
            path: socket.display().to_string(),
            detail: "supervisor rejected authenticated down request".to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_control_token_is_rejected() {
        let directory = tempfile::tempdir().expect("temporary directory should exist");
        let token = directory.path().join("token");
        std::fs::write(&token, "short\n").expect("token should write");
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o600))
            .expect("permissions should apply");

        let error = load_token(&token).expect_err("short token should fail");

        assert!(error.to_string().contains("16..=256"));
    }
}
