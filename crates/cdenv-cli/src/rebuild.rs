//! Build-first replacement transactions with explicit rollback phases.

use std::error::Error;
use std::future::Future;
use std::path::Path;

use cdenv_core::{ContainerId, ForegroundOperation, GenerationId, ProfileId, WorkspaceName};
use thiserror::Error;

use crate::{
    ActiveGeneration, CdenvRoot, DesiredConfigPath, ImageId, LockBehavior, LockError, LockGuard,
    LockMode, OperationState, PlanFingerprints, RepoRelativeConfigPath, SanitizedSummary,
    StateTimestamp, WorkspaceState, WorkspaceStateError, load_workspace_state,
    persist_workspace_state,
};

/// Number of newest generated image generations retained for quick rollback.
pub const GENERATED_IMAGE_HISTORY: usize = 2;

/// A fully validated desired plan retained through replacement.
pub struct PreparedRebuildPlan<P> {
    /// Pinned compatibility profile.
    pub profile: ProfileId,
    /// Canonically contained selected configuration.
    pub config: DesiredConfigPath,
    /// Complete keyed desired fingerprints.
    pub fingerprints: PlanFingerprints,
    /// Adapter-specific immutable effective plan.
    pub plan: P,
}

/// Full desired-plan and frozen-lock validation seam.
#[doc(hidden)]
pub trait RebuildPlanner: Send + Sync {
    /// Effective plan type.
    type Plan: Send + Sync;
    /// Parse, validation, or planning error.
    type PlanError: Error + Send + Sync + 'static;
    /// Frozen Feature lock or dependency error.
    type PreflightError: Error + Send + Sync + 'static;

    /// Recomputes the complete desired plan without mutation.
    fn prepare(
        &self,
        checkout: &Path,
        explicit: Option<&Path>,
        current: &WorkspaceState,
    ) -> Result<PreparedRebuildPlan<Self::Plan>, Self::PlanError>;

    /// Validates every frozen input before Docker mutation.
    fn validate_frozen<'a>(
        &'a self,
        desired: &'a PreparedRebuildPlan<Self::Plan>,
    ) -> impl Future<Output = Result<(), Self::PreflightError>> + Send + 'a;
}

/// Checkout dirtiness probe. Implementations must never return paths or porcelain bytes.
#[doc(hidden)]
pub trait CheckoutStatus: Send + Sync {
    /// Focused status-command error.
    type Error: Error + Send + Sync + 'static;

    /// Runs `git status --porcelain` and returns only whether output existed.
    fn has_changes<'a>(
        &'a self,
        checkout: &'a Path,
    ) -> impl Future<Output = Result<bool, Self::Error>> + Send + 'a;
}

impl CheckoutStatus for crate::GitAdapter {
    type Error = crate::GitError;

    async fn has_changes(&self, checkout: &Path) -> Result<bool, Self::Error> {
        self.checkout_has_changes(checkout)
    }
}

/// Explicitly persisted/recoverable phases of an image replacement.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RebuildPhase {
    /// Replacement image preparation has completed while the old environment remains active.
    Built,
    /// Forwarding and background lifecycle work are being quiesced.
    Quiescing,
    /// The old primary has been stopped and renamed to the private backup name.
    BackupRenamed,
    /// A distinct replacement completed lifecycle, agent, SSH, and environment provisioning.
    CandidateReady,
    /// Declared forwarding has transactionally moved to the replacement.
    ForwardingHandedOff,
}

/// Context supplied for best-effort restoration and operation-owned cleanup.
pub struct RebuildRollbackRequest<'a, B> {
    /// Build-first claim used to clean only operation-owned generated inputs.
    pub build: &'a B,
    /// Exact replacement generation owning candidate inputs.
    pub generation: GenerationId,
    /// Last committed generation, if one existed.
    pub previous: Option<&'a ActiveGeneration>,
    /// Fully prepared candidate, if readiness completed.
    pub candidate: Option<&'a ActiveGeneration>,
    /// Private operation-owned backup name.
    pub backup_name: &'a str,
    /// Last phase reached.
    pub phase: RebuildPhase,
}

