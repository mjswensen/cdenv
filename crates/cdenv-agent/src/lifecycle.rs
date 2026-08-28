//! Recoverable, generation-scoped lifecycle command execution.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use nix::fcntl::{Flock, FlockArg};
use nix::sys::signal::{Signal, kill};
use nix::unistd::{Pid, getegid, geteuid};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BUILD_ID, EnvironmentError, EnvironmentSnapshot, PROTOCOL_VERSION};

const MAXIMUM_LOG_BYTES: u64 = 1024 * 1024;
const POLL_INTERVAL: Duration = Duration::from_millis(25);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);

/// A lifecycle value resolved against the captured effective environment at execution time.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct LifecycleValue(pub Vec<LifecycleValueSegment>);

impl std::fmt::Debug for LifecycleValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LifecycleValue(<redacted>)")
    }
}

/// One segment of a lifecycle command or argument.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LifecycleValueSegment {
    /// Host-resolved literal text.
    Literal {
        /// Literal value.
        value: String,
    },
    /// Value from the captured effective environment.
    ContainerEnvironment {
        /// Environment variable name.
        name: String,
        /// Value used when the variable is absent.
        default: String,
    },
}

/// One shell or direct lifecycle process.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LifecycleProcess {
    /// Execute with `/bin/sh -c`.
    Shell {
        /// Exact shell input.
        command: LifecycleValue,
    },
    /// Execute directly without shell evaluation.
    Exec {
        /// Program followed by arguments.
        arguments: Vec<LifecycleValue>,
    },
}

impl std::fmt::Debug for LifecycleProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shell { .. } => formatter.write_str("Shell(<redacted>)"),
            Self::Exec { arguments } => formatter
                .debug_struct("Exec")
                .field("arguments", &arguments.len())
                .finish(),
        }
    }
}

/// One sequential command group; keyed entries execute concurrently.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum LifecycleCommand {
    /// One scalar command.
    Process {
        /// Process to execute.
        process: LifecycleProcess,
    },
    /// Stable-keyed concurrent commands, all of which must succeed.
    Parallel {
        /// Processes in deterministic key order.
        processes: BTreeMap<String, LifecycleProcess>,
    },
}

/// Container lifecycle stages executed by the agent.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LifecycleStage {
    /// New-generation creation hook.
    OnCreate,
    /// New-generation content hook.
    UpdateContent,
    /// New-generation post-create hook.
    PostCreate,
    /// Actual-start hook.
    PostStart,
    /// Per-transport attach hook.
    PostAttach,
}

impl LifecycleStage {
    const fn is_one_time(self) -> bool {
        !matches!(self, Self::PostStart | Self::PostAttach)
    }
}

/// One ordered stage in a runner request.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleStagePlan {
    /// Stage identity.
    pub stage: LifecycleStage,
    /// Sequential command groups.
    pub commands: Vec<LifecycleCommand>,
}

/// Immutable generation and runtime plan consumed by the container runner.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleRunRequest {
    /// Stable generation identity.
    pub generation: String,
    /// Expected agent build identity.
    pub build_id: String,
    /// Expected host-agent protocol.
    pub protocol_version: u32,
    /// Restricted selected-user state directory.
    pub state_directory: String,
    /// Captured effective environment snapshot.
    pub environment_path: String,
    /// Authoritative remote workspace folder.
    pub workspace_folder: String,
    /// Authoritative effective UID.
    pub uid: u32,
    /// Authoritative effective GID.
    pub gid: u32,
    /// Ordered stages which remain for this runner.
    pub stages: Vec<LifecycleStagePlan>,
}

/// Durable phase for the current lifecycle command group.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecyclePhase {
    /// No child for the command has been started.
    Before,
    /// At least one child may have executed.
    Running,
    /// The command group completed successfully.
    After,
    /// The command group definitely failed.
    Failed,
    /// A one-time command may have executed without a known result.
    Indeterminate,
    /// Cancellation completed and no owned child remains.
    Cancelled,
    /// Every requested stage completed.
    Complete,
}

