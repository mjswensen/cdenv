//! Thin executable entry point for the cdenv container agent.

use std::path::Path;
use std::process::ExitCode;

fn main() -> ExitCode {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
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
            [command, manifest] if command == "provision" => provision(Path::new(manifest)),
            [command, manifest] if command == "update-user" => update_user(Path::new(manifest)),
            _ => Err(
                "Usage: cdenv-agent <version|identity|provision MANIFEST|update-user MANIFEST>"
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

fn provision(manifest: &Path) -> Result<String, String> {
    let contents = read_manifest(manifest, "provision")?;
    let request = serde_json::from_slice(&contents)
        .map_err(|error| format!("invalid provision manifest: {error}"))?;
    cdenv_agent::provision(&request).map_err(|error| error.to_string())?;
    Ok("{}".to_owned())
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
