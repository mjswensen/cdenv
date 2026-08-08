use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::process::Stdio;

use bollard::Docker;
use bollard::container::LogOutput;
use bollard::exec::{StartExecOptions, StartExecResults};
use bollard::models::ExecConfig;
use bollard::query_parameters::{InspectContainerOptions, ListContainersOptionsBuilder};
use futures_util::StreamExt;
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufWriter};

use crate::error::{Result, SpikeError};

const DOCKER_TIMEOUT_SECONDS: u64 = 120;
const COMMAND_OUTPUT_LIMIT: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct DockerEndpoint {
    pub(crate) socket_path: String,
    pub(crate) docker_host: String,
}

#[derive(Debug, Clone)]
pub(crate) struct CommandOutput {
    pub(crate) stdout: Vec<u8>,
}

#[derive(Debug, Serialize)]
pub(crate) struct DockerVersions {
    #[serde(rename = "dockerClient")]
    pub(crate) docker_client: String,
    #[serde(rename = "dockerEngine")]
    pub(crate) docker_engine: String,
    #[serde(rename = "dockerApi")]
    pub(crate) docker_api: String,
    #[serde(rename = "compose")]
    pub(crate) compose: String,
    #[serde(rename = "bollardApi")]
    pub(crate) bollard_api: String,
    #[serde(rename = "socket")]
    pub(crate) socket: String,
}

#[derive(Debug, Serialize)]
pub(crate) struct LabelEvidence {
    pub(crate) workspace: String,
    #[serde(rename = "containerCount")]
    pub(crate) container_count: usize,
    #[serde(rename = "runningCount")]
    pub(crate) running_count: usize,
    pub(crate) services: Vec<String>,
    pub(crate) labels: BTreeMap<String, String>,
}

pub(crate) fn resolve_endpoint() -> Result<DockerEndpoint> {
    let configured = std::env::var("DOCKER_HOST").ok();
    let socket = match configured.as_deref() {
        Some(host) if host.starts_with("unix://") => host.trim_start_matches("unix://").to_string(),
        Some(host) => return Err(SpikeError::DockerEndpoint(host.to_string())),
        None => "/var/run/docker.sock".to_string(),
    };
    let metadata = std::fs::metadata(&socket).map_err(|error| SpikeError::Dependency {
        name: "Docker Engine Unix socket",
        detail: format!("{socket}: {error}"),
    })?;
    if !std::os::unix::fs::FileTypeExt::is_socket(&metadata.file_type()) {
        return Err(SpikeError::Dependency {
            name: "Docker Engine Unix socket",
            detail: format!("{socket} is not a Unix socket"),
        });
    }
    Ok(DockerEndpoint {
        docker_host: format!("unix://{socket}"),
        socket_path: socket,
    })
}

pub(crate) fn connect(endpoint: &DockerEndpoint) -> Result<Docker> {
    Docker::connect_with_socket(
        &endpoint.socket_path,
        DOCKER_TIMEOUT_SECONDS,
        bollard::API_DEFAULT_VERSION,
    )
    .map_err(SpikeError::from)
}

pub(crate) async fn versions(endpoint: &DockerEndpoint) -> Result<DockerVersions> {
    let docker = connect(endpoint)?;
    docker.ping().await?;
    let engine = docker.version().await?;
    let cli = run_command(
        "docker",
        &["version", "--format", "{{.Client.Version}}"],
        None,
        endpoint,
    )
    .await?;
    let compose = run_command("docker", &["compose", "version", "--short"], None, endpoint).await?;
    Ok(DockerVersions {
        docker_client: trimmed_utf8(&cli.stdout, "docker client version")?,
        docker_engine: engine.version.unwrap_or_else(|| "unknown".to_string()),
        docker_api: engine.api_version.unwrap_or_else(|| "unknown".to_string()),
        compose: trimmed_utf8(&compose.stdout, "Compose version")?,
        bollard_api: format!(
            "{}.{}",
            bollard::API_DEFAULT_VERSION.major_version,
            bollard::API_DEFAULT_VERSION.minor_version
        ),
        socket: endpoint.docker_host.clone(),
    })
}

