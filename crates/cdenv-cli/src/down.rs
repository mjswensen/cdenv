//! Ordered, idempotent shutdown of one persisted managed environment.
//!
//! `down` deliberately consumes only active-generation identity. Desired
//! configuration is irrelevant: even invalid desired input must not prevent a
//! known active environment from being stopped safely.

use std::error::Error;
use std::future::Future;
use std::path::Path;
use std::time::Duration;

use cdenv_core::{ForegroundOperation, WorkspaceName};
use thiserror::Error;

use crate::{
    ActiveGeneration, CdenvRoot, LockBehavior, LockError, LockGuard, LockMode, OperationState,
    SanitizedSummary, WorkspaceState, WorkspaceStateError, load_workspace_state,
    persist_workspace_state,
};

/// Default graceful lifecycle-runner cancellation period.
pub const LIFECYCLE_STOP_GRACE: Duration = Duration::from_secs(5);
/// Default Docker container graceful-stop period.
pub const CONTAINER_STOP_GRACE: Duration = Duration::from_secs(10);

/// Result of stopping the workspace forwarding supervisor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForwardingStopOutcome {
    /// The authenticated workspace supervisor stopped and released its assets.
    Stopped,
    /// No supervisor was installed or running.
    Missing,
    /// Supervisor state was degraded and no unrelated process was signalled.
    Degraded,
}

/// Result of bounded background lifecycle cancellation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleStopOutcome {
    /// No matching runner was active.
    NotRunning,
    /// The runner exited during graceful cancellation.
    Graceful,
    /// The matching runner required bounded forced cleanup.
    Forced,
    /// One-time work may have executed without a definite result.
    Indeterminate,
}

/// Result of stopping the persisted complete managed environment.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentStopOutcome {
    /// At least one managed container transitioned to stopped.
    Stopped,
    /// Every persisted managed container was already stopped.
    AlreadyStopped,
    /// No active generation was persisted, or its known resources are absent.
    Missing,
}

/// How stale foreground intent is handled by a mutating command that owns the lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InterruptedOperationRecovery {
    /// No stale operation existed.
    None,
    /// Reaching a fully stopped state gives the prior transition a known outcome.
    Recoverable(ForegroundOperation),
    /// The prior transition remains diagnostically interrupted after shutdown.
    Retained(ForegroundOperation),
}

/// Classifies stale persisted intent after exclusive lock acquisition proves no
/// earlier foreground command is still active.
#[must_use]
pub const fn classify_interrupted_operation(
    operation: ForegroundOperation,
    lifecycle_indeterminate: bool,
) -> InterruptedOperationRecovery {
    match operation {
        ForegroundOperation::Idle => InterruptedOperationRecovery::None,
        ForegroundOperation::Stopping => {
            InterruptedOperationRecovery::Recoverable(ForegroundOperation::Stopping)
        }
        ForegroundOperation::Starting if !lifecycle_indeterminate => {
            InterruptedOperationRecovery::Recoverable(ForegroundOperation::Starting)
        }
        operation => InterruptedOperationRecovery::Retained(operation),
    }
}

/// Non-fatal shutdown diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DownWarning {
    /// No forwarding supervisor existed.
    ForwardingMissing,
    /// Degraded supervisor state was left alone rather than signalling by PID.
    ForwardingDegraded,
    /// Lifecycle cancellation escalated to forced cleanup.
    LifecycleForced,
    /// Known managed resources were absent.
    EnvironmentMissing,
    /// A prior foreground transition remains interrupted.
    PriorOperationInterrupted(ForegroundOperation),
}

/// Inputs for one locked `down` transaction.
pub struct DownRequest<'a> {
    /// Persisted workspace name.
    pub workspace: &'a WorkspaceName,
    /// Explicit durable stopping-operation identity.
    pub operation: OperationState,
    /// Bounded graceful lifecycle cancellation period.
    pub lifecycle_grace: Duration,
    /// Docker/Compose graceful stop period.
    pub container_grace: Duration,
}

impl<'a> DownRequest<'a> {
    /// Constructs a request using reviewed V1 grace periods.
    #[must_use]
    pub const fn new(workspace: &'a WorkspaceName, operation: OperationState) -> Self {
        Self {
            workspace,
            operation,
            lifecycle_grace: LIFECYCLE_STOP_GRACE,
            container_grace: CONTAINER_STOP_GRACE,
        }
    }
}

