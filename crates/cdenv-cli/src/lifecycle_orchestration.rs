//! Host coordination of lifecycle stages through the configured readiness boundary.

use std::error::Error;
use std::ffi::OsString;
use std::future::Future;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use cdenv_devcontainer::{
    DeferredString, LifecycleCommand, LifecyclePlan, LifecycleProcess, LifecycleStage,
    LifecycleStagePlan,
};
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::time::sleep;

use crate::CancellationToken;

const PROCESS_POLL_INTERVAL: Duration = Duration::from_millis(20);
const PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);

/// Top-level operation requesting lifecycle coordination.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleOperation {
    /// Initial environment creation.
    Create,
    /// Ordinary environment start or verification.
    Up,
    /// Explicit generation replacement.
    Rebuild,
}

/// Container scenario selected by the effective plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleScenario {
    /// Direct image scenario.
    Image,
    /// Generated Dockerfile scenario.
    Dockerfile,
    /// Isolated Docker Compose project.
    Compose,
}

/// Whether a synchronous scalar lifecycle command may read caller input.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleInput {
    /// Inherit the caller's standard input.
    Inherit,
    /// Connect the command to closed input.
    Closed,
}

/// Verified effects of the container mutation seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LifecycleMutationOutcome {
    /// Whether this operation created a new immutable generation.
    pub new_generation: bool,
    /// Whether this operation actually transitioned the environment from stopped to running.
    pub actual_start: bool,
}

/// Retry safety reported when readiness is unsuccessful.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleRetryClassification {
    /// No one-time command may have executed; an ordinary retry is safe.
    SafeRetry,
    /// A command definitely failed and later stages were skipped.
    DefiniteFailure,
    /// Work after readiness failed or an existing runner reported failure.
    BackgroundFailure,
    /// A one-time command may have executed without a knowable outcome.
    IndeterminateOneTime,
}

/// Background runner state verified by the runtime seam.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundRunnerOutcome {
    /// A new runner was started for later work.
    Started,
    /// The matching generation runner was already active and no duplicate was launched.
    VerifiedRunning,
    /// Matching later work had already completed.
    Complete,
    /// Matching later work definitely failed.
    Failed,
    /// Matching one-time work has an unknowable outcome.
    Indeterminate,
}

/// Background activity returned with a successful readiness result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackgroundReadiness {
    /// No applicable stage remains after `waitFor`.
    NotRequired,
    /// Later work was started or verified.
    Runner(BackgroundRunnerOutcome),
}

/// Explicit readiness facts returned for a later atomic state commit.
///
/// Constructing this value performs no persistence.
#[derive(Debug)]
pub struct LifecycleReadiness<P, E> {
    /// Verified mutation facts.
    pub mutation: LifecycleMutationOutcome,
    /// Freshly provisioned agent facts.
    pub provisioned: P,
    /// Environment recaptured after the selected readiness boundary.
    pub environment: E,
    /// Configured stage which established readiness.
    pub completed_through: LifecycleStage,
    /// State of applicable later stages.
    pub background: BackgroundReadiness,
}

/// Failure from one lifecycle command boundary.
#[derive(Debug, Error)]
pub enum LifecycleCommandFailure<E: Error + Send + Sync + 'static> {
    /// No child was started, so retrying cannot duplicate a command.
    #[error("lifecycle command did not start: {0}")]
    BeforeStart(E),
    /// The command has a definite non-success result and owned children were cleaned up.
    #[error("lifecycle command failed: {0}")]
    Definite(E),
    /// Cancellation or transport loss made a one-time result unknowable.
    #[error("lifecycle command outcome is indeterminate: {0}")]
    Indeterminate(E),
}

/// Failure from the environment mutation seam.
#[derive(Debug, Error)]
pub enum LifecycleMutationFailure<E: Error + Send + Sync + 'static> {
    /// Mutation did not begin or was safely rolled back.
    #[error("environment mutation is safely retryable: {0}")]
    SafeRetry(E),
    /// Mutation failed after leaving definite non-ready live state.
    #[error("environment mutation failed: {0}")]
    Definite(E),
}

/// Layered host lifecycle orchestration failure.
#[derive(Debug, Error)]
pub enum LifecycleOrchestrationError<
    H: Error + Send + Sync + 'static,
    M: Error + Send + Sync + 'static,
    R: Error + Send + Sync + 'static,
