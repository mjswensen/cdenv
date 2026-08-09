//! Application-boundary errors and process exit policy.

use std::process::ExitCode;

use thiserror::Error;

use crate::CommandKind;

/// A typed failure returned by the host application boundary.
///
/// Adapter and domain crates retain their focused error types. This boundary
/// maps only user-visible application failures to machine codes and process
/// status.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplicationError {
    /// Parsing exists, but this implementation stage has no command workflow.
    #[error("command `{command}` is not implemented in this build")]
    CommandUnavailable {
        /// The parsed command without an installed workflow.
        command: CommandKind,
    },
}

impl ApplicationError {
    /// Returns the stable code included in a machine-readable error.
    #[must_use]
    pub const fn machine_code(&self) -> &'static str {
        match self {
            Self::CommandUnavailable { .. } => "command_unavailable",
        }
    }

    /// Maps the application failure to its process exit status.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::CommandUnavailable { .. } => ExitCode::FAILURE,
        }
    }
}
