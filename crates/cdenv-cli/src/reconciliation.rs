//! Shared desired/active reconciliation transaction for `create` and `up`.
//!
//! This module owns the persistence and commit boundary while adapters own plan
//! construction, dependency checks, Docker mutation, provisioning, lifecycle,
//! and runtime-only application. Keeping those seams separate makes it
//! impossible for orchestration to silently turn ordinary `up` into rebuild.

use std::error::Error;
use std::future::Future;
use std::path::Path;

use cdenv_core::{GenerationId, ProfileId, WorkspaceName};
use thiserror::Error;

use crate::{
    ActiveGeneration, CdenvRoot, DesiredConfigPath, LockBehavior, LockError, LockGuard, LockMode,
    OperationState, PlanFingerprints, RepoRelativeConfigPath, SanitizedSummary, StateTimestamp,
    WorkspaceState, WorkspaceStateError, load_workspace_state, persist_workspace_state,
};

/// Whether Feature inputs may be fetched while preparing an `up` plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FeatureSourcePolicy {
    /// Initial creation may resolve the complete frozen Feature input set.
    ResolveForCreation,
    /// An existing generation must be evaluated from its frozen local inputs.
    ///
    /// In particular, merely detecting desired drift must not contact a registry.
    FrozenOffline,
}

/// Independently classified desired-versus-active categories.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CategoryDrift {
    /// Build inputs differ and require an explicit rebuild to take effect.
    pub build: bool,
    /// Container create inputs differ and require an explicit rebuild.
    pub create: bool,
    /// Runtime inputs differ and may be applied independently.
    pub runtime: bool,
}

impl CategoryDrift {
    fn between(desired: &PlanFingerprints, active: &PlanFingerprints) -> Self {
        Self {
            build: desired.build() != active.build(),
            create: desired.create() != active.create(),
            runtime: desired.runtime() != active.runtime(),
        }
    }
}

/// A non-fatal fact emitted by reconciliation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReconciliationWarning {
    /// Desired build inputs are deferred until explicit rebuild.
    BuildDrift,
    /// Desired create inputs are deferred until explicit rebuild.
    CreateDrift,
}

/// Observable environment transition performed by an adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EnvironmentTransition {
    /// The first environment generation was created and started.
    Created,
    /// Recorded stopped containers were started directly.
    Started,
    /// The environment was already running and was reverified.
    AlreadyRunning,
}

/// Whether independently valid runtime drift was installed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeReconciliation {
    /// Runtime configuration was already current.
    Current,
    /// Runtime configuration was applied to the existing generation.
    Applied,
}

/// A validated desired plan whose category material is safe to persist.
///
/// The opaque `plan` remains owned until the environment adapter establishes
/// complete readiness. Only then can its returned active record be committed.
pub struct PreparedDesiredPlan<P> {
    /// Pinned supported profile.
    pub profile: ProfileId,
    /// Canonically contained repository-relative selection.
    pub config: DesiredConfigPath,
    /// Opaque keyed category fingerprints.
    pub fingerprints: PlanFingerprints,
    /// Adapter-specific effective plan.
    pub plan: P,
}

/// Command inputs shared by post-clone `create` and ordinary `up`.
pub struct ReconciliationRequest<'a> {
    /// Workspace whose exclusive lifecycle lock is acquired.
    pub workspace: &'a WorkspaceName,
    /// Explicit selection for this invocation, if supplied.
    pub config: Option<&'a RepoRelativeConfigPath>,
    /// Durable foreground operation identity prepared by the caller.
    pub operation: OperationState,
    /// Time used only if complete readiness is committed.
    pub completed_at: StateTimestamp,
}

/// Plan construction and command-specific preflight seam.
#[doc(hidden)]
pub trait ReconciliationPlanner: Send + Sync {
    /// Effective desired plan retained through reconciliation.
    type Plan: Send + Sync;
    /// Validation or planning failure.
    type PlanError: Error + Send + Sync + 'static;
    /// Dependency, profile, or frozen-source preflight failure.
    type PreflightError: Error + Send + Sync + 'static;

