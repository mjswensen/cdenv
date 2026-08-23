//! Thin executable entry point for the cdenv container agent.

use std::io::Read;
use std::path::Path;
use std::process::ExitCode;

const MAXIMUM_CAPTURE_REQUEST_BYTES: u64 = 4 * 1024 * 1024;

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
            _ => Err(
                "Usage: cdenv-agent <version|identity|capture-environment|run-environment SNAPSHOT -- COMMAND [ARG...]|provision MANIFEST|cleanup-staging PATH|update-user MANIFEST>"
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

fn read_manifest(path: &Path, operation: &str) -> Result<Vec<u8>, String> {
    std::fs::read(path).map_err(|error| format!("cannot read {operation} manifest: {error}"))
}
