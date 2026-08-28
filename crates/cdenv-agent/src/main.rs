//! Thin executable entry point for the cdenv container agent.

use std::ffi::OsString;
use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const MAXIMUM_CAPTURE_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[tokio::main]
async fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    #[cfg(target_os = "linux")]
    if let [
        command,
        stdio,
        host_key,
        authorized_key,
        environment,
        workspace,
    ] = arguments.as_slice()
        && command == "ssh-server"
        && stdio == "--stdio"
    {
        return ssh_server(
            Path::new(host_key),
            Path::new(authorized_key),
            Path::new(environment),
            Path::new(workspace),
        )
        .await;
    }
    #[cfg(target_os = "linux")]
    if let [command, manifest] = arguments.as_slice()
        && command == "post-attach"
    {
        return match post_attach(Path::new(manifest)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cdenv-agent: postAttachCommand failed: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if let [command, host, port, build_id, protocol] = arguments.as_slice()
        && command == "forwarding-bridge"
    {
        return forwarding_bridge(
            &host.to_string_lossy(),
            &port.to_string_lossy(),
            &build_id.to_string_lossy(),
            &protocol.to_string_lossy(),
        )
        .await;
    }
    if matches!(arguments.as_slice(), [command] if command == "emit-environment") {
        return match cdenv_agent::emit_current_environment() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cdenv-agent: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if let [command, snapshot, separator, program, rest @ ..] = arguments.as_slice()
        && command == "run-environment"
        && separator == "--"
    {
        return match cdenv_agent::run_with_environment(Path::new(snapshot), program, rest) {
            Ok(status) => status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .map_or(ExitCode::FAILURE, ExitCode::from),
            Err(error) => {
                eprintln!("cdenv-agent: {error}");
                ExitCode::FAILURE
            }
        };
    }

    let result = run_machine_command(&arguments);
    match result {
        Ok(output) => {
            println!("{output}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cdenv-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_machine_command(arguments: &[OsString]) -> Result<String, String> {
    cdenv_agent::ensure_supported_platform()
        .map_err(|error| error.to_string())
        .and_then(|()| match arguments {
            [command] if command == "version" => {
                serde_json::to_string(&cdenv_agent::version()).map_err(|error| error.to_string())
            }
            [command] if command == "identity" => cdenv_agent::identity()
                .map_err(|error| error.to_string())
                .and_then(|identity| {
                    serde_json::to_string(&identity).map_err(|error| error.to_string())
                }),
            [command] if command == "capture-environment" => capture_environment(),
            [command, manifest] if command == "provision" => provision(Path::new(manifest)),
            [command, path] if command == "cleanup-staging" => {
                cdenv_agent::cleanup_staging(&path.to_string_lossy())
                    .map(|()| "{}".to_owned())
                    .map_err(|error| error.to_string())
            }
            [command, manifest] if command == "update-user" => update_user(Path::new(manifest)),
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-runner" => {
                lifecycle_runner(Path::new(manifest))
            }
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-start" => {
                lifecycle_start(Path::new(manifest))
            }
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-inspect" => {
                lifecycle_inspect(Path::new(manifest))
            }
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-cancel" => {
                lifecycle_cancel(Path::new(manifest), Duration::from_secs(5))
            }
            #[cfg(target_os = "linux")]
            [command, manifest, timeout] if command == "lifecycle-cancel" => timeout
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|_| "invalid lifecycle cancellation timeout".to_owned())
                .and_then(|milliseconds| {
                    lifecycle_cancel(Path::new(manifest), Duration::from_millis(milliseconds))
                }),
            _ => Err(
                "Usage: cdenv-agent <version|identity|capture-environment|run-environment SNAPSHOT -- COMMAND [ARG...]|provision MANIFEST|cleanup-staging PATH|update-user MANIFEST|lifecycle-runner MANIFEST|lifecycle-start MANIFEST|lifecycle-inspect MANIFEST|lifecycle-cancel MANIFEST [TIMEOUT_MS]|post-attach MANIFEST|ssh-server --stdio HOST_KEY AUTHORIZED_KEY ENVIRONMENT WORKSPACE|forwarding-bridge HOST PORT BUILD_ID PROTOCOL>"
                    .to_owned(),
            ),
        })
}

