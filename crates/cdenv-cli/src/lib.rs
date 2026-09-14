//! Testable host-application boundary for `cdenv`.
//!
//! The crate owns the complete V1 command grammar, application exit policy,
//! and output envelopes. Host adapters and command workflows are added behind
//! this boundary without moving parsing or rendering into the executable.

use std::collections::BTreeSet;
use std::io::Write;
use std::process::ExitCode;

use serde::Serialize;

mod agent_artifacts;
mod agent_environment;
mod agent_provisioning;
mod bollard;
mod command_line;
mod compose;
mod compose_lifecycle;
mod compose_rebuild;
mod config;
mod create;
mod credential_broker;
mod credentials;
mod docker;
mod docker_cli;
mod doctor;
mod down;
mod error;
mod feature_lock;
mod feature_sources;
mod forward_command;
mod forwarding;
mod forwarding_reconciliation;
mod generated_image;
mod git;
mod host_git_credentials;
mod image_orchestration;
mod installation;
mod lifecycle_orchestration;
mod locking;
mod output;
mod paths;
mod process;
mod production;
mod proxy;
mod rebuild;
mod reconciliation;
mod reporting;
mod ssh_command;
mod ssh_config;
mod ssh_consent;
mod ssh_identity;
mod state;
mod storage;
mod workspace_registry;

