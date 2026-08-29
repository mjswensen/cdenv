//! Foreground system-OpenSSH wrapper for ad-hoc local forwarding.

use std::ffi::OsString;
use std::io;
use std::net::{SocketAddr, TcpListener};
use std::process::{Command, ExitStatus};

use std::collections::BTreeSet;

use cdenv_core::WorkspaceHost;
use thiserror::Error;

use crate::{CdenvRoot, ForwardArgs, load_workspace_state};

/// Failure to start or wait for the system OpenSSH forwarding client.
#[derive(Debug, Error)]
#[error("cannot run system OpenSSH forwarding client: {0}")]
pub struct SystemForwardError(#[source] pub io::Error);

/// A requested ad-hoc listener cannot be started safely.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ForwardPreflightError {
    /// The workspace state needed to detect declared listener conflicts was unavailable.
    #[error("cannot inspect workspace forwarding state: {0}")]
    State(String),
    /// An ad-hoc listener would collide with another requested or declared endpoint.
    #[error(
        "forwarding listener port {port} conflicts with an existing requested or declared mapping"
    )]
    Conflict {
        /// The conflicting local port.
        port: u16,
    },
    /// The operating system could not reserve a requested listener.
    #[error("cannot bind forwarding listener {address}: {source}")]
    Bind {
        /// Requested socket address.
        address: SocketAddr,
        /// Binding failure.
        #[source]
        source: io::Error,
    },
}

/// Checks every requested listener before OpenSSH starts any forwarding.
///
/// Reservations are held together for the duration of the check and released
/// only after all mappings proved viable. OpenSSH immediately rebinds them.
///
/// # Errors
///
/// Returns state, conflict, or socket reservation errors without starting SSH.
pub fn preflight_forward(
    root: &CdenvRoot,
    arguments: &ForwardArgs,
) -> Result<(), ForwardPreflightError> {
    let state = load_workspace_state(&root.workspace(&arguments.name).state_file())
        .map_err(|error| ForwardPreflightError::State(error.to_string()))?;
    let mut occupied = BTreeSet::new();
    if let Some(active) = state.state().active() {
        for endpoint in active.forwarding().assigned() {
            if let Some(assigned) = endpoint.assigned() {
                occupied.insert(assigned.port().get());
            }
        }
    }
    let mut requested = BTreeSet::new();
    let mut reservations = Vec::with_capacity(arguments.mappings.len());
    for mapping in &arguments.mappings {
        let port = mapping.local_port().get();
        if !requested.insert(port) || occupied.contains(&port) {
            return Err(ForwardPreflightError::Conflict { port });
        }
        let address = SocketAddr::new(arguments.bind, port);
        reservations.push(
            TcpListener::bind(address)
                .map_err(|source| ForwardPreflightError::Bind { address, source })?,
        );
    }
    drop(reservations);
    Ok(())
}

/// Builds the exact system-OpenSSH arguments for foreground local forwarding.
#[must_use]
pub fn system_forward_arguments(root: &CdenvRoot, arguments: &ForwardArgs) -> Vec<OsString> {
    let mut output = vec![
        OsString::from("-F"),
        root.ssh().config().into_os_string(),
        OsString::from("-N"),
        OsString::from("-o"),
        OsString::from("ExitOnForwardFailure=yes"),
    ];
    for mapping in &arguments.mappings {
        output.push(OsString::from("-L"));
        output.push(OsString::from(format!(
            "{}:{}:localhost:{}",
            arguments.bind,
            mapping.local_port(),
            mapping.container_port()
        )));
    }
    output.push(OsString::from(
        WorkspaceHost::from_workspace_name(arguments.name.clone()).to_string(),
    ));
    output
}

/// Runs foreground OpenSSH local forwarding with inherited standard I/O.
///
/// # Errors
///
/// Returns [`SystemForwardError`] when OpenSSH cannot be spawned or waited for.
pub fn run_system_forward(
    root: &CdenvRoot,
    arguments: &ForwardArgs,
) -> Result<ExitStatus, SystemForwardError> {
    Command::new("ssh")
        .args(system_forward_arguments(root, arguments))
        .status()
        .map_err(SystemForwardError)
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use cdenv_core::{TcpPort, WorkspaceName};

    use super::*;
    use crate::{ForwardMapping, RootEnvironment};

    struct Environment;

    impl RootEnvironment for Environment {
        fn cdenv_home(&self) -> Option<OsString> {
            Some(OsString::from("/tmp/cdenv"))
        }

        fn home_dir(&self) -> Option<std::path::PathBuf> {
            None
        }
    }

    #[test]
    fn arguments_use_loopback_and_one_local_forward_per_mapping() {
        let root = CdenvRoot::resolve(None, &Environment).expect("root");
        let args = ForwardArgs {
            name: WorkspaceName::parse("demo").expect("workspace"),
            mappings: vec![
                ForwardMapping::new(
                    TcpPort::new(3000).expect("port"),
                    TcpPort::new(8080).expect("port"),
                ),
                ForwardMapping::new(
                    TcpPort::new(5432).expect("port"),
                    TcpPort::new(5432).expect("port"),
                ),
            ],
            bind: IpAddr::V4(Ipv4Addr::LOCALHOST),
        };

        assert_eq!(
            system_forward_arguments(&root, &args),
            [
                "-F",
                "/tmp/cdenv/ssh/config",
                "-N",
                "-o",
                "ExitOnForwardFailure=yes",
                "-L",
                "127.0.0.1:3000:localhost:8080",
                "-L",
                "127.0.0.1:5432:localhost:5432",
                "demo.cdenv"
            ]
            .map(OsString::from)
        );
    }
}