#[cfg(target_os = "linux")]
async fn ssh_server(
    host_key: &Path,
    authorized_key: &Path,
    environment: &Path,
    workspace: &Path,
) -> ExitCode {
    let request = cdenv_agent::SshServerRequest {
        host_key: host_key.to_path_buf(),
        authorized_key: authorized_key.to_path_buf(),
        environment: environment.to_path_buf(),
        workspace: workspace.to_path_buf(),
    };
    let result = match cdenv_agent::SshServerConfig::load(&request) {
        Ok(config) => {
            let stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
            cdenv_agent::serve_ssh_stream(stream, config).await
        }
        Err(error) => Err(error),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cdenv-agent: SSH server failed: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn forwarding_bridge(host: &str, port: &str, build_id: &str, protocol: &str) -> ExitCode {
    let result = async {
        cdenv_agent::ensure_supported_platform().map_err(|error| error.to_string())?;
        let port = port
            .parse::<u16>()
            .map_err(|_| "invalid forwarding target port".to_owned())?;
        let protocol = protocol
            .parse::<u32>()
            .map_err(|_| "invalid forwarding protocol version".to_owned())?;
        cdenv_agent::verify_forwarding_identity(build_id, protocol)
            .map_err(|error| error.to_string())?;
        let target =
            cdenv_agent::ForwardTarget::new(host, port).map_err(|error| error.to_string())?;
        let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
        cdenv_agent::bridge_forwarding_stream(&mut stdio, &target)
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cdenv-agent: {error}");
            ExitCode::FAILURE
        }
    }
}

fn capture_environment() -> Result<String, String> {
    let mut contents = Vec::new();
    std::io::stdin()
        .take(MAXIMUM_CAPTURE_REQUEST_BYTES + 1)
        .read_to_end(&mut contents)
        .map_err(|_| "cannot read environment capture request".to_owned())?;
    if contents.len() as u64 > MAXIMUM_CAPTURE_REQUEST_BYTES {
        return Err("environment capture request exceeded its bound".to_owned());
    }
    let request = serde_json::from_slice(&contents)
        .map_err(|_| "invalid environment capture request".to_owned())?;
    let result = cdenv_agent::capture_environment(&request).map_err(|error| error.to_string())?;
    serde_json::to_string(&result)
        .map_err(|_| "cannot encode environment capture result".to_owned())
}

fn provision(manifest: &Path) -> Result<String, String> {
    let contents = read_manifest(manifest, "provision")?;
    let request = serde_json::from_slice(&contents)
        .map_err(|error| format!("invalid provision manifest: {error}"))?;
    let result = cdenv_agent::provision(&request).map_err(|error| error.to_string())?;
    serde_json::to_string(&result).map_err(|error| error.to_string())
}

fn update_user(manifest: &Path) -> Result<String, String> {
    let contents = read_manifest(manifest, "update-user")?;
    let request = serde_json::from_slice(&contents)
        .map_err(|error| format!("invalid update-user manifest: {error}"))?;
    cdenv_agent::update_user(&request).map_err(|error| error.to_string())?;
    Ok("{}".to_owned())
}

#[cfg(target_os = "linux")]
fn lifecycle_request(path: &Path) -> Result<cdenv_agent::LifecycleRunRequest, String> {
    let contents = read_manifest(path, "lifecycle")?;
    serde_json::from_slice(&contents).map_err(|_| "invalid lifecycle manifest".to_owned())
}

#[cfg(target_os = "linux")]
fn post_attach(path: &Path) -> Result<(), String> {
    cdenv_agent::run_post_attach(&lifecycle_request(path)?).map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn lifecycle_runner(path: &Path) -> Result<String, String> {
    let state =
        cdenv_agent::run_lifecycle(&lifecycle_request(path)?).map_err(|error| error.to_string())?;
    serde_json::to_string(&state).map_err(|_| "cannot encode lifecycle state".to_owned())
}

#[cfg(target_os = "linux")]
fn lifecycle_start(path: &Path) -> Result<String, String> {
    let request = lifecycle_request(path)?;
    match cdenv_agent::inspect_lifecycle(&request) {
        Ok(inspection)
            if inspection.runner_active
                || matches!(
                    inspection.state.phase,
                    cdenv_agent::LifecyclePhase::Complete
                        | cdenv_agent::LifecyclePhase::Failed
                        | cdenv_agent::LifecyclePhase::Indeterminate
                        | cdenv_agent::LifecyclePhase::Cancelled
                ) =>
        {
            return serde_json::to_string(&inspection)
                .map_err(|_| "cannot encode lifecycle state".to_owned());
        }
        Ok(_) => {}
        Err(cdenv_agent::LifecycleError::StateIo { source })
            if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
    }
    let executable = std::env::current_exe()
        .map_err(|error| format!("cannot locate lifecycle runner: {error}"))?;
    let child = Command::new(executable)
        .arg("lifecycle-runner")
        .arg(path)
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("cannot start lifecycle runner: {error}"))?;
    Ok(format!(r#"{{"runnerPid":{}}}"#, child.id()))
}

#[cfg(target_os = "linux")]
fn lifecycle_inspect(path: &Path) -> Result<String, String> {
    let inspection = cdenv_agent::inspect_lifecycle(&lifecycle_request(path)?)
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&inspection).map_err(|_| "cannot encode lifecycle state".to_owned())
}

#[cfg(target_os = "linux")]
fn lifecycle_cancel(path: &Path, timeout: Duration) -> Result<String, String> {
    let inspection = cdenv_agent::cancel_lifecycle(&lifecycle_request(path)?, timeout)
        .map_err(|error| error.to_string())?;
    serde_json::to_string(&inspection).map_err(|_| "cannot encode lifecycle state".to_owned())
}

fn read_manifest(path: &Path, operation: &str) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    let file = std::fs::File::open(path)
        .map_err(|error| format!("cannot read {operation} manifest: {error}"))?;
    file.take(MAXIMUM_MANIFEST_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {operation} manifest: {error}"))?;
    if bytes.len() as u64 > MAXIMUM_MANIFEST_BYTES {
        return Err(format!("{operation} manifest exceeded its bound"));
    }
    Ok(bytes)
}