pub(crate) async fn run_command(
    program: &str,
    arguments: &[&str],
    current_dir: Option<&Path>,
    endpoint: &DockerEndpoint,
) -> Result<CommandOutput> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .env("DOCKER_HOST", &endpoint.docker_host)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = current_dir {
        command.current_dir(directory);
    }
    let output = command
        .output()
        .await
        .map_err(|source| SpikeError::CommandStart {
            program: program.to_string(),
            source,
        })?;
    if output.stdout.len() > COMMAND_OUTPUT_LIMIT || output.stderr.len() > COMMAND_OUTPUT_LIMIT {
        return Err(SpikeError::CommandFailed {
            command: sanitized_command(program, arguments),
            status: output.status.to_string(),
            stderr: format!("captured output exceeds {COMMAND_OUTPUT_LIMIT} bytes"),
        });
    }
    if !output.status.success() {
        return Err(SpikeError::CommandFailed {
            command: sanitized_command(program, arguments),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(CommandOutput {
        stdout: output.stdout,
    })
}

fn sanitized_command(program: &str, arguments: &[&str]) -> String {
    let sanitized = arguments.iter().map(|argument| {
        if argument.contains("TOKEN=")
            || argument.starts_with("--password")
            || argument.starts_with("--secret")
        {
            "<redacted>"
        } else {
            *argument
        }
    });
    std::iter::once(program)
        .chain(sanitized)
        .collect::<Vec<_>>()
        .join(" ")
}

fn trimmed_utf8(bytes: &[u8], subject: &str) -> Result<String> {
    std::str::from_utf8(bytes)
        .map(str::trim)
        .map(ToString::to_string)
        .map_err(|error| SpikeError::Dependency {
            name: "UTF-8 external version output",
            detail: format!("{subject}: {error}"),
        })
}

pub(crate) async fn verify_workspace_labels(
    endpoint: &DockerEndpoint,
    workspace: &str,
    expected_count: usize,
) -> Result<LabelEvidence> {
    let docker = connect(endpoint)?;
    let filters = HashMap::from([(
        "label".to_string(),
        vec![
            "cdenv.installation=spike-installation".to_string(),
            format!("cdenv.workspace={workspace}"),
            "cdenv.generation=1".to_string(),
            "cdenv.profile=cdenv-devcontainer-v1".to_string(),
        ],
    )]);
    let containers = docker
        .list_containers(Some(
            ListContainersOptionsBuilder::default()
                .all(true)
                .filters(&filters)
                .build(),
        ))
        .await?;
    if containers.len() != expected_count {
        return Err(SpikeError::Profile {
            path: "Docker labels".to_string(),
            detail: format!(
                "workspace `{workspace}` has {} labelled containers; expected {expected_count}",
                containers.len()
            ),
        });
    }
    let mut running_count = 0_usize;
    let mut services = Vec::new();
    let required = BTreeMap::from([
        (
            "cdenv.installation".to_string(),
            "spike-installation".to_string(),
        ),
        ("cdenv.workspace".to_string(), workspace.to_string()),
        ("cdenv.generation".to_string(), "1".to_string()),
        (
            "cdenv.profile".to_string(),
            "cdenv-devcontainer-v1".to_string(),
        ),
    ]);
    for summary in &containers {
        let id = summary.id.as_deref().ok_or_else(|| SpikeError::Profile {
            path: "Docker container summary".to_string(),
            detail: "labelled container has no ID".to_string(),
        })?;
        let inspected = docker
            .inspect_container(id, None::<InspectContainerOptions>)
            .await?;
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        for (key, expected) in &required {
            if labels.get(key) != Some(expected) {
                return Err(SpikeError::Profile {
                    path: format!("container[{id}].labels.{key}"),
                    detail: format!("expected `{expected}`, got `{:?}`", labels.get(key)),
                });
            }
        }
        if inspected
            .state
            .and_then(|state| state.running)
            .unwrap_or(false)
        {
            running_count += 1;
        }
        if let Some(service) = labels.get("com.docker.compose.service") {
            services.push(service.clone());
        }
    }
    services.sort();
    Ok(LabelEvidence {
        workspace: workspace.to_string(),
        container_count: containers.len(),
        running_count,
        services,
        labels: required,
    })
}

#[expect(
    clippy::too_many_lines,
    reason = "the disposable packet-flow spike keeps the full duplex adapter in one reviewable function"
)]
pub(crate) async fn proxy_exec(
    endpoint: &DockerEndpoint,
    container: &str,
    command: Vec<String>,
) -> Result<()> {
    let docker = connect(endpoint)?;
    let exec = docker
        .create_exec(
            container,
            ExecConfig {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(false),
                cmd: Some(command),
                ..Default::default()
            },
        )
        .await?;
    let StartExecResults::Attached { mut output, input } = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                output_capacity: Some(64 * 1024),
                ..Default::default()
            }),
        )
        .await?
    else {
        return Err(SpikeError::Profile {
            path: "Bollard Exec".to_string(),
            detail: "attached Exec unexpectedly started detached".to_string(),
        });
    };

    let mut input_task = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut input = input;
        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            let read = stdin
                .read(&mut buffer)
                .await
                .map_err(|source| SpikeError::Read {
                    path: Path::new("ProxyCommand stdin").to_path_buf(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            input
                .write_all(&buffer[..read])
                .await
                .map_err(|source| SpikeError::Write {
                    path: Path::new("Docker Exec stdin").to_path_buf(),
                    source,
                })?;
            input.flush().await.map_err(|source| SpikeError::Write {
                path: Path::new("Docker Exec stdin flush").to_path_buf(),
                source,
            })?;
        }
        input.shutdown().await.map_err(|source| SpikeError::Write {
            path: Path::new("Docker Exec stdin EOF").to_path_buf(),
            source,
        })
    });

    let mut stdout = BufWriter::with_capacity(64 * 1024, tokio::io::stdout());
    let mut stderr = BufWriter::with_capacity(16 * 1024, tokio::io::stderr());
    let input_closed_first = loop {
        tokio::select! {
            result = &mut input_task => {
                result??;
                break true;
            }
            frame = output.next() => {
                let Some(frame) = frame else {
                    break false;
                };
                match frame? {
                    LogOutput::StdOut { message } => {
                        stdout
                            .write_all(&message)
                            .await
                            .map_err(|source| SpikeError::Write {
                                path: Path::new("proxy stdout").to_path_buf(),
                                source,
                            })?;
                        stdout.flush().await.map_err(|source| SpikeError::Write {
                            path: Path::new("proxy stdout flush").to_path_buf(),
                            source,
                        })?;
                    }
                    LogOutput::StdErr { message } => {
                        stderr
                            .write_all(&message)
                            .await
                            .map_err(|source| SpikeError::Write {
                                path: Path::new("proxy stderr").to_path_buf(),
                                source,
                            })?;
                        stderr.flush().await.map_err(|source| SpikeError::Write {
                            path: Path::new("proxy stderr flush").to_path_buf(),
                            source,
                        })?;
                    }
                    LogOutput::StdIn { .. } | LogOutput::Console { .. } => {
                        return Err(SpikeError::Profile {
                            path: "Bollard Exec framing".to_string(),
                            detail: "unexpected stdin/TTY frame would contaminate protocol output"
                                .to_string(),
                        });
                    }
                }
            }
        }
    };
    stdout.flush().await.map_err(|source| SpikeError::Write {
        path: Path::new("proxy stdout").to_path_buf(),
        source,
    })?;
    stderr.flush().await.map_err(|source| SpikeError::Write {
        path: Path::new("proxy stderr").to_path_buf(),
        source,
    })?;
    if !input_closed_first {
        input_task.await??;
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the disposable forwarding spike keeps attach setup and framing together"
)]
pub(crate) async fn bridge_exec(
    endpoint: &DockerEndpoint,
    container: &str,
    agent_path: &str,
    target_host: &str,
    target_port: u16,
    stream: tokio::net::TcpStream,
) -> Result<()> {
    let docker = connect(endpoint)?;
    let exec = docker
        .create_exec(
            container,
            ExecConfig {
                attach_stdin: Some(true),
                attach_stdout: Some(true),
                attach_stderr: Some(true),
                tty: Some(false),
                cmd: Some(vec![
                    agent_path.to_string(),
                    "tcp-bridge".to_string(),
                    target_host.to_string(),
                    target_port.to_string(),
                ]),
                ..Default::default()
            },
        )
        .await?;
    let StartExecResults::Attached { mut output, input } = docker
        .start_exec(
            &exec.id,
            Some(StartExecOptions {
                output_capacity: Some(64 * 1024),
                ..Default::default()
            }),
        )
        .await?
    else {
        return Err(SpikeError::Profile {
            path: "forwarding Bollard Exec".to_string(),
            detail: "bridge unexpectedly started detached".to_string(),
        });
    };
    let (mut socket_read, mut socket_write) = stream.into_split();
    let input_task = tokio::spawn(async move {
        let mut input = input;
        let mut buffer = vec![0_u8; 32 * 1024];
        loop {
            let read = socket_read
                .read(&mut buffer)
                .await
                .map_err(|source| SpikeError::Read {
                    path: Path::new("forward listener input").to_path_buf(),
                    source,
                })?;
            if read == 0 {
                break;
            }
            input
                .write_all(&buffer[..read])
                .await
                .map_err(|source| SpikeError::Write {
                    path: Path::new("forward target stdin").to_path_buf(),
                    source,
                })?;
            input.flush().await.map_err(|source| SpikeError::Write {
                path: Path::new("forward target stdin flush").to_path_buf(),
                source,
            })?;
        }
        input.shutdown().await.map_err(|source| SpikeError::Write {
            path: Path::new("forward target EOF").to_path_buf(),
            source,
        })
    });
    while let Some(frame) = output.next().await {
        match frame? {
            LogOutput::StdOut { message } => {
                socket_write
                    .write_all(&message)
                    .await
                    .map_err(|source| SpikeError::Write {
                        path: Path::new("declared forward socket").to_path_buf(),
                        source,
                    })?;
            }
            LogOutput::StdErr { message } => {
                return Err(SpikeError::Profile {
                    path: "forward target".to_string(),
                    detail: String::from_utf8_lossy(&message).trim().to_string(),
                });
            }
            LogOutput::StdIn { .. } | LogOutput::Console { .. } => {
                return Err(SpikeError::Profile {
                    path: "forward target framing".to_string(),
                    detail: "unexpected stdin/TTY frame".to_string(),
                });
            }
        }
    }
    socket_write
        .shutdown()
        .await
        .map_err(|source| SpikeError::Write {
            path: Path::new("declared forward EOF").to_path_buf(),
            source,
        })?;
    input_task.await??;
    Ok(())
}