/// Authenticated forwarding-supervisor shutdown seam.
#[doc(hidden)]
pub trait ForwardingSupervisorStop: Send + Sync {
    /// Focused supervisor protocol or inspection error.
    type Error: Error + Send + Sync + 'static;

    /// Stops only the supervisor matching persisted workspace/generation identity.
    fn stop<'a>(
        &'a self,
        workspace: &'a WorkspaceName,
        active: Option<&'a ActiveGeneration>,
    ) -> impl Future<Output = Result<ForwardingStopOutcome, Self::Error>> + Send + 'a;
}

/// Bounded lifecycle-runner cancellation seam.
#[doc(hidden)]
pub trait LifecycleRunnerStop: Send + Sync {
    /// Focused agent transport or cancellation error.
    type Error: Error + Send + Sync + 'static;

    /// Gracefully cancels, waits, and force-cleans only the matching runner.
    fn cancel<'a>(
        &'a self,
        active: Option<&'a ActiveGeneration>,
        grace: Duration,
    ) -> impl Future<Output = Result<LifecycleStopOutcome, Self::Error>> + Send + 'a;
}

/// Image/Dockerfile or Compose managed-environment stop seam.
#[doc(hidden)]
pub trait ManagedEnvironmentStop: Send + Sync {
    /// Focused Bollard or Compose service-set error.
    type Error: Error + Send + Sync + 'static;

    /// Stops exactly the persisted primary or complete managed Compose set.
    fn stop<'a>(
        &'a self,
        active: Option<&'a ActiveGeneration>,
        grace: Duration,
    ) -> impl Future<Output = Result<EnvironmentStopOutcome, Self::Error>> + Send + 'a;
}

/// Errors from independently attempted stop phases.
#[derive(Debug)]
pub struct StopFailures<F, L, E> {
    /// Forwarding stop failure, if any.
    pub forwarding: Option<F>,
    /// Lifecycle cancellation failure, if any.
    pub lifecycle: Option<L>,
    /// Managed environment stop failure, if any.
    pub environment: Option<E>,
    /// Whether cancellation left one-time work indeterminate.
    pub lifecycle_indeterminate: bool,
}

impl<F, L, E> StopFailures<F, L, E> {
    /// Returns true when every phase has a definite successful outcome.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.forwarding.is_none()
            && self.lifecycle.is_none()
            && self.environment.is_none()
            && !self.lifecycle_indeterminate
    }
}

/// Successful complete shutdown facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownOutcome {
    /// Forwarding phase result.
    pub forwarding: ForwardingStopOutcome,
    /// Lifecycle phase result.
    pub lifecycle: LifecycleStopOutcome,
    /// Managed environment phase result.
    pub environment: EnvironmentStopOutcome,
    /// Recovery disposition of stale foreground intent.
    pub recovery: InterruptedOperationRecovery,
    /// Stable non-fatal diagnostics.
    pub warnings: Vec<DownWarning>,
}

/// Layered shutdown transaction failure.
#[derive(Debug, Error)]
pub enum DownError<F, L, E>
where
    F: Error + 'static,
    L: Error + 'static,
    E: Error + 'static,
{
    /// The caller did not supply explicit stopping intent.
    #[error("down requires a persisted stopping operation")]
    InvalidOperation,
    /// Workspace lock acquisition failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Existing state could not be loaded or the stopping operation could not be persisted.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// One or more ordered stop phases failed after all phases were attempted.
    #[error("managed shutdown was incomplete")]
    Incomplete {
        /// Independently retained phase failures.
        failures: StopFailures<F, L, E>,
    },
    /// Phase outcomes were retained but recoverable failure state could not be persisted.
    #[error("managed shutdown was incomplete and failure state could not be persisted: {state}")]
    FailureRecord {
        /// Independently retained phase failures.
        failures: StopFailures<F, L, E>,
        /// Atomic state persistence failure.
        #[source]
        state: WorkspaceStateError,
    },
}

