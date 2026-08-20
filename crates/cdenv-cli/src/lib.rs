//! Testable host-application boundary for `cdenv`.
//!
//! The crate owns the complete V1 command grammar, application exit policy,
//! and output envelopes. Host adapters and command workflows are added behind
//! this boundary without moving parsing or rendering into the executable.

mod command_line;
mod config;
mod create;
mod docker;
mod docker_cli;
mod error;
mod git;
mod installation;
mod locking;
mod output;
mod paths;
mod process;
mod state;
mod storage;
mod workspace_registry;

pub use command_line::{
    CliCommand, CommandKind, CommandLine, CreateArgs, DoctorArgs, DownArgs, ForwardArgs,
    ForwardMapping, ForwardMappingError, ListArgs, LockArgs, OutputFormat, ProxyArgs, RebuildArgs,
    RepoRelativeConfigPath, RepoRelativeConfigPathError, SshArgs, SshConfigConsent, StatusArgs,
    UpArgs, WorkspaceSelector, WorkspaceSelectorError,
};
pub use config::{ConfigLoadError, ConfigSource, discover_and_read_config};
pub use create::{
    ConfigContainmentError, CreateWorkspaceError, CreateWorkspaceRequest, CreatedWorkspace,
    create_workspace, validate_explicit_config,
};
pub use docker::{
    ApiVersion, BollardConnector, BollardConnectorError, DOCKER_PROBE_TIMEOUT, DockerCapabilities,
    DockerCommandProbe, DockerEndpoint, DockerEndpointError, DockerEnvironment, DockerProbeError,
    DockerSocketProbe, FileSystemDockerSocketProbe, MINIMUM_COMPOSE, MINIMUM_DOCKER_API,
    MINIMUM_DOCKER_CLI, MINIMUM_DOCKER_ENGINE, ProcessDockerEnvironment, Version,
};
pub use docker_cli::{
    DEFAULT_MAXIMUM_CONTEXT_BYTES, DEFAULT_MAXIMUM_CONTEXT_ENTRIES,
    DEFAULT_MAXIMUM_GENERATED_BYTES, DockerBuildClaim, DockerBuildContext, DockerBuildRequest,
    DockerCliAdapter, DockerCliError, DockerContextLimits, DockerCreateClaim, DockerCreateRequest,
    DockerPullClaim, DockerResourceIdentity, DockerfileInput, GeneratedContextFile, ImageId,
    build_arguments, create_arguments, pull_arguments,
};
pub use error::ApplicationError;
pub use git::{GitAdapter, GitError, GitVersion, OperationLogError};
pub use installation::{
    FingerprintKey, FingerprintKeyState, FingerprintKeyUnknownReason, INSTALLATION_SCHEMA_VERSION,
    Installation, InstallationError, InstallationRecord, KeyedDigest, KeyedDigestError,
    PlanFingerprintCategory, SshIncludeConsent,
};
pub use locking::{
    AttachSetupError, LockBehavior, LockError, LockGuard, LockMode, ensure_lock_file,
    with_shared_lock_for_attach,
};
pub use output::{
    ErrorDetail, ErrorEnvelope, JSON_SCHEMA_VERSION, OutputRenderError, OutputWarning,
    SuccessEnvelope, render_application_result, render_json_error, render_json_success,
};
pub use paths::{
    CDENV_HOME, CachePaths, CdenvRoot, ManagedPathError, ManagedPathKind, ManagedPathState,
    ProcessEnvironment, RequiredPathError, RootEnvironment, RootResolutionError, RootSource,
    SshPaths, WorkspacePaths, inspect_managed_path, validate_openssh_path, validate_required_path,
};
pub use process::{
    CancellationToken, CapturedOutput, OperationId, ProcessDeadline, ProcessEnvironmentVariable,
    ProcessError, ProcessRequest, ProcessResult, ProcessRunner,
};
pub use state::{
    ActiveForwarding, ActiveGeneration, ActiveScenario, DeclaredForward, DesiredConfigPath,
    DesiredConfigPathError, ForwardProtocol, LifecycleCheckpoint, LifecycleStage,
    LoadedWorkspaceState, MigrationStatus, OperationState, OperationStateError, PlanFingerprints,
    ProvisionedState, SanitizedRepositorySource, SanitizedSummary, StateTimestamp,
    StateTimestampError, WORKSPACE_STATE_SCHEMA_VERSION, WorkspaceState, WorkspaceStateError,
    decode_workspace_state, load_workspace_state, persist_workspace_state,
};
pub use storage::{
    AtomicWriteStage, ManagedMode, StorageError, atomic_write, ensure_private_directory,
    tighten_managed_file,
};
pub use workspace_registry::{
    EnumeratedWorkspace, EnumerationError, PersistedOperationStatus, ReservationError,
    SupervisorRuntimeInspection, WorkspaceEntryStatus, WorkspaceReservation,
    classify_persisted_operation, enumerate_workspaces, inspect_supervisor_runtime,
    reserve_workspace,
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
/// Runs checkout creation for `create`. Other parsed commands return
/// [`ApplicationError::CommandUnavailable`] until their workflow chunk lands.
pub fn invoke_with_root(
    command_line: &CommandLine,
    root: &CdenvRoot,
) -> Result<(), ApplicationError> {
    match command_line.command() {
        CliCommand::Create(arguments) => create_workspace(
            root,
            CreateWorkspaceRequest {
                source: &arguments.git_source,
                name: arguments.name.as_ref(),
                config: arguments.config.as_ref(),
            },
            &GitAdapter::system(),
            &CancellationToken::default(),
        )
        .map(|_| ())
        .map_err(|error| ApplicationError::CreateFailed {
            message: error.to_string(),
        }),
        command => Err(ApplicationError::CommandUnavailable {
            command: command.kind(),
        }),
    }
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
