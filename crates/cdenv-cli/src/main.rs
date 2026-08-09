//! Thin executable entry point for the cdenv host application.

use std::io;
use std::process::ExitCode;

use cdenv_cli::{CommandLine, invoke, render_application_result};
use clap::Parser;

fn main() -> ExitCode {
    let command_line = CommandLine::parse();
    let output_format = command_line.output_format();
    let result = invoke(&command_line);

    render_application_result(
        output_format,
        result,
        &mut io::stdout().lock(),
        &mut io::stderr().lock(),
    )
}
