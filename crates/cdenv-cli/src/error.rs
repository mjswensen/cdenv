//! Application-boundary errors and process exit policy.

use std::process::ExitCode;

use thiserror::Error;

use crate::{CommandKind, RootResolutionError};

/// A typed failure returned by the host application boundary.
///
/// Adapter and domain crates retain their focused error types. This boundary
/// maps only user-visible application failures to machine codes and process
/// status.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ApplicationError {
    /// The process could not select a valid cdenv root.
    #[error(transparent)]
    RootResolution(#[from] RootResolutionError),
    /// Checkout creation failed with a credential-safe typed summary.
    #[error("create failed: {message}")]
    CreateFailed {
        /// Sanitized create-transaction message.
        message: String,
    },
    /// Explicit Feature lock generation failed.
    #[error("lock failed: {message}")]
    LockFailed {
        /// Credential-safe failure summary.
        message: String,
    },
    /// Local workspace enumeration failed for `list`.
    #[error("list failed: {message}")]
    ListFailed {
        /// Safe read-only reporting diagnostic.
        message: String,
    },
    /// Requested workspace status could not be established.
    #[error("status failed: {message}")]
    StatusFailed {
        /// Safe read-only reporting diagnostic.
        message: String,
    },
    /// Managed SSH identity, configuration, or Include setup failed.
    #[error("SSH setup failed: {message}")]
    SshSetupFailed {
        /// Credential-safe setup failure summary.
        message: String,
    },
    /// Explicit credential permission management failed safely.
    #[error("credentials failed: {message}")]
    CredentialsFailed {
        /// Value-free policy, storage, or readiness diagnostic.
        message: String,
    },
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
            Self::RootResolution(_) => "root_resolution_failed",
            Self::CreateFailed { .. } => "create_failed",
            Self::LockFailed { .. } => "lock_failed",
            Self::ListFailed { .. } => "list_failed",
            Self::StatusFailed { .. } => "status_failed",
            Self::SshSetupFailed { .. } => "ssh_setup_failed",
            Self::CredentialsFailed { .. } => "credentials_failed",
            Self::CommandUnavailable { .. } => "command_unavailable",
        }
    }

    /// Maps the application failure to its process exit status.
    #[must_use]
    pub const fn exit_code(&self) -> ExitCode {
        match self {
            Self::RootResolution(_)
            | Self::CreateFailed { .. }
            | Self::LockFailed { .. }
            | Self::ListFailed { .. }
            | Self::StatusFailed { .. }
            | Self::SshSetupFailed { .. }
            | Self::CredentialsFailed { .. }
            | Self::CommandUnavailable { .. } => ExitCode::FAILURE,
        }
    }
}