/// Restricted durable runner checkpoint.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleRunnerState {
    /// Generation identity.
    pub generation: String,
    /// Agent build identity.
    pub build_id: String,
    /// Agent protocol identity.
    pub protocol_version: u32,
    /// Immutable lifecycle/runtime request.
    pub plan: LifecycleRunRequest,
    /// Current stage offset.
    pub stage_index: usize,
    /// Current command-group offset.
    pub command_index: usize,
    /// Durable execution phase.
    pub phase: LifecyclePhase,
    /// Runner process ID while a runner owns the state lock.
    pub runner_pid: Option<u32>,
    /// Safe failure category, never command output.
    pub failure: Option<String>,
}

/// Live verification result returned to the host.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LifecycleInspection {
    /// Verified durable checkpoint.
    pub state: LifecycleRunnerState,
    /// Whether a runner currently owns the generation lock.
    pub runner_active: bool,
}

/// Standard-input policy for synchronous scalar execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStdin {
    /// Inherit the caller's standard input.
    Inherit,
    /// Connect the child to closed input.
    Closed,
}

/// Lifecycle runner failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LifecycleError {
    /// A request identity, path, user, stage, or command was invalid.
    #[error("invalid lifecycle request field `{field}`")]
    InvalidRequest {
        /// Safe request field.
        field: &'static str,
    },
    /// The request targets another agent binary.
    #[error("lifecycle request {field} does not match this agent")]
    IdentityMismatch {
        /// `buildId` or `protocolVersion`.
        field: &'static str,
    },
    /// Existing generation state contains another immutable plan.
    #[error("existing lifecycle state does not match the requested immutable generation plan")]
    PlanMismatch,
    /// Another lifecycle runner already owns this generation.
    #[error("the lifecycle runner is already active")]
    AlreadyRunning,
    /// A prior one-time command has an unknowable result.
    #[error("one-time lifecycle execution is indeterminate and requires rebuild")]
    Indeterminate,
    /// One command group failed.
    #[error("lifecycle stage {stage:?} command group {command_index} failed")]
    CommandFailed {
        /// Failed stage.
        stage: LifecycleStage,
        /// Failed command-group offset.
        command_index: usize,
    },
    /// Restricted runner state could not be read or written.
    #[error("cannot access restricted lifecycle state: {source}")]
    StateIo {
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Durable state was malformed or unsafe.
    #[error("lifecycle state is malformed or not restricted")]
    UnsafeState,
    /// Effective environment loading failed.
    #[error("cannot load lifecycle effective environment: {source}")]
    Environment {
        /// Environment snapshot failure.
        #[source]
        source: EnvironmentError,
    },
    /// A process could not be started or controlled.
    #[error("cannot control lifecycle child process: {source}")]
    Child {
        /// Process failure.
        #[source]
        source: std::io::Error,
    },
}

