//! Thin executable entry point for the cdenv container agent.

use std::process::ExitCode;

fn main() -> ExitCode {
    match cdenv_agent::ensure_supported_platform() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cdenv-agent: {error}");
            ExitCode::FAILURE
        }
    }
}