    /// Discovers, validates, and plans desired configuration without mutation.
    fn prepare(
        &self,
        checkout: &Path,
        explicit: Option<&Path>,
        current: &WorkspaceState,
        feature_policy: FeatureSourcePolicy,
    ) -> Result<PreparedDesiredPlan<Self::Plan>, Self::PlanError>;

    /// Runs command-specific checks before any Docker image or container work.
    fn preflight<'a>(
        &'a self,
        desired: &'a PreparedDesiredPlan<Self::Plan>,
        feature_policy: FeatureSourcePolicy,
    ) -> impl Future<Output = Result<(), Self::PreflightError>> + Send + 'a;
}

/// Borrowed immutable inputs to the Docker/lifecycle adapter.
pub struct EnvironmentReconciliationRequest<'a, P> {
    /// Validated desired effective plan.
    pub desired: &'a PreparedDesiredPlan<P>,
    /// Last completely ready generation, if one exists.
    pub active: Option<&'a ActiveGeneration>,
    /// Category drift. Every field is false for initial creation.
    pub drift: CategoryDrift,
}

/// Complete readiness facts returned for the sole active-state commit.
pub struct ReadyEnvironment {
    /// Fully provisioned generation record.
    pub active: ActiveGeneration,
    /// Live transition performed without implicit replacement.
    pub transition: EnvironmentTransition,
    /// Runtime-only application result.
    pub runtime: RuntimeReconciliation,
}

/// Docker, provisioning, lifecycle, and runtime-forwarding seam.
#[doc(hidden)]
pub trait EnvironmentReconciler<P>: Send + Sync {
    /// Focused adapter error.
    type Error: Error + Send + Sync + 'static;

    /// Reconciles to readiness without writing workspace state.
    fn reconcile<'a>(
        &'a self,
        request: EnvironmentReconciliationRequest<'a, P>,
    ) -> impl Future<Output = Result<ReadyEnvironment, Self::Error>> + Send + 'a;
}

/// Successful shared reconciliation result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationOutcome {
    /// Generation committed after complete readiness.
    pub generation: GenerationId,
    /// Environment transition.
    pub transition: EnvironmentTransition,
    /// Runtime-only application result.
    pub runtime: RuntimeReconciliation,
    /// Deferred build/create drift warnings.
    pub warnings: Vec<ReconciliationWarning>,
}

/// Layered failure from the shared transaction.
#[derive(Debug, Error)]
pub enum ReconciliationError<P, F, E>
where
    P: Error + 'static,
    F: Error + 'static,
    E: Error + 'static,
{
    /// Workspace lock acquisition failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Existing desired/active state could not be loaded or persisted.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// Desired configuration is invalid or unsupported; Docker was untouched.
    #[error("desired configuration is invalid: {source}")]
    Desired {
        /// Focused parser/planner source.
        #[source]
        source: P,
    },
    /// Dependency, profile, or frozen Feature preflight failed before Docker work.
    #[error("reconciliation preflight failed: {source}")]
    Preflight {
        /// Focused preflight source.
        #[source]
        source: F,
    },
    /// A prior one-time lifecycle stage is unsafe to retry with `up`.
    #[error("one-time lifecycle work is indeterminate; run `cdenv rebuild` before retrying")]
    IndeterminateLifecycle,
    /// Docker or lifecycle readiness failed; the previous active record was retained.
    #[error("environment reconciliation failed: {source}")]
    Environment {
        /// Focused environment source.
        #[source]
        source: E,
    },
    /// An ordinary `up` adapter attempted to replace an active generation.
    #[error("ordinary reconciliation attempted an implicit generation replacement")]
    ImplicitReplacement,
    /// Returned readiness did not preserve immutable build/create identity or apply runtime drift.
    #[error("environment readiness did not preserve the required category commit boundary")]
    InvalidReadiness,
    /// The primary error was retained but recording recoverable state failed.
    #[error("{message}; recoverable failure state could not be persisted: {state}")]
    FailureRecord {
        /// Credential-safe primary summary.
        message: String,
        /// Persistence failure.
        #[source]
        state: WorkspaceStateError,
    },
}

