//! System OpenSSH wrapper for `cdenv ssh`.

use std::io;
use std::process::{Command, ExitStatus};

use thiserror::Error;

use crate::{CdenvRoot, SshArgs, system_ssh_arguments};

/// Failure to start or wait for the system OpenSSH client.
#[derive(Debug, Error)]
#[error("cannot run system OpenSSH client: {0}")]
pub struct SystemSshError(#[source] pub io::Error);

/// Runs `ssh -F <managed-config> <name>.cdenv ...` with inherited standard I/O.
///
/// Every remote argument remains a distinct operating-system argument and the
/// returned status is the system SSH process's exact status.
///
/// # Errors
///
/// Returns [`SystemSshError`] when the system `ssh` process cannot be spawned or
/// waited for.
pub fn run_system_ssh(root: &CdenvRoot, arguments: &SshArgs) -> Result<ExitStatus, SystemSshError> {
    Command::new("ssh")
        .args(system_ssh_arguments(
            root,
            &arguments.name,
            &arguments.remote_argv,
        ))
        .status()
        .map_err(SystemSshError)
}