> {
    /// Repeatable host initialization failed.
    #[error("initializeCommand failed: {source}")]
    Initialize {
        /// Preserved host executor error.
        #[source]
        source: H,
    },
    /// Cancellation was observed at a definite coordinator boundary.
    #[error("lifecycle readiness was cancelled before {step}")]
    Cancelled {
        /// Next coordinator step.
        step: &'static str,
    },
    /// Container mutation failed.
    #[error("container mutation failed: {source}")]
    Mutation {
        /// Preserved mutation error.
        #[source]
        source: LifecycleMutationFailure<M>,
    },
    /// A foreground container lifecycle stage failed.
    #[error("container lifecycle stage {stage:?} failed: {source}")]
    Stage {
        /// Failed stage.
        stage: LifecycleStage,
        /// Preserved runtime command failure.
        #[source]
        source: LifecycleCommandFailure<R>,
    },
    /// Always-upload provisioning failed before foreground readiness work.
    #[error("readiness agent provisioning failed: {source}")]
    Provision {
        /// Preserved runtime error.
        #[source]
        source: R,
    },
    /// Effective environment capture or recapture failed.
    #[error("readiness environment {operation} failed: {source}")]
    Environment {
        /// `capture` or `recapture`.
        operation: &'static str,
        /// Preserved runtime error.
        #[source]
        source: R,
    },
    /// Forwarding readiness failed without stopping the environment.
    #[error("forwarding readiness failed while the environment remains running: {source}")]
    Forwarding {
        /// Preserved forwarding error.
        #[source]
        source: R,
    },
    /// Starting or verifying later work failed.
    #[error("background lifecycle runner failed: {source}")]
    Runner {
        /// Preserved runner-control error.
        #[source]
        source: R,
    },
    /// Existing background work has a terminal unhealthy checkpoint.
    #[error("background lifecycle work is {outcome:?}")]
    Background {
        /// Failed or indeterminate runner result.
        outcome: BackgroundRunnerOutcome,
    },
}

impl<
    H: Error + Send + Sync + 'static,
    M: Error + Send + Sync + 'static,
    R: Error + Send + Sync + 'static,
> LifecycleOrchestrationError<H, M, R>
{
    /// Classifies whether a failed operation can safely be retried without a rebuild.
    #[must_use]
    pub const fn retry_classification(&self) -> LifecycleRetryClassification {
        match self {
            Self::Initialize { .. }
            | Self::Cancelled { .. }
            | Self::Provision { .. }
            | Self::Environment { .. }
            | Self::Forwarding { .. }
            | Self::Mutation {
                source: LifecycleMutationFailure::SafeRetry(_),
            }
            | Self::Stage {
                source: LifecycleCommandFailure::BeforeStart(_),
                ..
            } => LifecycleRetryClassification::SafeRetry,
            Self::Stage {
                source: LifecycleCommandFailure::Indeterminate(_),
                stage,
            } if stage.is_one_time() => LifecycleRetryClassification::IndeterminateOneTime,
            Self::Background {
                outcome: BackgroundRunnerOutcome::Indeterminate,
            } => LifecycleRetryClassification::IndeterminateOneTime,
            Self::Background { .. } | Self::Runner { .. } => {
                LifecycleRetryClassification::BackgroundFailure
            }
            Self::Mutation { .. } | Self::Stage { .. } => {
                LifecycleRetryClassification::DefiniteFailure
            }
        }
    }
}

/// Static seam for image, Dockerfile, and Compose creation/start reconciliation.
#[doc(hidden)]
pub trait LifecycleMutation: Send + Sync {
    /// Adapter-specific mutation error.
    type Error: Error + Send + Sync + 'static;

    /// Mutates or verifies the selected scenario without persisting active state.
    fn mutate(
        &self,
        operation: LifecycleOperation,
        scenario: LifecycleScenario,
        cancellation: &CancellationToken,
    ) -> impl Future<
        Output = Result<LifecycleMutationOutcome, LifecycleMutationFailure<Self::Error>>,
    > + Send;
}