/// Reconciles a workspace while owning its exclusive lifecycle lock.
///
/// Desired validation completes before state or Docker mutation. Desired intent
/// is then persisted before preflight and lifecycle execution. No failure path
/// replaces `active`; complete readiness is the only commit point.
///
/// # Errors
///
/// Returns a typed locking, state, desired, preflight, retry-safety, adapter, or
/// commit-boundary error.
pub async fn reconcile_workspace<P, E>(
    root: &CdenvRoot,
    request: ReconciliationRequest<'_>,
    planner: &P,
    environment: &E,
) -> Result<ReconciliationOutcome, ReconciliationError<P::PlanError, P::PreflightError, E::Error>>
where
    P: ReconciliationPlanner,
    E: EnvironmentReconciler<P::Plan>,
{
    let paths = root.workspace(request.workspace);
    let _lock = LockGuard::acquire(&paths.lock_file(), LockMode::Exclusive, LockBehavior::Wait)?;
    let mut state = load_workspace_state(&paths.state_file())?.into_state();
    let feature_policy = if state.active().is_some() {
        FeatureSourcePolicy::FrozenOffline
    } else {
        FeatureSourcePolicy::ResolveForCreation
    };
    let desired = planner
        .prepare(
            &paths.checkout(),
            request.config.map(RepoRelativeConfigPath::as_path),
            &state,
            feature_policy,
        )
        .map_err(|source| ReconciliationError::Desired { source })?;

    if state
        .active()
        .is_some_and(|active| active.lifecycle().indeterminate())
    {
        return Err(ReconciliationError::IndeterminateLifecycle);
    }

    let previous = state.active().cloned();
    let drift = previous
        .as_ref()
        .map_or_else(CategoryDrift::default, |active| {
            CategoryDrift::between(&desired.fingerprints, active.fingerprints())
        });
    state.update_desired(
        desired.profile.clone(),
        desired.config.clone(),
        desired.fingerprints.clone(),
    );
    state.set_operation(request.operation);
    state.set_last_error(None);
    persist_workspace_state(&paths.state_file(), &state)?;

    if let Err(source) = planner.preflight(&desired, feature_policy).await {
        let message = source.to_string();
        record_failure(&paths.state_file(), &mut state, &message).map_err(|state| {
            ReconciliationError::FailureRecord {
                message: message.clone(),
                state,
            }
        })?;
        return Err(ReconciliationError::Preflight { source });
    }

    let mut ready = match environment
        .reconcile(EnvironmentReconciliationRequest {
            desired: &desired,
            active: previous.as_ref(),
            drift,
        })
        .await
    {
        Ok(ready) => ready,
        Err(source) => {
            let message = source.to_string();
            record_failure(&paths.state_file(), &mut state, &message).map_err(|state| {
                ReconciliationError::FailureRecord {
                    message: message.clone(),
                    state,
                }
            })?;
            return Err(ReconciliationError::Environment { source });
        }
    };

    if let Err(boundary) = validate_readiness(previous.as_ref(), &desired, drift, &mut ready) {
        record_failure(&paths.state_file(), &mut state, boundary.message())?;
        return Err(match boundary {
            ReadinessBoundaryError::ImplicitReplacement => ReconciliationError::ImplicitReplacement,
            ReadinessBoundaryError::InvalidCategories => ReconciliationError::InvalidReadiness,
        });
    }

    let generation = ready.active.generation();
    state.commit_active(ready.active, request.completed_at);
    persist_workspace_state(&paths.state_file(), &state)?;
    let mut warnings = Vec::new();
    if drift.build {
        warnings.push(ReconciliationWarning::BuildDrift);
    }
    if drift.create {
        warnings.push(ReconciliationWarning::CreateDrift);
    }
    Ok(ReconciliationOutcome {
        generation,
        transition: ready.transition,
        runtime: ready.runtime,
        warnings,
    })
}