/// Runs a request in the foreground while holding its generation lock.
///
/// Repeated calls never restart a completed command. A checkpoint left running by a previous
/// process is made indeterminate before this function returns.
///
/// # Errors
///
/// Returns identity, state, environment, child, cancellation, or command failures.
pub fn run_lifecycle(
    request: &LifecycleRunRequest,
) -> Result<LifecycleRunnerState, LifecycleError> {
    validate_request(request)?;
    let paths = RunnerPaths::new(request)?;
    let lock_file = open_lock(&paths.lock)?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusiveNonblock)
        .map_err(|(_, _)| LifecycleError::AlreadyRunning)?;
    let mut state = load_or_create(request, &paths)?;
    if state.plan != *request {
        return Err(LifecycleError::PlanMismatch);
    }
    if state.phase == LifecyclePhase::Running {
        state.phase = if current_stage(&state).is_some_and(LifecycleStage::is_one_time) {
            LifecyclePhase::Indeterminate
        } else {
            LifecyclePhase::Failed
        };
        state.runner_pid = None;
        state.failure = Some("interrupted".to_owned());
        store_state(&paths, &state)?;
    }
    if state.phase == LifecyclePhase::Indeterminate {
        return Err(LifecycleError::Indeterminate);
    }
    if matches!(
        state.phase,
        LifecyclePhase::Complete | LifecyclePhase::Failed | LifecyclePhase::Cancelled
    ) {
        return Ok(state);
    }

    state.runner_pid = Some(std::process::id());
    store_state(&paths, &state)?;
    let environment = EnvironmentSnapshot::load(Path::new(&request.environment_path))
        .map_err(|source| LifecycleError::Environment { source })?;
    while state.stage_index < request.stages.len() {
        let stage_plan = &request.stages[state.stage_index];
        while state.command_index < stage_plan.commands.len() {
            if paths.cancel.exists() {
                state.phase = LifecyclePhase::Cancelled;
                state.runner_pid = None;
                store_state(&paths, &state)?;
                return Ok(state);
            }
            state.phase = LifecyclePhase::Running;
            store_state(&paths, &state)?;
            let result = execute_command(
                &stage_plan.commands[state.command_index],
                request,
                &environment,
                LifecycleStdin::Closed,
                &paths,
                false,
            );
            match result {
                Ok(()) => {
                    state.phase = LifecyclePhase::After;
                    state.command_index += 1;
                    store_state(&paths, &state)?;
                }
                Err(ExecutionFailure::Cancelled) => {
                    state.phase = if stage_plan.stage.is_one_time() {
                        LifecyclePhase::Indeterminate
                    } else {
                        LifecyclePhase::Cancelled
                    };
                    state.runner_pid = None;
                    state.failure = Some("cancelled".to_owned());
                    store_state(&paths, &state)?;
                    return if state.phase == LifecyclePhase::Indeterminate {
                        Err(LifecycleError::Indeterminate)
                    } else {
                        Ok(state)
                    };
                }
                Err(ExecutionFailure::Error(error)) => {
                    state.phase = LifecyclePhase::Failed;
                    state.runner_pid = None;
                    state.failure = Some(error);
                    store_state(&paths, &state)?;
                    return Err(LifecycleError::CommandFailed {
                        stage: stage_plan.stage,
                        command_index: state.command_index,
                    });
                }
            }
        }
        state.stage_index += 1;
        state.command_index = 0;
        state.phase = LifecyclePhase::Before;
        store_state(&paths, &state)?;
    }
    state.phase = LifecyclePhase::Complete;
    state.runner_pid = None;
    state.failure = None;
    store_state(&paths, &state)?;
    let _ = fs::remove_file(&paths.cancel);
    Ok(state)
}

/// Executes one command group synchronously with an explicit stdin policy.
///
/// Parallel forms always receive closed stdin regardless of `stdin`.
///
/// # Errors
///
/// Returns validation, environment, child-control, cancellation, or exit failures.
pub fn execute_lifecycle_command(
    command: &LifecycleCommand,
    request: &LifecycleRunRequest,
    stdin: LifecycleStdin,
) -> Result<(), LifecycleError> {
    validate_request(request)?;
    let paths = RunnerPaths::new(request)?;
    let environment = EnvironmentSnapshot::load(Path::new(&request.environment_path))
        .map_err(|source| LifecycleError::Environment { source })?;
    execute_command(command, request, &environment, stdin, &paths, false).map_err(|failure| {
        match failure {
            ExecutionFailure::Cancelled => LifecycleError::Child {
                source: std::io::Error::new(std::io::ErrorKind::Interrupted, "lifecycle cancelled"),
            },
            ExecutionFailure::Error(message) => LifecycleError::Child {
                source: std::io::Error::other(message),
            },
        }
    })
}

/// Inspects and verifies existing runner state against an immutable request.
///
/// # Errors
///
/// Returns when identity, plan, path safety, permissions, or state encoding is invalid.
pub fn inspect_lifecycle(
    request: &LifecycleRunRequest,
) -> Result<LifecycleInspection, LifecycleError> {
    validate_request(request)?;
    let paths = RunnerPaths::new(request)?;
    let state = load_state(&paths.state)?;
    if state.plan != *request {
        return Err(LifecycleError::PlanMismatch);
    }
    let file = open_lock(&paths.lock)?;
    let runner_active = match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(_) => false,
        Err((_, _)) => true,
    };
    let mut visible = state;
    if visible.phase == LifecyclePhase::Running && !runner_active {
        visible.phase = if current_stage(&visible).is_some_and(LifecycleStage::is_one_time) {
            LifecyclePhase::Indeterminate
        } else {
            LifecyclePhase::Failed
        };
        visible.runner_pid = None;
    }
    Ok(LifecycleInspection {
        state: visible,
        runner_active,
    })
}