pub use agent_artifacts::{AgentArtifactError, AgentArtifactIdentity, AgentArtifactProvider};
pub use agent_environment::{
    AgentEnvironmentCapturer, AgentEnvironmentError, AgentEnvironmentRequest, ReadinessEnvironment,
    SshEnvironment,
};
pub use agent_provisioning::{
    AgentProvisionAsset, AgentProvisionRequest, AgentProvisionTransportError, AgentProvisioner,
    AgentProvisioningError, AgentProvisioningFacts, RemoteAgentIdentity,
};
pub use bollard::{
    AttachedExec, BOLLARD_CONTROL_TIMEOUT, BollardAdapter, BollardAdapterError,
    ComposePrimaryExpectation, ContainerDiscoveryScope, ContainerExpectation, ContainerInspection,
    CorrelatedContainers, DetachedExec, DiscoveredContainer, ExecCommand, ExecId, ExecInspect,
    ExecStreamError, ImageCleanupExpectation, ImageInspection, InspectedMount,
    InspectedPortBinding, MAXIMUM_EXEC_FRAME_BYTES, WorkspaceCorrelation, correlate_containers,
    decode_docker_multiplexed, verify_port_bindings,
};
pub use command_line::{
    CliCommand, CommandKind, CommandLine, CreateArgs, CredentialEnable, CredentialOriginCapability,
    CredentialOriginsArgs, CredentialsArgs, CredentialsCommand, DoctorArgs, DownArgs, ForwardArgs,
    ForwardMapping, ForwardMappingError, ListArgs, LockArgs, OutputFormat, ProxyArgs, RebuildArgs,
    RepoRelativeConfigPath, RepoRelativeConfigPathError, SshArgs, SshConfigConsent, StatusArgs,
    UpArgs, WorkspaceSelector, WorkspaceSelectorError,
};
pub use compose::{
    ComposeAdapter, ComposeAdapterError, ComposeBaseClaim, ComposeBaseRequest, ComposeProject,
    ComposeStopRequest, ComposeUpClaim, ComposeUpRequest, compose_arguments,
};
pub use compose_lifecycle::{
    ComposeLifecycleError, ComposeLifecycleFacts, ComposeLifecycleOrchestrator,
    ComposeServiceSetState, ComposeStopOutcome, CreateComposeRequest, RecordedComposeRequest,
    classify_compose_service_set,
};
pub use compose_rebuild::{
    ComposeCleanupKind, ComposeCleanupWarning, ComposePartialEvidence, ComposePartialService,
    ComposePartialState, ComposeRebuildError, ComposeRebuildInvariantError,
    ComposeRebuildOrchestrator, ComposeRebuildOutcome, ComposeRebuildPhase, ComposeRebuildRecovery,
    ComposeRebuildRequest, ComposeRebuildRuntime, ComposeServiceHealth, ObservedComposeService,
    classify_compose_partial_state,
};
pub use config::{ConfigLoadError, ConfigSource, discover_and_read_config};
pub use create::{
    ConfigContainmentError, CreateWorkspaceError, CreateWorkspaceRequest, CreatedWorkspace,
    create_workspace, validate_explicit_config,
};
pub use credential_broker::{
    BrokerBackendError, BrokerBackendFuture, BrokerByteStream, CredentialBrokerBackend,
    HostCredentialBrokerError, credential_retry_delay, serve_host_credential_broker,
};
pub use credentials::{
    CREDENTIAL_PERMISSION_SCHEMA, CredentialCommandError, CredentialPermissionState,
    CredentialStatusReport, credential_status, mutate_credentials, render_credentials_application,
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
pub use doctor::{DoctorCheck, DoctorOutcome, DoctorReport, doctor_report, render_doctor_report};
pub use down::{
    CONTAINER_STOP_GRACE, DownError, DownOutcome, DownRequest, DownWarning, EnvironmentStopOutcome,
    ForwardingStopOutcome, ForwardingSupervisorStop, InterruptedOperationRecovery,
    LIFECYCLE_STOP_GRACE, LifecycleRunnerStop, LifecycleStopOutcome, ManagedEnvironmentStop,
    StopFailures, classify_interrupted_operation, down_workspace,
};
pub use error::ApplicationError;
pub use feature_lock::{
    ExistingContainerLockStatus, FEATURE_LOCK_FILE, FeatureLockError, generate_feature_lock,
    inspect_existing_container_lock, lock_workspace, resolve_frozen_features_offline,
};
pub use feature_sources::{
    FeatureSourceError, FeatureSourceLimits, FeatureSourceResolver, MAX_FEATURE_BLOB_BYTES,
    MAX_FEATURE_EXPANSION_RATIO, MAX_FEATURE_EXTRACTED_BYTES, MAX_FEATURE_FILES,
    MAX_FEATURE_METADATA_BYTES, MAX_FEATURE_PATH_BYTES, MAX_FEATURE_REDIRECTS, VerifiedFeature,
    extract_archive,
};
pub use forward_command::{
    ForwardPreflightError, SystemForwardError, preflight_forward, run_system_forward,
    system_forward_arguments,
};
pub use forwarding::{
    FORWARDING_SUPERVISOR_PROTOCOL, ForwardingSupervisorError, MAXIMUM_CONTROL_MESSAGE_BYTES,
    SUPERVISOR_CONTROL_TIMEOUT, SupervisorClaim, SupervisorCredentialLease, SupervisorForward,
    SupervisorManifest, SupervisorState, load_supervisor_state, run_private_supervisor_manifest,
    start_detached_supervisor, stop_supervisor, supervisor_control_token, supervisor_status,
};
pub use forwarding_reconciliation::{
    DeclaredForwardingRuntime, DesiredForwardingPlan, FIRST_UNPRIVILEGED_PORT, ForwardingPlanError,
    ForwardingReconciliationError, ForwardingReconciliationOutcome, ForwardingRuntimeOutcome,
    ForwardingRuntimeRequest, ScopedForwardingSupervisor, reconcile_declared_forwarding,
};
pub use generated_image::{
    GeneratedFeature, GeneratedImageError, GeneratedImagePlan, GeneratedUidGidUpdate, LinuxAccount,
    UidGidMutation, UidGidUpdateError, plan_uid_gid_update,
};
pub use git::{GitAdapter, GitError, GitVersion, OperationLogError};
pub use host_git_credentials::{
    HOST_GIT_LOOKUP_TIMEOUT, HostGitContextError, HostGitCredentialAdapter,
    HostGitCredentialContext, HostGitCredentialError, HostGitCredentialOutcome,
    HostGitCredentialUnavailable, HostGitLaunchEnvironment, MAX_HOST_GIT_HELPERS,
    MAX_HOST_GIT_STDERR_BYTES, ProcessHostGitLaunchEnvironment,
};
pub use image_orchestration::{
    ImageCleanupFailure, ImageContainerBuildRequest, ImageContainerCreateRequest,
    ImageContainerError, ImageContainerFacts, ImageContainerMatchState, ImageContainerOrchestrator,
    ImageContainerStopOutcome, PreparedImage, RecordedContainerRequest,
    classify_image_container_matches,
};
pub use installation::{
    FingerprintKey, FingerprintKeyState, FingerprintKeyUnknownReason, INSTALLATION_SCHEMA_VERSION,
    Installation, InstallationError, InstallationRecord, KeyedDigest, KeyedDigestError,
    PlanFingerprintCategory, SshIncludeConsent,
};
pub use lifecycle_orchestration::{
    BackgroundReadiness, BackgroundRunnerOutcome, ContainerLifecycle, HostLifecycle,
    HostLifecycleError, HostLifecycleExecutor, LifecycleCommandFailure, LifecycleInput,
    LifecycleMutation, LifecycleMutationFailure, LifecycleMutationOutcome, LifecycleOperation,
    LifecycleOrchestrationError, LifecycleOrchestrationRequest, LifecycleOrchestrator,
    LifecycleReadiness, LifecycleRetryClassification, LifecycleScenario,
};
pub use locking::{
    AttachSetupError, LockBehavior, LockError, LockGuard, LockMode, ensure_lock_file,
    with_shared_lock_for_attach,
};
pub use output::{
    ErrorDetail, ErrorEnvelope, JSON_SCHEMA_VERSION, OutputRenderError, OutputWarning,
    SuccessEnvelope, port_output_warnings, render_application_result, render_json_error,
    render_json_success,
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
pub use production::{
    ProductionWorkflowError, down_production, rebuild_production, reconcile_production,
};
pub use proxy::{
    ProxyEngine, ProxyError, ProxyRuntimeError, ProxyTarget, run_proxy_stdio, run_proxy_transport,
};
pub use rebuild::{
    CheckoutStatus, GENERATED_IMAGE_HISTORY, GeneratedImageCandidate, PreparedRebuildPlan,
    RebuildCleanupRequest, RebuildEnvironment, RebuildError, RebuildInvariantError, RebuildOutcome,
    RebuildPhase, RebuildPlanner, RebuildRequest, RebuildRollbackRequest, ReplacementFailure,
    rebuild_workspace, select_generated_images_for_cleanup,
};
pub use reconciliation::{
    CategoryDrift, EnvironmentReconciler, EnvironmentReconciliationRequest, EnvironmentTransition,
    FeatureSourcePolicy, PreparedDesiredPlan, ReadyEnvironment, ReconciliationError,
    ReconciliationOutcome, ReconciliationPlanner, ReconciliationRequest, ReconciliationWarning,
    RuntimeReconciliation, reconcile_workspace,
};
pub use reporting::{
    DockerSnapshot, FingerprintReport, ReportingError, ScenarioReport, StatusRequestError,
    WorkspaceListItem, WorkspaceListReport, WorkspaceStatusReport, collect_live_workspace_reports,
    correlate_workspace_reports, render_human_list, render_human_status,
    requested_workspace_status,
};
pub use ssh_command::{SystemSshError, run_system_ssh};
pub use ssh_config::{
    ExecutableResolutionError, SshConfigError, regenerate_managed_ssh, render_managed_config,
    resolve_current_executable, resolve_invoked_executable, system_ssh_arguments,
};
pub use ssh_consent::{
    ProcessSshConsentInteraction, SshConsentError, SshConsentInteraction, SshIncludeOutcome,
    apply_ssh_include_consent,
};
pub use ssh_identity::{
    SshIdentityError, WorkspaceHostIdentity, WorkspaceSshAssets, ensure_workspace_ssh_identity,
    load_workspace_ssh_assets,
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

/// Executes and renders `list` or `status`, returning `None` for every other command.
///
/// Reporting owns its command-specific success payloads here so JSON stdout is
/// exactly one document. Docker failures remain successful warnings for `list`
/// and become requested-state failures for `status`.
#[must_use]
pub fn render_reporting_application<Stdout, Stderr>(
    command_line: &CommandLine,
    stdout: &mut Stdout,
    stderr: &mut Stderr,
) -> Option<ExitCode>
where
    Stdout: Write + ?Sized,
    Stderr: Write + ?Sized,
{
    if !matches!(
        command_line.command(),
        CliCommand::List(_) | CliCommand::Status(_) | CliCommand::Doctor(_)
    ) {
        return None;
    }
    let format = command_line.output_format();
    let root = match CdenvRoot::resolve(command_line.root(), &ProcessEnvironment) {
        Ok(root) => root,
        Err(error) => {
            return Some(render_application_result(
                format,
                Err(ApplicationError::RootResolution(error)),
                stdout,
                stderr,
            ));
        }
    };
    if matches!(command_line.command(), CliCommand::Doctor(_)) {
        let report = doctor_report(&root);
        return Some(render_doctor_report(format, &report, stdout, stderr));
    }
    let (entries, docker) = match collect_live_workspace_reports(&root) {
        Ok(result) => result,
        Err(error) => {
            let application_error = match command_line.command() {
                CliCommand::List(_) => ApplicationError::ListFailed {
                    message: error.to_string(),
                },
                CliCommand::Status(_) => ApplicationError::StatusFailed {
                    message: error.to_string(),
                },
                _ => unreachable!("reporting commands were checked above"),
            };
            return Some(render_application_result(
                format,
                Err(application_error),
                stdout,
                stderr,
            ));
        }
    };

    match command_line.command() {
        CliCommand::List(_) => {
            let report = correlate_workspace_reports(&root, &entries, &docker);
            Some(render_list_result(format, &report, stdout, stderr))
        }
        CliCommand::Status(arguments) => {
            let report = requested_workspace_status(&root, &entries, &docker, &arguments.name);
            Some(render_status_result(format, report, stdout, stderr))
        }
        _ => None,
    }
}

fn render_list_result(
    format: OutputFormat,
    report: &WorkspaceListReport,
    stdout: &mut (impl Write + ?Sized),
    stderr: &mut (impl Write + ?Sized),
) -> ExitCode {
    let mut warnings = report_warnings(
        report
            .workspaces()
            .iter()
            .flat_map(|workspace| workspace.status().facts()),
    );
    if let Some(message) = report.docker_warning()
        && !warnings
            .iter()
            .any(|warning| warning.code() == "docker_unavailable")
    {
        warnings.push(OutputWarning::new(
            "docker_unavailable",
            format!("Docker unavailable: {message}"),
        ));
    }
    match format {
        OutputFormat::Json => {
            let envelope = SuccessEnvelope::with_warnings(report, warnings);
            render_json_success(stdout, &envelope).map_or_else(
                |error| output_failure(stderr, &error),
                |()| ExitCode::SUCCESS,
            )
        }
        OutputFormat::Human => match render_human_list(stdout, report) {
            Ok(()) => {
                for warning in warnings {
                    let _ = writeln!(stderr, "cdenv: warning: {}", warning.message());
                }
                ExitCode::SUCCESS
            }
            Err(error) => output_failure(stderr, &error),
        },
    }
}

#[derive(Serialize)]
struct StatusPayload<'a> {
    workspace: &'a WorkspaceStatusReport,
}

fn render_status_result(
    format: OutputFormat,
    report: Result<WorkspaceStatusReport, StatusRequestError>,
    stdout: &mut (impl Write + ?Sized),
    stderr: &mut (impl Write + ?Sized),
) -> ExitCode {
    let report = match report {
        Ok(report) => report,
        Err(error) => {
            return render_application_result(
                format,
                Err(ApplicationError::StatusFailed {
                    message: error.to_string(),
                }),
                stdout,
                stderr,
            );
        }
    };
    let warnings = report_warnings(report.status().facts());
    if report.is_requested_state_failure() {
        let error = ApplicationError::StatusFailed {
            message: format!(
                "workspace `{}` has an unavailable or unhealthy requested state",
                report.name()
            ),
        };
        return match format {
            OutputFormat::Json => {
                let envelope = ErrorEnvelope::with_warnings(
                    ErrorDetail::from_application_error(&error),
                    warnings,
                );
                render_json_error(stdout, &envelope).map_or_else(
                    |render_error| output_failure(stderr, &render_error),
                    |()| error.exit_code(),
                )
            }
            OutputFormat::Human => match render_human_status(stdout, &report) {
                Ok(()) => {
                    let _ = writeln!(stderr, "cdenv: {error}");
                    error.exit_code()
                }
                Err(render_error) => output_failure(stderr, &render_error),
            },
        };
    }
    match format {
        OutputFormat::Json => {
            let envelope =
                SuccessEnvelope::with_warnings(StatusPayload { workspace: &report }, warnings);
            render_json_success(stdout, &envelope).map_or_else(
                |error| output_failure(stderr, &error),
                |()| ExitCode::SUCCESS,
            )
        }
        OutputFormat::Human => render_human_status(stdout, &report).map_or_else(
            |error| output_failure(stderr, &error),
            |()| ExitCode::SUCCESS,
        ),
    }
}

fn report_warnings<'a>(
    facts: impl IntoIterator<Item = &'a cdenv_core::StatusFact>,
) -> Vec<OutputWarning> {
    let mut seen = BTreeSet::new();
    facts
        .into_iter()
        .filter(|fact| fact.severity() == cdenv_core::StatusFactSeverity::Warning)
        .filter_map(|fact| {
            let code = serde_json::to_value(fact.code()).ok()?.as_str()?.to_owned();
            seen.insert(code.clone())
                .then(|| OutputWarning::new(code.clone(), code.replace('_', " ")))
        })
        .collect()
}

fn output_failure(stderr: &mut (impl Write + ?Sized), error: &impl std::fmt::Display) -> ExitCode {
    let _ = writeln!(stderr, "cdenv: {error}");
    ExitCode::FAILURE
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
/// Runs the selected production workflow for `create`, `up`, `down`, and
/// `rebuild`. Parsed commands without composition return
/// [`ApplicationError::CommandUnavailable`].
pub fn invoke_with_root(
    command_line: &CommandLine,
    root: &CdenvRoot,
) -> Result<(), ApplicationError> {
    match command_line.command() {
        CliCommand::Create(arguments) => {
            let created = create_workspace(
                root,
                CreateWorkspaceRequest {
                    source: &arguments.git_source,
                    name: arguments.name.as_ref(),
                    config: arguments.config.as_ref(),
                },
                &GitAdapter::system(),
                &CancellationToken::default(),
            )
            .map_err(|error| ApplicationError::CreateFailed {
                message: error.to_string(),
            })?;
            reconcile_production(root, created.name(), arguments.config.as_ref()).map_err(|error| {
                ApplicationError::EnvironmentFailed {
                    message: error.to_string(),
                }
            })
        }
        CliCommand::Up(arguments) => {
            reconcile_production(root, &arguments.name, arguments.config.as_ref()).map_err(
                |error| ApplicationError::EnvironmentFailed {
                    message: error.to_string(),
                },
            )
        }
        CliCommand::Down(arguments) => down_production(root, &arguments.name).map_err(|error| {
            ApplicationError::EnvironmentFailed {
                message: error.to_string(),
            }
        }),
        CliCommand::Rebuild(arguments) => rebuild_production(
            root,
            &arguments.name,
            arguments.config.as_ref(),
            arguments.no_cache,
        )
        .map_err(|error| ApplicationError::EnvironmentFailed {
            message: error.to_string(),
        }),
        CliCommand::Credentials(arguments) => mutate_credentials(root, &arguments.command)
            .map(|_| ())
            .map_err(|error| ApplicationError::CredentialsFailed {
                message: error.to_string(),
            }),
        CliCommand::Lock(arguments) => {
            lock_workspace(root, arguments)
                .map(|_| ())
                .map_err(|error| ApplicationError::LockFailed {
                    message: error.to_string(),
                })
        }
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

    use super::{
        ApplicationError, CdenvRoot, CommandLine, RootEnvironment, invoke_with_environment,
        invoke_with_root,
    };

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
    fn production_up_dispatches_to_the_workflow_instead_of_command_unavailable() {
        let temporary = tempfile::tempdir().expect("temporary root");
        let path = temporary.path().join("cdenv");
        let environment = CountingEnvironment {
            cdenv_home_reads: Cell::new(0),
            home_reads: Cell::new(0),
        };
        let root = CdenvRoot::resolve(Some(&path), &environment).expect("root");
        let command_line =
            CommandLine::try_parse_from(["cdenv", "up", "project"]).expect("up command");

        assert!(matches!(
            invoke_with_root(&command_line, &root),
            Err(ApplicationError::EnvironmentFailed { .. })
        ));
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