/// Context supplied for warning-only post-commit cleanup.
pub struct RebuildCleanupRequest<'a, B> {
    /// Previously active generation.
    pub previous: Option<&'a ActiveGeneration>,
    /// Newly committed generation.
    pub replacement: &'a ActiveGeneration,
    /// Build-first adapter claim.
    pub build: &'a B,
    /// Private backup name that is now safe to remove.
    pub backup_name: &'a str,
}

/// Docker, lifecycle, provisioning, SSH, and forwarding replacement seam.
#[doc(hidden)]
pub trait RebuildEnvironment<P>: Send + Sync {
    /// Build-first claim retained until cleanup.
    type Build: Send + Sync;
    /// Focused mutation/provisioning error.
    type Error: Error + Send + Sync + 'static;

    /// Pulls/builds all replacement images before active resources are stopped.
    fn build<'a>(
        &'a self,
        desired: &'a PreparedRebuildPlan<P>,
        generation: GenerationId,
        no_cache: bool,
    ) -> impl Future<Output = Result<Self::Build, Self::Error>> + Send + 'a;

    /// Cancels old background lifecycle work and stops forwarding before rename.
    fn quiesce<'a>(
        &'a self,
        previous: Option<&'a ActiveGeneration>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Stops and renames the exact old primary to an operation-owned backup.
    fn backup<'a>(
        &'a self,
        previous: &'a ActiveGeneration,
        backup_name: &'a str,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Creates, starts, verifies, runs lifecycle readiness, and reprovisions all assets.
    fn create_ready<'a>(
        &'a self,
        desired: &'a PreparedRebuildPlan<P>,
        build: &'a Self::Build,
        generation: GenerationId,
    ) -> impl Future<Output = Result<ActiveGeneration, Self::Error>> + Send + 'a;

    /// Transactionally hands declared forwarding to the ready replacement.
    fn handoff<'a>(
        &'a self,
        previous: Option<&'a ActiveGeneration>,
        replacement: &'a ActiveGeneration,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Removes candidates and restores the old name/container where feasible.
    fn rollback<'a>(
        &'a self,
        request: RebuildRollbackRequest<'a, Self::Build>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Removes only operation-owned resources and safely selected generated-image history.
    fn cleanup<'a>(
        &'a self,
        request: RebuildCleanupRequest<'a, Self::Build>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;
}

/// Inputs for one locked rebuild transaction.
pub struct RebuildRequest<'a> {
    /// Workspace whose environment is replaced.
    pub workspace: &'a WorkspaceName,
    /// Optional desired configuration selection.
    pub config: Option<&'a RepoRelativeConfigPath>,
    /// Explicit persisted rebuilding operation.
    pub operation: OperationState,
    /// Private, operation-owned old-container backup name.
    pub backup_name: &'a str,
    /// Disable Docker/Compose build caches.
    pub no_cache: bool,
    /// Completion time committed only with complete readiness.
    pub completed_at: StateTimestamp,
}

/// Successful rebuild facts. Cleanup failure does not turn replacement into failure.
#[derive(Debug)]
pub struct RebuildOutcome<E> {
    /// Newly committed generation.
    pub generation: GenerationId,
    /// Independently verified replacement primary ID.
    pub container: ContainerId,
    /// Whether `git status --porcelain` reported any checkout changes.
    pub checkout_has_changes: bool,
    /// Warning-only operation-owned cleanup failure.
    pub cleanup_warning: Option<E>,
}

/// A replacement invariant failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum RebuildInvariantError {
    /// Generation cannot be incremented.
    #[error("active generation cannot be incremented")]
    GenerationOverflow,
    /// Adapter returned a generation other than the requested next generation.
    #[error("replacement returned an unexpected generation")]
    WrongGeneration,
    /// Replacement reused the prior primary container ID.
    #[error("replacement primary container ID did not change")]
    ReusedContainer,
}

/// Primary replacement failure retained with any rollback failure.
#[derive(Debug, Error)]
pub enum ReplacementFailure<E: Error + 'static> {
    /// Adapter phase failed.
    #[error(transparent)]
    Adapter(E),
    /// Adapter output violated a commit invariant.
    #[error(transparent)]
    Invariant(#[from] RebuildInvariantError),
    /// Atomic active-state commit failed after readiness.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
}

