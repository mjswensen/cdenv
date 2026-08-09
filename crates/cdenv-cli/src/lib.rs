//! Testable host-application boundary for `cdenv`.
//!
//! The crate owns the complete V1 command grammar, application exit policy,
//! and output envelopes. Host adapters and command workflows are added behind
//! this boundary without moving parsing or rendering into the executable.

mod command_line;
mod error;
mod output;
mod paths;

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
pub use paths::{
    CDENV_HOME, CachePaths, CdenvRoot, ManagedPathError, ManagedPathKind, ManagedPathState,
    ProcessEnvironment, RequiredPathError, RootEnvironment, RootResolutionError, RootSource,
    SshPaths, WorkspacePaths, inspect_managed_path, validate_openssh_path, validate_required_path,
};

/// Resolves the process root once and invokes the selected command.
///
/// # Errors
///
/// Returns a root-resolution error or the selected workflow's application
/// error.
pub fn invoke(command_line: &CommandLine) -> Result<(), ApplicationError> {
    invoke_with_environment(command_line, &ProcessEnvironment)
}

/// Resolves the root once through an injectable environment and invokes the
/// selected command.
///
/// # Errors
///
/// Returns a root-resolution error or the selected workflow's application
/// error.
pub fn invoke_with_environment(
    command_line: &CommandLine,
    environment: &impl RootEnvironment,
) -> Result<(), ApplicationError> {
    let root = CdenvRoot::resolve(command_line.root(), environment)?;
    invoke_with_root(command_line, &root)
}

/// Invokes a command with its already-resolved root.
///
/// Command workflows receive this value rather than reading process globals.
///
/// # Errors
///
/// Returns [`ApplicationError::CommandUnavailable`] until the selected command
/// workflow is implemented by its corresponding implementation chunk.
pub fn invoke_with_root(
    command_line: &CommandLine,
    _root: &CdenvRoot,
) -> Result<(), ApplicationError> {
    Err(ApplicationError::CommandUnavailable {
        command: command_line.command().kind(),
    })
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::ffi::OsString;
    use std::path::PathBuf;

    use clap::Parser;

    use super::{ApplicationError, CommandLine, RootEnvironment, invoke_with_environment};

    struct CountingEnvironment {
        cdenv_home_reads: Cell<usize>,
        home_reads: Cell<usize>,
    }

    impl RootEnvironment for CountingEnvironment {
        fn cdenv_home(&self) -> Option<OsString> {
            self.cdenv_home_reads.set(self.cdenv_home_reads.get() + 1);
            Some(OsString::from("/isolated/cdenv"))
        }

        fn home_dir(&self) -> Option<PathBuf> {
            self.home_reads.set(self.home_reads.get() + 1);
            Some(PathBuf::from("/must/not/be/read"))
        }
    }

    #[test]
    fn application_wiring_resolves_the_root_exactly_once() {
        let command_line =
            CommandLine::try_parse_from(["cdenv", "list"]).expect("test command should parse");
        let environment = CountingEnvironment {
            cdenv_home_reads: Cell::new(0),
            home_reads: Cell::new(0),
        };

        let result = invoke_with_environment(&command_line, &environment);

        assert!(matches!(
            result,
            Err(ApplicationError::CommandUnavailable { .. })
        ));
        assert_eq!(
            (
                environment.cdenv_home_reads.get(),
                environment.home_reads.get()
            ),
            (1, 0)
        );
    }
}