/// Requests graceful cancellation and waits a bounded period for runner cleanup.
///
/// # Errors
///
/// Returns state or process-control failures.
pub fn cancel_lifecycle(
    request: &LifecycleRunRequest,
    timeout: Duration,
) -> Result<LifecycleInspection, LifecycleError> {
    validate_request(request)?;
    let paths = RunnerPaths::new(request)?;
    atomic_write(&paths.cancel, b"cancel\n")?;
    let deadline = Instant::now() + timeout;
    loop {
        let inspection = inspect_lifecycle(request)?;
        if !inspection.runner_active {
            return Ok(inspection);
        }
        if Instant::now() >= deadline {
            if let Some(pid) = inspection
                .state
                .runner_pid
                .and_then(|value| i32::try_from(value).ok())
            {
                let _ = kill(Pid::from_raw(pid), Signal::SIGTERM);
                thread::sleep(TERMINATION_GRACE);
                let _ = kill(Pid::from_raw(pid), Signal::SIGKILL);
            }
            return inspect_lifecycle(request);
        }
        thread::sleep(POLL_INTERVAL);
    }
}

/// Runs the immutable `postAttachCommand` once for a newly established transport.
///
/// Invocations serialize on a generation-scoped container lock. Command output is written only
/// to the restricted lifecycle log, and every command receives closed standard input so SSH
/// protocol bytes remain untouched.
///
/// # Errors
///
/// Returns request, identity, environment, state, locking, child, or command failures.
pub fn run_post_attach(request: &LifecycleRunRequest) -> Result<(), LifecycleError> {
    validate_request(request)?;
    if request.stages.len() != 1 || request.stages[0].stage != LifecycleStage::PostAttach {
        return Err(LifecycleError::InvalidRequest {
            field: "postAttach stage",
        });
    }
    let paths = RunnerPaths::new_attach(request)?;
    let lock_file = open_lock(&paths.lock)?;
    let _lock = Flock::lock(lock_file, FlockArg::LockExclusive).map_err(|(_, source)| {
        LifecycleError::StateIo {
            source: source.into(),
        }
    })?;
    let environment = EnvironmentSnapshot::load(Path::new(&request.environment_path))
        .map_err(|source| LifecycleError::Environment { source })?;
    for (command_index, command) in request.stages[0].commands.iter().enumerate() {
        if execute_command(
            command,
            request,
            &environment,
            LifecycleStdin::Closed,
            &paths,
            true,
        )
        .is_err()
        {
            store_attach_state(&paths, request, false)?;
            return Err(LifecycleError::CommandFailed {
                stage: LifecycleStage::PostAttach,
                command_index,
            });
        }
    }
    store_attach_state(&paths, request, true)
}

fn validate_request(request: &LifecycleRunRequest) -> Result<(), LifecycleError> {
    if request.build_id != BUILD_ID {
        return Err(LifecycleError::IdentityMismatch { field: "buildId" });
    }
    if request.protocol_version != PROTOCOL_VERSION {
        return Err(LifecycleError::IdentityMismatch {
            field: "protocolVersion",
        });
    }
    if !valid_generation(&request.generation) {
        return Err(LifecycleError::InvalidRequest {
            field: "generation",
        });
    }
    for (field, value) in [
        ("stateDirectory", request.state_directory.as_str()),
        ("environmentPath", request.environment_path.as_str()),
        ("workspaceFolder", request.workspace_folder.as_str()),
    ] {
        if safe_absolute(Path::new(value)).is_none() {
            return Err(LifecycleError::InvalidRequest { field });
        }
    }
    if request.uid != geteuid().as_raw() || request.gid != getegid().as_raw() {
        return Err(LifecycleError::InvalidRequest { field: "user" });
    }
    let mut previous = None;
    for plan in &request.stages {
        let rank = stage_rank(plan.stage);
        if previous.is_some_and(|value| value >= rank) {
            return Err(LifecycleError::InvalidRequest { field: "stages" });
        }
        previous = Some(rank);
    }
    Ok(())
}

const fn stage_rank(stage: LifecycleStage) -> u8 {
    match stage {
        LifecycleStage::OnCreate => 0,
        LifecycleStage::UpdateContent => 1,
        LifecycleStage::PostCreate => 2,
        LifecycleStage::PostStart => 3,
        LifecycleStage::PostAttach => 4,
    }
}

fn valid_generation(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn safe_absolute(path: &Path) -> Option<&Path> {
    (path.is_absolute()
        && path != Path::new("/")
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        }))
    .then_some(path)
}