/// Stops forwarding, lifecycle work, and the complete persisted environment in order.
///
/// Every phase is attempted even if an earlier phase fails. The checkout,
/// active intent, images, containers, networks, and named volumes are never
/// removed by this coordinator.
///
/// # Errors
///
/// Returns lock/state failures immediately. Ordered stop-phase failures are
/// accumulated and returned only after later phases and failure persistence.
pub async fn down_workspace<F, L, E>(
    root: &CdenvRoot,
    request: DownRequest<'_>,
    forwarding: &F,
    lifecycle: &L,
    environment: &E,
) -> Result<DownOutcome, DownError<F::Error, L::Error, E::Error>>
where
    F: ForwardingSupervisorStop,
    L: LifecycleRunnerStop,
    E: ManagedEnvironmentStop,
{
    if request.operation.kind() != ForegroundOperation::Stopping {
        return Err(DownError::InvalidOperation);
    }
    let paths = root.workspace(request.workspace);
    let _lock = LockGuard::acquire(&paths.lock_file(), LockMode::Exclusive, LockBehavior::Wait)?;
    let mut state = load_workspace_state(&paths.state_file())?.into_state();
    let recovery = classify_interrupted_operation(
        state.operation().kind(),
        state
            .active()
            .is_some_and(|active| active.lifecycle().indeterminate()),
    );
    state.set_operation(request.operation);
    state.set_last_error(None);
    persist_workspace_state(&paths.state_file(), &state)?;

    let forwarding_result = forwarding.stop(request.workspace, state.active()).await;
    let lifecycle_result = lifecycle
        .cancel(state.active(), request.lifecycle_grace)
        .await;
    if let Ok(outcome) = lifecycle_result {
        let indeterminate = outcome == LifecycleStopOutcome::Indeterminate;
        if let Some(active) = state.active_mut() {
            active.record_lifecycle_stopped(indeterminate);
        }
    }
    let environment_result = environment
        .stop(state.active(), request.container_grace)
        .await;

    finish_down(
        &paths.state_file(),
        state,
        recovery,
        forwarding_result,
        lifecycle_result,
        environment_result,
    )
}

fn finish_down<F, L, E>(
    state_path: &Path,
    mut state: WorkspaceState,
    recovery: InterruptedOperationRecovery,
    forwarding: Result<ForwardingStopOutcome, F>,
    lifecycle: Result<LifecycleStopOutcome, L>,
    environment: Result<EnvironmentStopOutcome, E>,
) -> Result<DownOutcome, DownError<F, L, E>>
where
    F: Error + 'static,
    L: Error + 'static,
    E: Error + 'static,
{
    let lifecycle_indeterminate = lifecycle
        .as_ref()
        .is_ok_and(|outcome| *outcome == LifecycleStopOutcome::Indeterminate);
    let failures = StopFailures {
        forwarding: forwarding.as_ref().err().map(|_| ()),
        lifecycle: lifecycle.as_ref().err().map(|_| ()),
        environment: environment.as_ref().err().map(|_| ()),
        lifecycle_indeterminate,
    };
    let successful = failures.is_empty();
    state.set_operation(OperationState::idle());
    state.set_last_error(failure_summary(successful, recovery, &failures));

    if let Err(state_error) = persist_workspace_state(state_path, &state) {
        return Err(DownError::FailureRecord {
            failures: StopFailures {
                forwarding: forwarding.err(),
                lifecycle: lifecycle.err(),
                environment: environment.err(),
                lifecycle_indeterminate,
            },
            state: state_error,
        });
    }
    if !successful {
        return Err(DownError::Incomplete {
            failures: StopFailures {
                forwarding: forwarding.err(),
                lifecycle: lifecycle.err(),
                environment: environment.err(),
                lifecycle_indeterminate,
            },
        });
    }

    // All values are known to be Ok after `successful` was established.
    let (Ok(forwarding), Ok(lifecycle), Ok(environment)) = (forwarding, lifecycle, environment)
    else {
        unreachable!("successful shutdown has no phase errors");
    };
    let warnings = warnings(forwarding, lifecycle, environment, recovery);
    Ok(DownOutcome {
        forwarding,
        lifecycle,
        environment,
        recovery,
        warnings,
    })
}

fn failure_summary<F, L, E>(
    successful: bool,
    recovery: InterruptedOperationRecovery,
    failures: &StopFailures<F, L, E>,
) -> Option<SanitizedSummary> {
    if !successful {
        let mut phases = Vec::new();
        if failures.forwarding.is_some() {
            phases.push("forwarding");
        }
        if failures.lifecycle.is_some() || failures.lifecycle_indeterminate {
            phases.push("lifecycle");
        }
        if failures.environment.is_some() {
            phases.push("environment");
        }
        return Some(SanitizedSummary::redact(
            &format!("down incomplete during {}", phases.join(", ")),
            [],
        ));
    }
    if let InterruptedOperationRecovery::Retained(operation) = recovery {
        return Some(SanitizedSummary::redact(
            &format!("previous {operation:?} operation was interrupted"),
            [],
        ));
    }
    None
}

