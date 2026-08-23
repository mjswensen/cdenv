//! Thin executable entry point for the cdenv container agent.

use std::io::Read;
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

const MAXIMUM_CAPTURE_REQUEST_BYTES: u64 = 4 * 1024 * 1024;
const MAXIMUM_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
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

    let result = cdenv_agent::ensure_supported_platform()
        .map_err(|error| error.to_string())
        .and_then(|()| match arguments.as_slice() {
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
            [command, manifest] if command == "lifecycle-runner" => lifecycle_runner(Path::new(manifest)),
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-start" => lifecycle_start(Path::new(manifest)),
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-inspect" => lifecycle_inspect(Path::new(manifest)),
            #[cfg(target_os = "linux")]
            [command, manifest] if command == "lifecycle-cancel" => {
                lifecycle_cancel(Path::new(manifest), Duration::from_secs(5))
            }
            #[cfg(target_os = "linux")]
            [command, manifest, timeout] if command == "lifecycle-cancel" => timeout
                .to_string_lossy()
                .parse::<u64>()
                .map_err(|_| "invalid lifecycle cancellation timeout".to_owned())
                .and_then(|milliseconds| lifecycle_cancel(Path::new(manifest), Duration::from_millis(milliseconds))),
            _ => Err(
                "Usage: cdenv-agent <version|identity|capture-environment|run-environment SNAPSHOT -- COMMAND [ARG...]|provision MANIFEST|cleanup-staging PATH|update-user MANIFEST|lifecycle-runner MANIFEST|lifecycle-start MANIFEST|lifecycle-inspect MANIFEST|lifecycle-cancel MANIFEST [TIMEOUT_MS]>"
                    .to_owned(),
            ),
        });
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
