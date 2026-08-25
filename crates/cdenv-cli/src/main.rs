//! Thin executable entry point for the cdenv host application.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cdenv_cli::{
    ApplicationError, CdenvRoot, CliCommand, CommandLine, Installation, ProcessEnvironment,
    ProcessSshConsentInteraction, apply_ssh_include_consent, enumerate_workspaces, invoke,
    regenerate_managed_ssh, render_application_result, render_reporting_application,
    resolve_current_executable, run_private_supervisor_manifest, run_system_ssh,
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
    if let CliCommand::Ssh(ssh_arguments) = command_line.command() {
        let root = match CdenvRoot::resolve(command_line.root(), &ProcessEnvironment) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("cdenv: {error}");
                return ExitCode::FAILURE;
            }
        };
        return match run_system_ssh(&root, ssh_arguments) {
            Ok(status) => ssh_exit_code(status),
            Err(error) => {
                eprintln!("cdenv: {error}");
                ExitCode::FAILURE
            }
        };
    }
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if let Some(exit_code) = render_reporting_application(&command_line, &mut stdout, &mut stderr) {
        return exit_code;
    }
    let mut result = invoke(&command_line);
    if result.is_ok()
        && matches!(
            command_line.command(),
            CliCommand::Create(_) | CliCommand::Up(_) | CliCommand::Rebuild(_)
        )
    {
        result = complete_ssh_setup(
            &command_line,
            arguments
                .first()
                .map_or_else(|| OsStr::new("cdenv"), AsRef::as_ref),
        );
    }

    render_application_result(output_format, result, &mut stdout, &mut stderr)
}

fn complete_ssh_setup(command_line: &CommandLine, argv0: &OsStr) -> Result<(), ApplicationError> {
    let root = CdenvRoot::resolve(command_line.root(), &ProcessEnvironment)?;
    let executable =
        resolve_current_executable(argv0).map_err(|error| ApplicationError::SshSetupFailed {
            message: error.to_string(),
        })?;
    let entries =
        enumerate_workspaces(&root).map_err(|error| ApplicationError::SshSetupFailed {
            message: error.to_string(),
        })?;
    let workspaces = entries
        .iter()
        .filter_map(|entry| entry.name().parse().ok())
        .collect::<Vec<cdenv_core::WorkspaceName>>();
    regenerate_managed_ssh(&root, &executable, workspaces).map_err(|error| {
        ApplicationError::SshSetupFailed {
            message: error.to_string(),
        }
    })?;

    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        ApplicationError::SshSetupFailed {
            message: "cannot configure the optional SSH Include because HOME is unavailable"
                .to_owned(),
        }
    })?;
    let mut installation =
        Installation::open_or_create(&root).map_err(|error| ApplicationError::SshSetupFailed {
            message: error.to_string(),
        })?;
    apply_ssh_include_consent(
        &root,
        &mut installation,
        command_line.ssh_config_consent(),
        &home,
        &mut ProcessSshConsentInteraction,
    )
    .map(|_| ())
    .map_err(|error| ApplicationError::SshSetupFailed {
        message: error.to_string(),
    })
}

fn ssh_exit_code(status: std::process::ExitStatus) -> ExitCode {
    if let Some(code) = status.code() {
        return ExitCode::from(u8::try_from(code).unwrap_or(u8::MAX));
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return ExitCode::from(u8::try_from(128_i32.saturating_add(signal)).unwrap_or(u8::MAX));
        }
    }
    ExitCode::FAILURE
}