#[derive(Clone, Copy)]
enum ReadinessBoundaryError {
    ImplicitReplacement,
    InvalidCategories,
}

impl ReadinessBoundaryError {
    const fn message(self) -> &'static str {
        match self {
            Self::ImplicitReplacement => {
                "ordinary reconciliation attempted an implicit generation replacement"
            }
            Self::InvalidCategories => {
                "environment readiness violated the category commit boundary"
            }
        }
    }
}

fn validate_readiness<P>(
    previous: Option<&ActiveGeneration>,
    desired: &PreparedDesiredPlan<P>,
    drift: CategoryDrift,
    ready: &mut ReadyEnvironment,
) -> Result<(), ReadinessBoundaryError> {
    if previous.is_some_and(|active| active.generation() != ready.active.generation()) {
        return Err(ReadinessBoundaryError::ImplicitReplacement);
    }
    let valid_categories = previous.map_or_else(
        || ready.active.fingerprints() == &desired.fingerprints,
        |active| {
            ready.active.fingerprints().build() == active.fingerprints().build()
                && ready.active.fingerprints().create() == active.fingerprints().create()
                && ready.runtime
                    == if drift.runtime {
                        RuntimeReconciliation::Applied
                    } else {
                        RuntimeReconciliation::Current
                    }
        },
    );
    if !valid_categories {
        return Err(ReadinessBoundaryError::InvalidCategories);
    }
    if drift.runtime {
        ready
            .active
            .commit_runtime_fingerprint(desired.fingerprints.runtime().clone());
    }
    Ok(())
}