struct RunnerPaths {
    directory: PathBuf,
    state: PathBuf,
    lock: PathBuf,
    cancel: PathBuf,
    log: PathBuf,
}

impl RunnerPaths {
    fn new(request: &LifecycleRunRequest) -> Result<Self, LifecycleError> {
        Self::with_stem(request, &format!(".cdenv-lifecycle-{}", request.generation))
    }

    fn new_attach(request: &LifecycleRunRequest) -> Result<Self, LifecycleError> {
        Self::with_stem(request, &format!(".cdenv-attach-{}", request.generation))
    }

    fn with_stem(request: &LifecycleRunRequest, stem: &str) -> Result<Self, LifecycleError> {
        let directory = PathBuf::from(&request.state_directory);
        prepare_directory(&directory)?;
        Ok(Self {
            state: directory.join(format!("{stem}.json")),
            lock: directory.join(format!("{stem}.lock")),
            cancel: directory.join(format!("{stem}.cancel")),
            log: directory.join(format!("{stem}.log")),
            directory,
        })
    }
}

fn prepare_directory(path: &Path) -> Result<(), LifecycleError> {
    reject_symlink_components(path)?;
    fs::create_dir_all(path).map_err(|source| LifecycleError::StateIo { source })?;
    reject_symlink_components(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|source| LifecycleError::StateIo { source })?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != geteuid().as_raw()
    {
        return Err(LifecycleError::UnsafeState);
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|source| LifecycleError::StateIo { source })
}

fn reject_symlink_components(path: &Path) -> Result<(), LifecycleError> {
    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LifecycleError::UnsafeState);
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(source) => return Err(LifecycleError::StateIo { source }),
        }
    }
    Ok(())
}

fn open_lock(path: &Path) -> Result<File, LifecycleError> {
    if path.exists() {
        validate_restricted_file(path)?;
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)
        .map_err(|source| LifecycleError::StateIo { source })?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| LifecycleError::StateIo { source })?;
    Ok(file)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AttachState<'a> {
    generation: &'a str,
    successful: bool,
}

fn store_attach_state(
    paths: &RunnerPaths,
    request: &LifecycleRunRequest,
    successful: bool,
) -> Result<(), LifecycleError> {
    let bytes = serde_json::to_vec(&AttachState {
        generation: &request.generation,
        successful,
    })
    .map_err(|_| LifecycleError::UnsafeState)?;
    atomic_write_in(&paths.directory, &paths.state, &bytes)
}

fn load_or_create(
    request: &LifecycleRunRequest,
    paths: &RunnerPaths,
) -> Result<LifecycleRunnerState, LifecycleError> {
    if paths.state.exists() {
        load_state(&paths.state)
    } else {
        let state = LifecycleRunnerState {
            generation: request.generation.clone(),
            build_id: request.build_id.clone(),
            protocol_version: request.protocol_version,
            plan: request.clone(),
            stage_index: 0,
            command_index: 0,
            phase: LifecyclePhase::Before,
            runner_pid: None,
            failure: None,
        };
        store_state(paths, &state)?;
        Ok(state)
    }
}

fn load_state(path: &Path) -> Result<LifecycleRunnerState, LifecycleError> {
    validate_restricted_file(path)?;
    let bytes = fs::read(path).map_err(|source| LifecycleError::StateIo { source })?;
    serde_json::from_slice(&bytes).map_err(|_| LifecycleError::UnsafeState)
}

fn store_state(paths: &RunnerPaths, state: &LifecycleRunnerState) -> Result<(), LifecycleError> {
    let bytes = serde_json::to_vec(state).map_err(|_| LifecycleError::UnsafeState)?;
    atomic_write_in(&paths.directory, &paths.state, &bytes)
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), LifecycleError> {
    let parent = path.parent().ok_or(LifecycleError::UnsafeState)?;
    atomic_write_in(parent, path, bytes)
}

fn validate_restricted_file(path: &Path) -> Result<(), LifecycleError> {
    let metadata =
        fs::symlink_metadata(path).map_err(|source| LifecycleError::StateIo { source })?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.uid() != geteuid().as_raw()
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(LifecycleError::UnsafeState);
    }
    Ok(())
}

