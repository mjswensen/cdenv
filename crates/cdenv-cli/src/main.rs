//! Thin executable entry point for the cdenv host application.

use std::io;
use std::process::ExitCode;

use cdenv_cli::{CommandLine, invoke, render_application_result, render_reporting_application};
use clap::Parser;

fn main() -> ExitCode {
    let command_line = CommandLine::parse();
    let output_format = command_line.output_format();
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if let Some(exit_code) = render_reporting_application(&command_line, &mut stdout, &mut stderr) {
        return exit_code;
    }
    let result = invoke(&command_line);

    render_application_result(output_format, result, &mut stdout, &mut stderr)
}