fn record_failure(
    state_path: &Path,
    state: &mut WorkspaceState,
    message: &str,
) -> Result<(), WorkspaceStateError> {
    state.set_operation(OperationState::idle());
    state.set_last_error(Some(SanitizedSummary::redact(message, [])));
    persist_workspace_state(state_path, state)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use cdenv_core::ForegroundOperation;

    use crate::{
        KeyedDigest, RootEnvironment, ensure_lock_file, load_workspace_state,
        persist_workspace_state,
    };

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
    struct FakePlanner {
        fingerprints: PlanFingerprints,
        policies: Arc<Mutex<Vec<FeatureSourcePolicy>>>,
    }

    impl ReconciliationPlanner for FakePlanner {
        type Plan = ();
        type PlanError = std::io::Error;
        type PreflightError = std::io::Error;

        fn prepare(
            &self,
            _checkout: &Path,
            _explicit: Option<&Path>,
            _current: &WorkspaceState,
            feature_policy: FeatureSourcePolicy,
        ) -> Result<PreparedDesiredPlan<Self::Plan>, Self::PlanError> {
            self.policies.lock().expect("policies").push(feature_policy);
            Ok(PreparedDesiredPlan {
                profile: ProfileId::parse("cdenv-devcontainer-v1").expect("profile"),
                config: DesiredConfigPath::parse(".devcontainer/devcontainer.json")
                    .expect("config"),
                fingerprints: self.fingerprints.clone(),
                plan: (),
            })
        }

        async fn preflight(
            &self,
            _desired: &PreparedDesiredPlan<Self::Plan>,
            feature_policy: FeatureSourcePolicy,
        ) -> Result<(), Self::PreflightError> {
            self.policies.lock().expect("policies").push(feature_policy);
            Ok(())
        }
    }

    #[derive(Clone)]
    struct FakeEnvironment {
        calls: Arc<Mutex<Vec<CategoryDrift>>>,
    }

    impl EnvironmentReconciler<()> for FakeEnvironment {
        type Error = std::io::Error;

        async fn reconcile(
            &self,
            request: EnvironmentReconciliationRequest<'_, ()>,
        ) -> Result<ReadyEnvironment, Self::Error> {
            self.calls.lock().expect("calls").push(request.drift);
            Ok(ReadyEnvironment {
                active: request.active.expect("fixture active generation").clone(),
                transition: EnvironmentTransition::AlreadyRunning,
                runtime: if request.drift.runtime {
                    RuntimeReconciliation::Applied
                } else {
                    RuntimeReconciliation::Current
                },
            })
        }
    }

    fn digest(character: char) -> KeyedDigest {
        KeyedDigest::parse(&format!("keyed:{}", character.to_string().repeat(64)))
            .expect("test digest")
    }

    fn fingerprints(values: [char; 3]) -> PlanFingerprints {
        PlanFingerprints::new(digest(values[0]), digest(values[1]), digest(values[2]))
    }

    #[test]
    fn category_drift_remains_independent() {
        let active = fingerprints(['1', '2', '3']);
        let desired = fingerprints(['4', '2', '5']);

        assert_eq!(
            CategoryDrift::between(&desired, &active),
            CategoryDrift {
                build: true,
                create: false,
                runtime: true,
            }
        );
    }

    #[test]
    fn unchanged_categories_are_idempotent() {
        let active = fingerprints(['a', 'b', 'c']);

        assert_eq!(
            CategoryDrift::between(&active, &active),
            CategoryDrift::default()
        );
    }

    #[tokio::test]
    async fn existing_up_is_offline_applies_only_runtime_and_reprovisions_every_time() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = CdenvRoot::resolve(Some(&temporary.path().join("root")), &NoEnvironment)
            .expect("test root");
        let workspace = WorkspaceName::parse("project").expect("workspace");
        let paths = root.workspace(&workspace);
        fs::create_dir_all(paths.checkout()).expect("workspace checkout");
        ensure_lock_file(&paths.lock_file()).expect("lock file");
        let fixture = include_bytes!("../tests/fixtures/state-compose-background.json");
        let state = crate::decode_workspace_state(Path::new("fixture.json"), fixture)
            .expect("fixture state")
            .into_state();
        persist_workspace_state(&paths.state_file(), &state).expect("initial state");

        let planner = FakePlanner {
            fingerprints: fingerprints(['9', '2', '5']),
            policies: Arc::new(Mutex::new(Vec::new())),
        };
        let environment = FakeEnvironment {
            calls: Arc::new(Mutex::new(Vec::new())),
        };
        let operation = || {
            OperationState::active(
                ForegroundOperation::Starting,
                "up-operation".to_owned(),
                StateTimestamp::parse("2025-01-02T04:00:00Z").expect("timestamp"),
            )
            .expect("operation")
        };
        let completed =
            || StateTimestamp::parse("2025-01-02T04:01:00Z").expect("completion timestamp");

        let first = reconcile_workspace(
            &root,
            ReconciliationRequest {
                workspace: &workspace,
                config: None,
                operation: operation(),
                completed_at: completed(),
            },
            &planner,
            &environment,
        )
        .await
        .expect("first up");
        let second = reconcile_workspace(
            &root,
            ReconciliationRequest {
                workspace: &workspace,
                config: None,
                operation: operation(),
                completed_at: completed(),
            },
            &planner,
            &environment,
        )
        .await
        .expect("idempotent up");

        assert_eq!(first.warnings, vec![ReconciliationWarning::BuildDrift]);
        assert_eq!(second.runtime, RuntimeReconciliation::Current);
        assert_eq!(
            *planner.policies.lock().expect("policies"),
            vec![FeatureSourcePolicy::FrozenOffline; 4]
        );
        assert_eq!(
            *environment.calls.lock().expect("calls"),
            vec![
                CategoryDrift {
                    build: true,
                    create: false,
                    runtime: true,
                },
                CategoryDrift {
                    build: true,
                    create: false,
                    runtime: false,
                },
            ]
        );
        let persisted = load_workspace_state(&paths.state_file()).expect("committed state");
        let active = persisted.state().active().expect("active generation");
        assert_eq!(active.generation().get(), 3);
        assert_eq!(active.fingerprints().build(), &digest('1'));
        assert_eq!(active.fingerprints().runtime(), &digest('5'));
    }
}