/// Static host-checkout lifecycle seam.
#[doc(hidden)]
pub trait HostLifecycle: Send + Sync {
    /// Host command error.
    type Error: Error + Send + Sync + 'static;

    /// Executes all `initializeCommand` groups in source order.
    fn initialize(
        &self,
        plan: &LifecycleStagePlan,
        checkout: &Path,
        input: LifecycleInput,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Static container lifecycle, provisioning, environment, forwarding, and runner seam.
#[doc(hidden)]
pub trait ContainerLifecycle: Send + Sync {
    /// Runtime adapter error.
    type Error: Error + Send + Sync + 'static;
    /// Fresh provisioning facts retained for the eventual transaction commit.
    type Provisioned;
    /// Initial captured environment token.
    type InitialEnvironment;
    /// Post-readiness recaptured environment facts.
    type Environment;

    /// Executes one complete foreground stage.
    fn execute_stage(
        &self,
        plan: &LifecycleStagePlan,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<(), LifecycleCommandFailure<Self::Error>>> + Send;

    /// Always uploads, provisions, and verifies the expected agent.
    fn provision(
        &self,
        cancellation: &CancellationToken,
    ) -> impl Future<Output = Result<Self::Provisioned, Self::Error>> + Send;

    /// Captures the environment used by foreground readiness lifecycle work.
    fn capture_environment<'a>(
        &'a self,
        provisioned: &'a Self::Provisioned,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<Self::InitialEnvironment, Self::Error>> + Send + 'a;

    /// Recaptures the environment which new transports will use.
    fn recapture_environment<'a>(
        &'a self,
        provisioned: &'a Self::Provisioned,
        initial: &'a Self::InitialEnvironment,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<Self::Environment, Self::Error>> + Send + 'a;

    /// Establishes all declared forwarding readiness transactionally.
    fn forwarding_ready<'a>(
        &'a self,
        environment: &'a Self::Environment,
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Starts or verifies exactly one generation runner for all applicable later stages.
    fn start_or_verify_runner<'a>(
        &'a self,
        later: &'a [LifecycleStagePlan],
        cancellation: &'a CancellationToken,
    ) -> impl Future<Output = Result<BackgroundRunnerOutcome, Self::Error>> + Send + 'a;
}

/// Borrowed host lifecycle coordination inputs.
pub struct LifecycleOrchestrationRequest<'a> {
    /// Immutable active-generation lifecycle plan.
    pub plan: &'a LifecyclePlan,
    /// Canonical host checkout used by repeatable initialization.
    pub checkout: &'a Path,
    /// Current command operation.
    pub operation: LifecycleOperation,
    /// Effective container scenario.
    pub scenario: LifecycleScenario,
    /// Caller-approved stdin policy for synchronous scalar forms.
    pub input: LifecycleInput,
}

/// Coordinates lifecycle work without writing active-generation state.
pub struct LifecycleOrchestrator<H, M, R> {
    host: H,
    mutation: M,
    runtime: R,
}

impl<H, M, R> LifecycleOrchestrator<H, M, R> {
    /// Constructs an orchestrator from narrow, statically dispatched seams.
    #[must_use]
    pub const fn new(host: H, mutation: M, runtime: R) -> Self {
        Self {
            host,
            mutation,
            runtime,
        }
    }
}

impl<H: HostLifecycle, M: LifecycleMutation, R: ContainerLifecycle> LifecycleOrchestrator<H, M, R> {
    /// Executes through readiness and returns facts for a later atomic transaction commit.
    ///
    /// # Errors
    ///
    /// Returns layered host, mutation, stage, provisioning, environment, forwarding, runner, or
    /// cancellation errors. No error path commits active state.
    pub async fn orchestrate(
        &self,
        request: &LifecycleOrchestrationRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<
        LifecycleReadiness<R::Provisioned, R::Environment>,
        LifecycleOrchestrationError<H::Error, M::Error, R::Error>,
    > {
        self.host
            .initialize(
                &request.plan.initialize,
                request.checkout,
                request.input,
                cancellation,
            )
            .await
            .map_err(|source| LifecycleOrchestrationError::Initialize { source })?;
        check_cancelled(cancellation, "container mutation")?;
        let mutation = self
            .mutation
            .mutate(request.operation, request.scenario, cancellation)
            .await
            .map_err(|source| LifecycleOrchestrationError::Mutation { source })?;

        check_cancelled(cancellation, "agent provisioning")?;
        let provisioned = self
            .runtime
            .provision(cancellation)
            .await
            .map_err(|source| LifecycleOrchestrationError::Provision { source })?;
        let initial = self
            .runtime
            .capture_environment(&provisioned, cancellation)
            .await
            .map_err(|source| LifecycleOrchestrationError::Environment {
                operation: "capture",
                source,
            })?;

        let applicable = applicable_stages(request.plan, mutation);
        for stage in applicable
            .iter()
            .filter(|stage| stage.stage <= request.plan.readiness)
        {
            check_cancelled(cancellation, "foreground lifecycle stage")?;
            self.runtime
                .execute_stage(stage, cancellation)
                .await
                .map_err(|source| LifecycleOrchestrationError::Stage {
                    stage: stage.stage,
                    source,
                })?;
        }

        let environment = self
            .runtime
            .recapture_environment(&provisioned, &initial, cancellation)
            .await
            .map_err(|source| LifecycleOrchestrationError::Environment {
                operation: "recapture",
                source,
            })?;
        self.runtime
            .forwarding_ready(&environment, cancellation)
            .await
            .map_err(|source| LifecycleOrchestrationError::Forwarding { source })?;

        let later = applicable
            .into_iter()
            .filter(|stage| stage.stage > request.plan.readiness)
            .cloned()
            .collect::<Vec<_>>();
        let background = if later.is_empty() {
            BackgroundReadiness::NotRequired
        } else {
            let outcome = self
                .runtime
                .start_or_verify_runner(&later, cancellation)
                .await
                .map_err(|source| LifecycleOrchestrationError::Runner { source })?;
            if matches!(
                outcome,
                BackgroundRunnerOutcome::Failed | BackgroundRunnerOutcome::Indeterminate
            ) {
                return Err(LifecycleOrchestrationError::Background { outcome });
            }
            BackgroundReadiness::Runner(outcome)
        };
        Ok(LifecycleReadiness {
            mutation,
            provisioned,
            environment,
            completed_through: request.plan.readiness,
            background,
        })
    }
}

fn check_cancelled<H, M, R>(
    cancellation: &CancellationToken,
    step: &'static str,
) -> Result<(), LifecycleOrchestrationError<H, M, R>>
where
    H: Error + Send + Sync + 'static,
    M: Error + Send + Sync + 'static,
    R: Error + Send + Sync + 'static,
{
    if cancellation.is_cancelled() {
        Err(LifecycleOrchestrationError::Cancelled { step })
    } else {
        Ok(())
    }
}

fn applicable_stages(
    plan: &LifecyclePlan,
    mutation: LifecycleMutationOutcome,
) -> Vec<&LifecycleStagePlan> {
    let mut stages = Vec::new();
    if mutation.new_generation {
        stages.extend([&plan.on_create, &plan.update_content, &plan.post_create]);
    }
    if mutation.actual_start {
        stages.push(&plan.post_start);
    }
    stages
}

/// Production host-checkout executor for `initializeCommand`.
#[derive(Clone, Copy, Debug, Default)]
pub struct HostLifecycleExecutor;

/// Host lifecycle process failure.
#[derive(Debug, Error)]
pub enum HostLifecycleError {
    /// Host initialization was cancelled and every owned process was terminated.
    #[error("host lifecycle command was cancelled")]
    Cancelled,
    /// A shell or direct process could not be spawned or waited.
    #[error("cannot execute host lifecycle command: {source}")]
    Process {
        /// Operating-system failure.
        #[source]
        source: std::io::Error,
    },
    /// One scalar or parallel command exited unsuccessfully.
    #[error("host lifecycle command exited unsuccessfully")]
    Failed,
    /// A direct command supplied an empty argv.
    #[error("host lifecycle direct command has an empty argv")]
    EmptyArguments,
}

impl HostLifecycle for HostLifecycleExecutor {
    type Error = HostLifecycleError;