/// Layered rebuild transaction failure.
#[derive(Debug, Error)]
pub enum RebuildError<P, F, S, E>
where
    P: Error + 'static,
    F: Error + 'static,
    S: Error + 'static,
    E: Error + 'static,
{
    /// The caller did not supply rebuilding intent or a private backup name.
    #[error("rebuild requires rebuilding intent and a valid private backup name")]
    InvalidOperation,
    /// Workspace lock acquisition failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Existing state or intent persistence failed.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// Desired plan was invalid before mutation.
    #[error("desired rebuild plan is invalid: {source}")]
    Desired {
        /// Focused plan source.
        #[source]
        source: P,
    },
    /// Frozen Feature/dependency validation failed before mutation.
    #[error("rebuild preflight failed: {source}")]
    Preflight {
        /// Focused frozen-input source.
        #[source]
        source: F,
    },
    /// Checkout status could not be determined.
    #[error("cannot inspect checkout changes: {source}")]
    CheckoutStatus {
        /// Focused status adapter source.
        #[source]
        source: S,
    },
    /// The generation counter cannot advance.
    #[error(transparent)]
    Generation(#[from] RebuildInvariantError),
    /// A replacement phase failed; rollback outcome is retained precisely.
    #[error("replacement failed during {phase:?}: {primary}")]
    Replacement {
        /// Last phase reached.
        phase: RebuildPhase,
        /// Original phase/invariant/state failure.
        #[source]
        primary: ReplacementFailure<E>,
        /// Best-effort restoration failure, if restoration was incomplete.
        rollback: Option<E>,
    },
    /// Failure details could not be persisted after restoration.
    #[error("rebuild failed and recovery state could not be persisted: {state}")]
    FailureRecord {
        /// Last phase reached.
        phase: RebuildPhase,
        /// Original phase/invariant failure.
        primary: ReplacementFailure<E>,
        /// Best-effort restoration failure.
        rollback: Option<E>,
        /// State persistence failure.
        #[source]
        state: WorkspaceStateError,
    },
}

/// Rebuilds one environment with build-first mutation and best-effort rollback.
///
/// The complete desired plan and frozen lock are validated before Docker work. The old active
/// record remains authoritative until replacement readiness and forwarding handoff both succeed.
///
/// # Errors
///
/// Returns typed planning, status, state, adapter, invariant, or rollback failures.
#[expect(
    clippy::too_many_lines,
    reason = "the transaction order and rollback edges remain auditable"
)]
pub async fn rebuild_workspace<P, S, E>(
    root: &CdenvRoot,
    request: RebuildRequest<'_>,
    planner: &P,
    status: &S,
    environment: &E,
) -> Result<
    RebuildOutcome<E::Error>,
    RebuildError<P::PlanError, P::PreflightError, S::Error, E::Error>,
