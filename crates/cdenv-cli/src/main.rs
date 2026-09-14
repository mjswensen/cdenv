//! Thin executable entry point for the cdenv host application.

use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cdenv_cli::{
    ApplicationError, CancellationToken, CdenvRoot, CliCommand, CommandLine, Installation,
    ProcessEnvironment, ProcessSshConsentInteraction, apply_ssh_include_consent,
    enumerate_workspaces, invoke, preflight_forward, regenerate_managed_ssh,
    render_application_result, render_reporting_application, resolve_current_executable,
    run_private_supervisor_manifest, run_proxy_stdio, run_system_forward, run_system_ssh,
};
use clap::Parser;

fn main() -> ExitCode {
    let arguments = std::env::args_os().collect::<Vec<_>>();
    if let [_, command] = arguments.as_slice()
        && command == "__validate-artifacts"
    {
        return validate_package_artifacts();
    }
    if let [_, command, manifest] = arguments.as_slice()
        && command == "__forwarding-supervisor"
    {
        return private_supervisor_exit(Path::new(manifest));
    }
    let command_line = CommandLine::parse();
    let output_format = command_line.output_format();
    if let CliCommand::Proxy(proxy_arguments) = command_line.command() {
        let root = match CdenvRoot::resolve(command_line.root(), &ProcessEnvironment) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("cdenv: {error}");
                return ExitCode::FAILURE;
            }
        };
        return proxy_exit(&root, proxy_arguments.workspace.workspace_name());
    }
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
    if let CliCommand::Forward(forward_arguments) = command_line.command() {
        let root = match CdenvRoot::resolve(command_line.root(), &ProcessEnvironment) {
            Ok(root) => root,
            Err(error) => {
                eprintln!("cdenv: {error}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = preflight_forward(&root, forward_arguments) {
            eprintln!("cdenv: {error}");
            return ExitCode::FAILURE;
        }
        if !forward_arguments.bind.is_loopback() {
            eprintln!(
                "cdenv: warning: forwarding is exposed on non-loopback address {}; connections may be reachable by other hosts",
                forward_arguments.bind
            );
        }
        return match run_system_forward(&root, forward_arguments) {
            Ok(status) => ssh_exit_code(status),
            Err(error) => {
                eprintln!("cdenv: {error}");
                ExitCode::FAILURE
            }
        };
    }
    if matches!(command_line.command(), CliCommand::Credentials(_)) {
        return credentials_exit(&command_line);
    }
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    if let Some(exit_code) = render_reporting_application(&command_line, &mut stdout, &mut stderr) {
        return exit_code;
    }
    // Workflows may mirror child diagnostics from helper threads. Do not hold
    // the process-wide stdio locks while those threads are running.
    drop(stdout);
    drop(stderr);
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

    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    render_application_result(output_format, result, &mut stdout, &mut stderr)
}

fn credentials_exit(command_line: &CommandLine) -> ExitCode {
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    match CdenvRoot::resolve(command_line.root(), &ProcessEnvironment) {
        Ok(root) => {
            cdenv_cli::render_credentials_application(command_line, &root, &mut stdout, &mut stderr)
        }
        Err(error) => render_application_result(
            command_line.output_format(),
            Err(error.into()),
            &mut stdout,
            &mut stderr,
        ),
    }
}

fn private_supervisor_exit(manifest: &Path) -> ExitCode {
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
    match runtime.block_on(run_private_supervisor_manifest(manifest)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("cdenv: private forwarding supervisor failed: {error}");
            ExitCode::FAILURE
        }
    }
}

// Deliberately bypass installation discovery: package validation needs no Docker,
// home directory, consent, or initialized cdenv root.
fn validate_package_artifacts() -> ExitCode {
    use cdenv_cli::AgentArtifactProvider;
    use cdenv_core::ContainerArchitecture;
    use sha2::{Digest, Sha256};

    let result = (|| {
        let build_id = AgentArtifactProvider::embedded_identity().map_err(|e| e.to_string())?;
        let mut agents = serde_json::Map::new();
        for (name, architecture) in [
            ("aarch64", ContainerArchitecture::Aarch64),
            ("x86_64", ContainerArchitecture::X86_64),
        ] {
            let bytes = AgentArtifactProvider::embedded()
                .artifact(architecture)
                .map_err(|e| e.to_string())?;
            agents.insert(
                name.to_owned(),
                serde_json::json!(format!("{:x}", Sha256::digest(bytes))),
            );
        }
        Ok::<_, String>(serde_json::json!({
            "schemaVersion": 1,
            "version": env!("CARGO_PKG_VERSION"),
            "buildId": build_id.as_str(),
            "protocolVersion": 1,
            "agents": agents,
        }))
    })();
    match result {
        Ok(report) => {
            println!("{report}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cdenv: artifact validation failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn proxy_exit(root: &CdenvRoot, workspace: &cdenv_core::WorkspaceName) -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("cdenv: cannot start proxy runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    let cancellation = CancellationToken::default();
    let signal_cancellation = cancellation.clone();
    runtime.block_on(async {
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_cancellation.cancel();
            }
        });
        let result = run_proxy_stdio(root, workspace, &cancellation).await;
        signal.abort();
        match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("cdenv: {error}");
                ExitCode::FAILURE
            }
        }
    })
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
