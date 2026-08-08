//! Thin executable entry point for the cdenv host application.

use std::process::ExitCode;

fn main() -> ExitCode {
    cdenv_cli::run()
}