>
where
    P: RebuildPlanner,
    S: CheckoutStatus,
    E: RebuildEnvironment<P::Plan>,
{
    if request.operation.kind() != ForegroundOperation::Rebuilding
        || !valid_backup_name(request.backup_name)
    {
        return Err(RebuildError::InvalidOperation);
    }
    let paths = root.workspace(request.workspace);
    let _lock = LockGuard::acquire(&paths.lock_file(), LockMode::Exclusive, LockBehavior::Wait)?;
    let mut state = load_workspace_state(&paths.state_file())?.into_state();
    let desired = planner
        .prepare(
            &paths.checkout(),
            request.config.map(RepoRelativeConfigPath::as_path),
            &state,
        )
        .map_err(|source| RebuildError::Desired { source })?;
    planner
        .validate_frozen(&desired)
        .await
        .map_err(|source| RebuildError::Preflight { source })?;
    let checkout_has_changes = status
        .has_changes(&paths.checkout())
        .await
        .map_err(|source| RebuildError::CheckoutStatus { source })?;
    let previous = state.active().cloned();
    let generation = next_generation(previous.as_ref())?;

    state.update_desired(
        desired.profile.clone(),
        desired.config.clone(),
        desired.fingerprints.clone(),
    );
    state.set_operation(request.operation);
    state.set_last_error(None);
    persist_workspace_state(&paths.state_file(), &state)?;

    let build = match environment
        .build(&desired, generation, request.no_cache)
        .await
    {
        Ok(build) => build,
        Err(source) => {
            return fail_without_rollback(
                &paths.state_file(),
                state,
                RebuildPhase::Built,
                ReplacementFailure::Adapter(source),
            );
        }
    };
    if let Err(source) = environment.quiesce(previous.as_ref()).await {
        return fail_with_rollback(
            &paths.state_file(),
            state,
            environment,
            &build,
            previous.as_ref(),
            None,
            request.backup_name,
            RebuildPhase::Quiescing,
            ReplacementFailure::Adapter(source),
        )
        .await;
    }
    if let Some(old) = previous.as_ref()
        && let Err(source) = environment.backup(old, request.backup_name).await
    {
        return fail_with_rollback(
            &paths.state_file(),
            state,
            environment,
            &build,
            previous.as_ref(),
            None,
            request.backup_name,
            RebuildPhase::BackupRenamed,
            ReplacementFailure::Adapter(source),
        )
        .await;
    }
    let candidate = match environment.create_ready(&desired, &build, generation).await {
        Ok(candidate) => candidate,
        Err(source) => {
            return fail_with_rollback(
                &paths.state_file(),
                state,
                environment,
                &build,
                previous.as_ref(),
                None,
                request.backup_name,
                RebuildPhase::BackupRenamed,
                ReplacementFailure::Adapter(source),
            )
            .await;
        }
    };
    let invariant = if candidate.generation() != generation {
        Some(RebuildInvariantError::WrongGeneration)
    } else if previous
        .as_ref()
        .is_some_and(|old| old.container_id() == candidate.container_id())
    {
        Some(RebuildInvariantError::ReusedContainer)
    } else {
        None
    };
    if let Some(invariant) = invariant {
        return fail_with_rollback(
            &paths.state_file(),
            state,
            environment,
            &build,
            previous.as_ref(),
            Some(&candidate),
            request.backup_name,
            RebuildPhase::CandidateReady,
            ReplacementFailure::Invariant(invariant),
        )
        .await;
    }
    if let Err(source) = environment.handoff(previous.as_ref(), &candidate).await {
        return fail_with_rollback(
            &paths.state_file(),
            state,
            environment,
            &build,
            previous.as_ref(),
            Some(&candidate),
            request.backup_name,
            RebuildPhase::CandidateReady,
            ReplacementFailure::Adapter(source),
        )
        .await;
    }

    let container = candidate.container_id().clone();
    let recovery_state = state.clone();
    state.commit_active(candidate.clone(), request.completed_at);
    if let Err(source) = persist_workspace_state(&paths.state_file(), &state) {
        return fail_with_rollback(
            &paths.state_file(),
            recovery_state,
            environment,
            &build,
            previous.as_ref(),
            Some(&candidate),
            request.backup_name,
            RebuildPhase::ForwardingHandedOff,
            ReplacementFailure::State(source),
        )
        .await;
    }
    let cleanup_warning = environment
        .cleanup(RebuildCleanupRequest {
            previous: previous.as_ref(),
            replacement: &candidate,
            build: &build,
            backup_name: request.backup_name,
        })
        .await
        .err();
    Ok(RebuildOutcome {
        generation,
        container,
        checkout_has_changes,
        cleanup_warning,
    })
}

fn next_generation(
    previous: Option<&ActiveGeneration>,
) -> Result<GenerationId, RebuildInvariantError> {
    let value = previous.map_or(1, |active| active.generation().get().saturating_add(1));
    if value == 0 || previous.is_some_and(|active| value == active.generation().get()) {
        return Err(RebuildInvariantError::GenerationOverflow);
    }
    GenerationId::new(value).map_err(|_| RebuildInvariantError::GenerationOverflow)
}

fn valid_backup_name(value: &str) -> bool {
    value.starts_with("cdenv-backup-")
        && !value.starts_with('-')
        && !value.chars().any(char::is_control)
        && value.len() <= 255
}