fn atomic_write_in(parent: &Path, path: &Path, bytes: &[u8]) -> Result<(), LifecycleError> {
    let target = path.file_name().and_then(OsStr::to_str).unwrap_or("state");
    let temporary = parent.join(format!(
        ".cdenv-lifecycle-write-{}-{target}",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(LifecycleError::StateIo { source });
    }
    Ok(())
}

fn current_stage(state: &LifecycleRunnerState) -> Option<LifecycleStage> {
    state
        .plan
        .stages
        .get(state.stage_index)
        .map(|plan| plan.stage)
}

#[derive(Debug)]
enum ExecutionFailure {
    Cancelled,
    Error(String),
}

struct OwnedChild {
    child: Child,
    status: Option<ExitStatus>,
}

fn execute_command(
    command: &LifecycleCommand,
    request: &LifecycleRunRequest,
    environment: &EnvironmentSnapshot,
    stdin: LifecycleStdin,
    paths: &RunnerPaths,
    stdout_to_stderr: bool,
) -> Result<(), ExecutionFailure> {
    let log = Arc::new(Mutex::new(
        open_log(&paths.log).map_err(|error| ExecutionFailure::Error(error.to_string()))?,
    ));
    let mut children = Vec::new();
    let mut readers = Vec::new();
    match command {
        LifecycleCommand::Process { process } => {
            let (child, mut spawned_readers) = spawn_process(
                process,
                None,
                request,
                environment,
                stdin,
                Arc::clone(&log),
                stdout_to_stderr,
            )?;
            children.push(OwnedChild {
                child,
                status: None,
            });
            readers.append(&mut spawned_readers);
        }
        LifecycleCommand::Parallel { processes } => {
            for (key, process) in processes {
                match spawn_process(
                    process,
                    Some(key.clone()),
                    request,
                    environment,
                    LifecycleStdin::Closed,
                    Arc::clone(&log),
                    stdout_to_stderr,
                ) {
                    Ok((child, mut spawned_readers)) => {
                        children.push(OwnedChild {
                            child,
                            status: None,
                        });
                        readers.append(&mut spawned_readers);
                    }
                    Err(error) => {
                        terminate_children(&mut children);
                        join_readers(readers);
                        return Err(error);
                    }
                }
            }
        }
    }
    let result = wait_children(&mut children, &paths.cancel);
    join_readers(readers);
    result
}

fn spawn_process(
    process: &LifecycleProcess,
    key: Option<String>,
    request: &LifecycleRunRequest,
    environment: &EnvironmentSnapshot,
    stdin: LifecycleStdin,
    log: Arc<Mutex<BoundedLog>>,
    stdout_to_stderr: bool,
) -> Result<(Child, Vec<thread::JoinHandle<()>>), ExecutionFailure> {
    let mut command = match process {
        LifecycleProcess::Shell { command } => {
            let mut process = Command::new("/bin/sh");
            process.args([OsString::from("-c"), resolve(command, environment)?]);
            process
        }
        LifecycleProcess::Exec { arguments } => {
            let (program, rest) = arguments
                .split_first()
                .ok_or_else(|| ExecutionFailure::Error("empty argv".to_owned()))?;
            let mut process = Command::new(resolve(program, environment)?);
            for argument in rest {
                process.arg(resolve(argument, environment)?);
            }
            process
        }
    };
    environment.apply_to(&mut command);
    command.current_dir(&request.workspace_folder);
    command.process_group(0);
    command.stdin(match stdin {
        LifecycleStdin::Inherit => Stdio::inherit(),
        LifecycleStdin::Closed => Stdio::null(),
    });
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| ExecutionFailure::Error(error.to_string()))?;
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(stream_reader(
            stdout,
            key.clone(),
            stdout_to_stderr,
            Arc::clone(&log),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(stream_reader(stderr, key, true, log));
    }
    Ok((child, readers))
}

fn resolve(
    value: &LifecycleValue,
    environment: &EnvironmentSnapshot,
) -> Result<OsString, ExecutionFailure> {
    let mut output = OsString::new();
    for segment in &value.0 {
        match segment {
            LifecycleValueSegment::Literal { value } => output.push(value),
            LifecycleValueSegment::ContainerEnvironment { name, default } => {
                if name.is_empty()
                    || name.as_bytes().contains(&b'=')
                    || name.as_bytes().contains(&0)
                {
                    return Err(ExecutionFailure::Error(
                        "invalid environment name".to_owned(),
                    ));
                }
                output.push(
                    environment
                        .value(OsStr::new(name))
                        .unwrap_or_else(|| OsStr::new(default)),
                );
            }
        }
    }
    Ok(output)
}

fn wait_children(children: &mut [OwnedChild], cancel: &Path) -> Result<(), ExecutionFailure> {
    loop {
        if cancel.exists() {
            terminate_children(children);
            return Err(ExecutionFailure::Cancelled);
        }
        let mut complete = true;
        let mut failed = false;
        for owned in &mut *children {
            if owned.status.is_none() {
                owned.status = owned
                    .child
                    .try_wait()
                    .map_err(|error| ExecutionFailure::Error(error.to_string()))?;
            }
            complete &= owned.status.is_some();
            failed |= owned.status.is_some_and(|status| !status.success());
        }
        if failed {
            terminate_children(children);
            return Err(ExecutionFailure::Error("nonzero exit status".to_owned()));
        }
        if complete {
            return Ok(());
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn terminate_children(children: &mut [OwnedChild]) {
    for owned in &*children {
        if owned.status.is_none() {
            signal_group(&owned.child, Signal::SIGTERM);
        }
    }
    let deadline = Instant::now() + TERMINATION_GRACE;
    while Instant::now() < deadline {
        let mut complete = true;
        for owned in &mut *children {
            if owned.status.is_none() {
                owned.status = owned.child.try_wait().ok().flatten();
            }
            complete &= owned.status.is_some();
        }
        if complete {
            return;
        }
        thread::sleep(POLL_INTERVAL);
    }
    for owned in &*children {
        if owned.status.is_none() {
            signal_group(&owned.child, Signal::SIGKILL);
        }
    }
    for owned in &mut *children {
        if owned.status.is_none() {
            let _ = owned.child.wait();
        }
    }
}

fn signal_group(child: &Child, signal: Signal) {
    if let Ok(pid) = i32::try_from(child.id()) {
        let _ = kill(Pid::from_raw(-pid), signal);
    }
}

struct BoundedLog {
    file: File,
    written: u64,
}

fn open_log(path: &Path) -> Result<BoundedLog, LifecycleError> {
    if path.exists() {
        validate_restricted_file(path)?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|source| LifecycleError::StateIo { source })?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| LifecycleError::StateIo { source })?;
    let written = file
        .metadata()
        .map_err(|source| LifecycleError::StateIo { source })?
        .len()
        .min(MAXIMUM_LOG_BYTES);
    Ok(BoundedLog { file, written })
}

impl BoundedLog {
    fn append(&mut self, prefix: &[u8], bytes: &[u8]) {
        if self.written >= MAXIMUM_LOG_BYTES {
            return;
        }
        let remaining = usize::try_from(MAXIMUM_LOG_BYTES - self.written).unwrap_or(usize::MAX);
        let prefix_length = prefix.len().min(remaining);
        let _ = self.file.write_all(&prefix[..prefix_length]);
        self.written += prefix_length as u64;
        let remaining = usize::try_from(MAXIMUM_LOG_BYTES - self.written).unwrap_or(usize::MAX);
        let length = bytes.len().min(remaining);
        let _ = self.file.write_all(&bytes[..length]);
        self.written += length as u64;
        let _ = self.file.flush();
    }
}

fn stream_reader(
    mut stream: impl Read + Send + 'static,
    key: Option<String>,
    stderr: bool,
    log: Arc<Mutex<BoundedLog>>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let prefix = key.map_or_else(Vec::new, |key| format!("[{key}] ").into_bytes());
        let mut buffer = [0_u8; 8192];
        while let Ok(length) = stream.read(&mut buffer) {
            if length == 0 {
                break;
            }
            if let Ok(mut output) = log.lock() {
                output.append(&prefix, &buffer[..length]);
                if stderr {
                    let mut target = std::io::stderr().lock();
                    let _ = target.write_all(&prefix);
                    let _ = target.write_all(&buffer[..length]);
                } else {
                    let mut target = std::io::stdout().lock();
                    let _ = target.write_all(&prefix);
                    let _ = target.write_all(&buffer[..length]);
                }
            }
        }
    })
}

fn join_readers(readers: Vec<thread::JoinHandle<()>>) {
    for reader in readers {
        let _ = reader.join();
    }
}