fn warnings(
    forwarding: ForwardingStopOutcome,
    lifecycle: LifecycleStopOutcome,
    environment: EnvironmentStopOutcome,
    recovery: InterruptedOperationRecovery,
) -> Vec<DownWarning> {
    let mut warnings = Vec::new();
    match forwarding {
        ForwardingStopOutcome::Stopped => {}
        ForwardingStopOutcome::Missing => warnings.push(DownWarning::ForwardingMissing),
        ForwardingStopOutcome::Degraded => warnings.push(DownWarning::ForwardingDegraded),
    }
    if lifecycle == LifecycleStopOutcome::Forced {
        warnings.push(DownWarning::LifecycleForced);
    }
    if environment == EnvironmentStopOutcome::Missing {
        warnings.push(DownWarning::EnvironmentMissing);
    }
    if let InterruptedOperationRecovery::Retained(operation) = recovery {
        warnings.push(DownWarning::PriorOperationInterrupted(operation));
    }
    warnings
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use crate::{RootEnvironment, StateTimestamp, decode_workspace_state, ensure_lock_file};

    use super::*;

    struct NoEnvironment;

    impl RootEnvironment for NoEnvironment {
        fn cdenv_home(&self) -> Option<OsString> {
            None
        }

        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    #[derive(Clone)]
    struct OrderedStops {
        calls: Arc<Mutex<Vec<&'static str>>>,
        environment_outcomes: Arc<Mutex<Vec<EnvironmentStopOutcome>>>,
        lifecycle_outcome: LifecycleStopOutcome,
        fail_forwarding: bool,
    }

    impl ForwardingSupervisorStop for OrderedStops {
        type Error = io::Error;

        async fn stop(
            &self,
            _workspace: &WorkspaceName,
            _active: Option<&ActiveGeneration>,
        ) -> Result<ForwardingStopOutcome, Self::Error> {
            self.calls.lock().expect("calls").push("forwarding");
            if self.fail_forwarding {
                Err(io::Error::other("supervisor failure"))
            } else {
                Ok(ForwardingStopOutcome::Missing)
            }
        }
    }

    impl LifecycleRunnerStop for OrderedStops {
        type Error = io::Error;

        async fn cancel(
            &self,
            _active: Option<&ActiveGeneration>,
            _grace: Duration,
        ) -> Result<LifecycleStopOutcome, Self::Error> {
            self.calls.lock().expect("calls").push("lifecycle");
            Ok(self.lifecycle_outcome)
        }
    }

    impl ManagedEnvironmentStop for OrderedStops {
        type Error = io::Error;

        async fn stop(
            &self,
            _active: Option<&ActiveGeneration>,
            _grace: Duration,
        ) -> Result<EnvironmentStopOutcome, Self::Error> {
            self.calls.lock().expect("calls").push("environment");
            Ok(self
                .environment_outcomes
                .lock()
                .expect("outcomes")
                .remove(0))
        }
    }

    fn workspace_fixture() -> (tempfile::TempDir, CdenvRoot, WorkspaceName) {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = CdenvRoot::resolve(Some(&temporary.path().join("root")), &NoEnvironment)
            .expect("test root");
        let workspace = WorkspaceName::parse("project").expect("workspace");
        let paths = root.workspace(&workspace);
        fs::create_dir_all(paths.checkout()).expect("checkout");
        ensure_lock_file(&paths.lock_file()).expect("lock file");
        let state = decode_workspace_state(
            Path::new("fixture.json"),
            include_bytes!("../tests/fixtures/state-compose-background.json"),
        )
        .expect("fixture")
        .into_state();
        persist_workspace_state(&paths.state_file(), &state).expect("state");
        (temporary, root, workspace)
    }

    fn operation() -> OperationState {
        OperationState::active(
            ForegroundOperation::Stopping,
            "down-operation".to_owned(),
            StateTimestamp::parse("2025-01-02T05:00:00Z").expect("timestamp"),
        )
        .expect("operation")
    }

    #[tokio::test]
    async fn stop_order_is_fixed_and_repeated_down_is_idempotent() {
        let (_temporary, root, workspace) = workspace_fixture();
        let checkout_marker = root
            .workspace(&workspace)
            .checkout()
            .join("uncommitted.txt");
        fs::write(&checkout_marker, b"preserve me").expect("checkout change");
        let stops = OrderedStops {
            calls: Arc::new(Mutex::new(Vec::new())),
            environment_outcomes: Arc::new(Mutex::new(vec![
                EnvironmentStopOutcome::Stopped,
                EnvironmentStopOutcome::AlreadyStopped,
            ])),
            lifecycle_outcome: LifecycleStopOutcome::Forced,
            fail_forwarding: false,
        };

        let first = down_workspace(
            &root,
            DownRequest::new(&workspace, operation()),
            &stops,
            &stops,
            &stops,
        )
        .await
        .expect("first down");
        let second = down_workspace(
            &root,
            DownRequest::new(&workspace, operation()),
            &stops,
            &stops,
            &stops,
        )
        .await
        .expect("repeated down");

        assert_eq!(first.environment, EnvironmentStopOutcome::Stopped);
        assert_eq!(second.environment, EnvironmentStopOutcome::AlreadyStopped);
        assert_eq!(
            fs::read(checkout_marker).expect("checkout retained"),
            b"preserve me"
        );
        assert_eq!(
            *stops.calls.lock().expect("calls"),
            vec![
                "forwarding",
                "lifecycle",
                "environment",
                "forwarding",
                "lifecycle",
                "environment",
            ]
        );
        let state = load_workspace_state(&root.workspace(&workspace).state_file())
            .expect("persisted state");
        assert_eq!(state.state().operation().kind(), ForegroundOperation::Idle);
        assert!(state.state().last_error().is_none());
        assert!(
            !state
                .state()
                .active()
                .expect("active")
                .lifecycle()
                .indeterminate()
        );
    }

    #[tokio::test]
    async fn an_early_failure_does_not_skip_later_stop_phases() {
        let (_temporary, root, workspace) = workspace_fixture();
        let stops = OrderedStops {
            calls: Arc::new(Mutex::new(Vec::new())),
            environment_outcomes: Arc::new(Mutex::new(vec![EnvironmentStopOutcome::Stopped])),
            lifecycle_outcome: LifecycleStopOutcome::Forced,
            fail_forwarding: true,
        };

        let error = down_workspace(
            &root,
            DownRequest::new(&workspace, operation()),
            &stops,
            &stops,
            &stops,
        )
        .await
        .expect_err("forwarding failure should be retained");

        assert!(matches!(error, DownError::Incomplete { .. }));
        assert_eq!(
            *stops.calls.lock().expect("calls"),
            vec!["forwarding", "lifecycle", "environment"]
        );
        let state = load_workspace_state(&root.workspace(&workspace).state_file())
            .expect("persisted state");
        assert!(state.state().last_error().is_some());
    }

    #[tokio::test]
    async fn indeterminate_lifecycle_is_recorded_and_requires_rebuild() {
        let (_temporary, root, workspace) = workspace_fixture();
        let stops = OrderedStops {
            calls: Arc::new(Mutex::new(Vec::new())),
            environment_outcomes: Arc::new(Mutex::new(vec![EnvironmentStopOutcome::Stopped])),
            lifecycle_outcome: LifecycleStopOutcome::Indeterminate,
            fail_forwarding: false,
        };

        let error = down_workspace(
            &root,
            DownRequest::new(&workspace, operation()),
            &stops,
            &stops,
            &stops,
        )
        .await
        .expect_err("indeterminate lifecycle must remain unhealthy");
        let DownError::Incomplete { failures } = error else {
            panic!("expected retained phase failures");
        };
        assert!(failures.lifecycle_indeterminate);
        let state = load_workspace_state(&root.workspace(&workspace).state_file())
            .expect("persisted state");
        assert!(
            state
                .state()
                .active()
                .expect("active")
                .lifecycle()
                .indeterminate()
        );
    }

    #[test]
    fn stale_operation_recovery_requires_a_known_transition() {
        assert_eq!(
            classify_interrupted_operation(ForegroundOperation::Stopping, false),
            InterruptedOperationRecovery::Recoverable(ForegroundOperation::Stopping)
        );
        assert_eq!(
            classify_interrupted_operation(ForegroundOperation::Starting, true),
            InterruptedOperationRecovery::Retained(ForegroundOperation::Starting)
        );
        assert_eq!(
            classify_interrupted_operation(ForegroundOperation::Rebuilding, false),
            InterruptedOperationRecovery::Retained(ForegroundOperation::Rebuilding)
        );
    }
}