fn fail_without_rollback<P, F, S, E, T>(
    state_path: &Path,
    mut state: WorkspaceState,
    phase: RebuildPhase,
    primary: ReplacementFailure<E>,
) -> Result<T, RebuildError<P, F, S, E>>
where
    P: Error + 'static,
    F: Error + 'static,
    S: Error + 'static,
    E: Error + 'static,
{
    state.set_operation(OperationState::idle());
    state.set_last_error(Some(SanitizedSummary::redact(&primary.to_string(), [])));
    if let Err(state_error) = persist_workspace_state(state_path, &state) {
        return Err(RebuildError::FailureRecord {
            phase,
            primary,
            rollback: None,
            state: state_error,
        });
    }
    Err(RebuildError::Replacement {
        phase,
        primary,
        rollback: None,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "rollback receives every owned-resource boundary explicitly"
)]
async fn fail_with_rollback<P, F, S, E, R, Plan, Output>(
    state_path: &Path,
    mut state: WorkspaceState,
    environment: &R,
    build: &R::Build,
    previous: Option<&ActiveGeneration>,
    candidate: Option<&ActiveGeneration>,
    backup_name: &str,
    phase: RebuildPhase,
    primary: ReplacementFailure<E>,
) -> Result<Output, RebuildError<P, F, S, E>>
where
    P: Error + 'static,
    F: Error + 'static,
    S: Error + 'static,
    E: Error + Send + Sync + 'static,
    R: RebuildEnvironment<Plan, Error = E>,
{
    let rollback = environment
        .rollback(RebuildRollbackRequest {
            build,
            generation: next_generation(previous)?,
            previous,
            candidate,
            backup_name,
            phase,
        })
        .await
        .err();
    state.set_operation(if rollback.is_none() {
        OperationState::idle()
    } else {
        state.operation().clone()
    });
    let summary = if rollback.is_none() {
        primary.to_string()
    } else {
        "rebuild failed and rollback was incomplete".to_owned()
    };
    state.set_last_error(Some(SanitizedSummary::redact(&summary, [])));
    if let Err(state_error) = persist_workspace_state(state_path, &state) {
        return Err(RebuildError::FailureRecord {
            phase,
            primary,
            rollback,
            state: state_error,
        });
    }
    Err(RebuildError::Replacement {
        phase,
        primary,
        rollback,
    })
}

/// One image considered by bounded generated-image retention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImageCandidate {
    /// Exact content-addressed image ID.
    pub id: ImageId,
    /// Parsed generation label.
    pub generation: GenerationId,
    /// Whether all cdenv generated/install/workspace/profile labels matched.
    pub workspace_generated: bool,
    /// Whether any container currently references this image.
    pub referenced: bool,
    /// Whether Docker reports any repository tag.
    pub repository_tagged: bool,
}

/// Selects only old, unreferenced, untagged, verified generated workspace images.
///
/// Inputs not proven cdenv-generated are ignored. The newest bounded generation history is always
/// retained, and no base/repository-tagged image can be returned.
#[must_use]
pub fn select_generated_images_for_cleanup(candidates: &[GeneratedImageCandidate]) -> Vec<ImageId> {
    let mut eligible = candidates
        .iter()
        .filter(|image| image.workspace_generated && !image.referenced && !image.repository_tagged)
        .collect::<Vec<_>>();
    eligible.sort_by_key(|image| std::cmp::Reverse(image.generation));
    eligible
        .into_iter()
        .skip(GENERATED_IMAGE_HISTORY)
        .map(|image| image.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(value: char, generation: u64) -> GeneratedImageCandidate {
        GeneratedImageCandidate {
            id: ImageId::parse(&format!("sha256:{}", value.to_string().repeat(64)))
                .expect("image ID"),
            generation: GenerationId::new(generation).expect("generation"),
            workspace_generated: true,
            referenced: false,
            repository_tagged: false,
        }
    }

    #[test]
    fn cleanup_retains_bounded_newest_generated_history() {
        let candidates = [image('a', 1), image('b', 2), image('c', 3), image('d', 4)];
        let selected = select_generated_images_for_cleanup(&candidates);
        assert_eq!(
            selected,
            vec![candidates[1].id.clone(), candidates[0].id.clone()]
        );
    }

    #[test]
    fn cleanup_never_selects_tagged_referenced_or_unverified_images() {
        let mut tagged = image('a', 1);
        tagged.repository_tagged = true;
        let mut referenced = image('b', 2);
        referenced.referenced = true;
        let mut base = image('c', 3);
        base.workspace_generated = false;
        assert!(select_generated_images_for_cleanup(&[tagged, referenced, base]).is_empty());
    }

    #[test]
    fn private_backup_names_are_required() {
        assert!(valid_backup_name("cdenv-backup-operation-47"));
        assert!(!valid_backup_name("workspace"));
    }
}
