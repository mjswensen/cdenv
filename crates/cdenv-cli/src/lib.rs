//! Testable host-application boundary for `cdenv`.
//!
//! The crate owns the complete V1 command grammar, application exit policy,
//! and output envelopes. Host adapters and command workflows are added behind
//! this boundary without moving parsing or rendering into the executable.

mod command_line;
mod error;
mod output;

pub use command_line::{
    CliCommand, CommandKind, CommandLine, CreateArgs, DoctorArgs, DownArgs, ForwardArgs,
    ForwardMapping, ForwardMappingError, ListArgs, LockArgs, OutputFormat, ProxyArgs, RebuildArgs,
    RepoRelativeConfigPath, RepoRelativeConfigPathError, SshArgs, SshConfigConsent, StatusArgs,
    UpArgs, WorkspaceSelector, WorkspaceSelectorError,
};
pub use error::ApplicationError;
pub use output::{
    ErrorDetail, ErrorEnvelope, JSON_SCHEMA_VERSION, OutputRenderError, OutputWarning,
    SuccessEnvelope, render_application_result, render_json_error, render_json_success,
};

/// Invokes the selected command through the host application boundary.
///
/// This foundation deliberately installs no adapter or workflow handlers; a
/// parsed command therefore produces a typed unavailable-command error rather
/// than silently succeeding.
///
/// # Errors
///
/// Returns [`ApplicationError::CommandUnavailable`] until the selected command
/// workflow is implemented by its corresponding implementation chunk.
pub fn invoke(command_line: &CommandLine) -> Result<(), ApplicationError> {
    Err(ApplicationError::CommandUnavailable {
        command: command_line.command().kind(),
    })
}
