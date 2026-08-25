//! Thin executable entry point for the cdenv host application.

use std::io;
use std::path::Path;
use std::process::ExitCode;

use cdenv_cli::{
    CommandLine, invoke, render_application_result, render_reporting_application,
    run_private_supervisor_manifest,
};
use clap::Parser;

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let [_, command, manifest] = arguments.as_slice()
        && command == "__forwarding-supervisor"
    {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("cdenv: cannot start private supervisor runtime: {error}");
                return ExitCode::FAILURE;
            }
        };
        return match runtime.block_on(run_private_supervisor_manifest(Path::new(manifest))) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cdenv: private forwarding supervisor failed: {error}");
                ExitCode::FAILURE
            }
        };
    }
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
