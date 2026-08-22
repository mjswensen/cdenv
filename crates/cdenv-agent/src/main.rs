//! Thin executable entry point for the cdenv container agent.

use std::process::ExitCode;

fn main() -> ExitCode {
    if std::env::args_os().nth(1).as_deref() != Some(std::ffi::OsStr::new("version"))
        || std::env::args_os().nth(2).is_some()
    {
        eprintln!("Usage: cdenv-agent version");
        return ExitCode::FAILURE;
    }

    match cdenv_agent::ensure_supported_platform() {
        Ok(()) => match serde_json::to_string(&cdenv_agent::version()) {
            Ok(version) => {
                println!("{version}");
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("cdenv-agent: failed to encode version: {error}");
                ExitCode::FAILURE
            }
        },
        Err(error) => {
            eprintln!("cdenv-agent: {error}");
            ExitCode::FAILURE
        }
    }
}