    async fn initialize(
        &self,
        plan: &LifecycleStagePlan,
        checkout: &Path,
        input: LifecycleInput,
        cancellation: &CancellationToken,
    ) -> Result<(), Self::Error> {
        if plan.stage != LifecycleStage::Initialize {
            return Err(HostLifecycleError::Failed);
        }
        for command in &plan.commands {
            run_host_group(command, checkout, input, cancellation).await?;
        }
        Ok(())
    }
}

fn resolve_host(value: &DeferredString) -> OsString {
    value
        .resolve(&std::collections::BTreeMap::new())
        .expose()
        .into()
}

async fn run_host_group(
    group: &LifecycleCommand,
    checkout: &Path,
    input: LifecycleInput,
    cancellation: &CancellationToken,
) -> Result<(), HostLifecycleError> {
    let processes = match group {
        LifecycleCommand::Process(process) => vec![process],
        LifecycleCommand::Parallel(processes) => processes.values().collect(),
    };
    let closed = matches!(group, LifecycleCommand::Parallel(_)) || input == LifecycleInput::Closed;
    let mut children = Vec::with_capacity(processes.len());
    for process in processes {
        match spawn_host(process, checkout, closed) {
            Ok(child) => children.push(child),
            Err(error) => {
                terminate_host_children(&mut children).await;
                return Err(error);
            }
        }
    }
    wait_host_children(&mut children, cancellation).await
}

fn spawn_host(
    process: &LifecycleProcess,
    checkout: &Path,
    closed: bool,
) -> Result<Child, HostLifecycleError> {
    let mut command = match process {
        LifecycleProcess::Shell(value) => {
            let mut command = Command::new("/bin/sh");
            command.args([OsString::from("-c"), resolve_host(value)]);
            command
        }
        LifecycleProcess::Exec(arguments) => {
            let (program, arguments) = arguments
                .split_first()
                .ok_or(HostLifecycleError::EmptyArguments)?;
            let mut command = Command::new(resolve_host(program));
            command.args(arguments.iter().map(resolve_host));
            command
        }
    };
    command
        .current_dir(checkout)
        .stdin(if closed {
            Stdio::null()
        } else {
            Stdio::inherit()
        })
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    command.as_std_mut().process_group(0);
    command
        .spawn()
        .map_err(|source| HostLifecycleError::Process { source })
}

async fn wait_host_children(
    children: &mut [Child],
    cancellation: &CancellationToken,
) -> Result<(), HostLifecycleError> {
    let mut statuses = vec![None; children.len()];
    loop {
        if cancellation.is_cancelled() {
            terminate_host_children(children).await;
            return Err(HostLifecycleError::Cancelled);
        }
        let mut complete = true;
        let mut failed = false;
        let mut control_error = None;
        for (child, status) in children.iter_mut().zip(&mut statuses) {
            if status.is_none() {
                match child.try_wait() {
                    Ok(result) => *status = result,
                    Err(source) => {
                        control_error = Some(source);
                        break;
                    }
                }
            }
            complete &= status.is_some();
            failed |= status.is_some_and(|status| !status.success());
        }
        if let Some(source) = control_error {
            terminate_host_children(children).await;
            return Err(HostLifecycleError::Process { source });
        }
        if failed {
            terminate_host_children(children).await;
            return Err(HostLifecycleError::Failed);
        }
        if complete {
            return Ok(());
        }
        sleep(PROCESS_POLL_INTERVAL).await;
    }
}

async fn terminate_host_children(children: &mut [Child]) {
    for child in &mut *children {
        if child.try_wait().ok().flatten().is_none() {
            signal_host_group(child, Signal::SIGTERM);
        }
    }
    let deadline = Instant::now() + PROCESS_TERMINATION_GRACE;
    while Instant::now() < deadline {
        let mut complete = true;
        for child in &mut *children {
            complete &= child.try_wait().ok().flatten().is_some();
        }
        if complete {
            return;
        }
        sleep(PROCESS_POLL_INTERVAL).await;
    }
    for child in &mut *children {
        if child.try_wait().ok().flatten().is_none() {
            signal_host_group(child, Signal::SIGKILL);
            let _ = child.wait().await;
        }
    }
}

fn signal_host_group(child: &Child, signal: Signal) {
    if let Some(pid) = child.id().and_then(|value| i32::try_from(value).ok()) {
        let _ = kill(Pid::from_raw(-pid), signal);
    }
}

use std::os::unix::process::CommandExt;
