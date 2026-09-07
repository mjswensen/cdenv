//! Durable checkout creation before environment orchestration is available.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use cdenv_core::{ForegroundOperation, ProfileId, WorkspaceName, derive_workspace_name};
use thiserror::Error;
use url::Url;

use crate::git::{OperationLog, OperationLogError};
use crate::{
    CancellationToken, CdenvRoot, DesiredConfigPath, DesiredConfigPathError, FingerprintKeyState,
    GitAdapter, GitError, GitVersion, Installation, InstallationError, LockBehavior, LockError,
    LockGuard, LockMode, ManagedPathError, ManagedPathKind, ManagedPathState, OperationState,
    OperationStateError, PlanFingerprintCategory, PlanFingerprints, RepoRelativeConfigPath,
    ReservationError, SanitizedRepositorySource, SanitizedSummary, StateTimestamp, StorageError,
    WorkspaceReservation, WorkspaceState, WorkspaceStateError, ensure_private_directory,
    inspect_managed_path, persist_workspace_state, reserve_workspace,
};

const PROFILE_ID: &str = "cdenv-devcontainer-v1";
const DEFAULT_CONFIG: &str = ".devcontainer/devcontainer.json";
const MAXIMUM_LOG_BYTES: usize = 1024 * 1024;
const RETAINED_CREATE_LOGS: usize = 20;
const PENDING_PLAN_DOMAIN: &[u8] = b"create-checkout-only-v1";

/// Inputs to the checkout-only create transaction.
#[derive(Clone, Copy, Debug)]
pub struct CreateWorkspaceRequest<'a> {
    /// Original source forwarded to Git only as an argument after its option delimiter.
    pub source: &'a str,
    /// Explicit name, or `None` to derive it from the source.
    pub name: Option<&'a WorkspaceName>,
    /// Explicit repository-relative configuration selection.
    pub config: Option<&'a RepoRelativeConfigPath>,
}

/// Result of a checkout-only create transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWorkspace {
    name: WorkspaceName,
    checkout: PathBuf,
    operation_log: PathBuf,
    git_version: GitVersion,
}

impl CreatedWorkspace {
    /// Returns the selected workspace name.
    #[must_use]
    pub const fn name(&self) -> &WorkspaceName {
        &self.name
    }

    /// Returns the exact checkout directory.
    #[must_use]
    pub fn checkout(&self) -> &Path {
        &self.checkout
    }

    /// Returns the bounded private global operation log.
    #[must_use]
    pub fn operation_log(&self) -> &Path {
        &self.operation_log
    }

    /// Returns the command-scoped detected Git version.
    #[must_use]
    pub const fn git_version(&self) -> &GitVersion {
        &self.git_version
    }
}

/// A selected configuration that is unsafe or unavailable in a checkout.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigContainmentError {
    /// The supplied path cannot be represented as UTF-8 state.
    #[error("configuration path must be valid UTF-8")]
    NonUtf8,
    /// Lexical repository-relative validation failed.
    #[error(transparent)]
    Lexical(#[from] DesiredConfigPathError),
    /// The checkout cannot be canonicalized.
    #[error("cannot canonicalize checkout {path:?}: {source}")]
    Checkout {
        /// Checkout path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The selection is absent.
    #[error("selected configuration does not exist: {path:?}")]
    Missing {
        /// Selected path.
        path: PathBuf,
    },
    /// The selection cannot be inspected or canonicalized.
    #[error("cannot inspect selected configuration {path:?}: {source}")]
    Inspect {
        /// Selected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The selection does not resolve to a regular file.
    #[error("selected configuration is not a regular file: {path:?}")]
    NotRegular {
        /// Selected path.
        path: PathBuf,
    },
    /// Canonical resolution escaped the checkout.
    #[error("selected configuration escapes the repository checkout: {path:?}")]
    EscapesCheckout {
        /// Selected path.
        path: PathBuf,
    },
}

/// Validates and canonicalizes an explicit configuration inside a checkout.
///
/// # Errors
///
/// Returns [`ConfigContainmentError`] for non-UTF-8, absolute, traversing,
/// missing, non-regular, or symlink-escaping selections.
pub fn validate_explicit_config(
    checkout: &Path,
    selection: &Path,
) -> Result<DesiredConfigPath, ConfigContainmentError> {
    let text = selection.to_str().ok_or(ConfigContainmentError::NonUtf8)?;
    DesiredConfigPath::parse(text)?;
    let canonical_checkout =
        fs::canonicalize(checkout).map_err(|source| ConfigContainmentError::Checkout {
            path: checkout.to_path_buf(),
            source,
        })?;
    let selected = checkout.join(selection);
    let metadata = match fs::symlink_metadata(&selected) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Err(ConfigContainmentError::Missing { path: selected });
        }
        Err(source) => {
            return Err(ConfigContainmentError::Inspect {
                path: selected,
                source,
            });
        }
    };
    if !metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
        return Err(ConfigContainmentError::NotRegular { path: selected });
    }
    let canonical_selected =
        fs::canonicalize(&selected).map_err(|source| ConfigContainmentError::Inspect {
            path: selected.clone(),
            source,
        })?;
    if !canonical_selected.starts_with(&canonical_checkout) {
        return Err(ConfigContainmentError::EscapesCheckout { path: selected });
    }
    if !fs::metadata(&canonical_selected)
        .map_err(|source| ConfigContainmentError::Inspect {
            path: canonical_selected.clone(),
            source,
        })?
        .is_file()
    {
        return Err(ConfigContainmentError::NotRegular {
            path: canonical_selected,
        });
    }
    let relative = canonical_selected
        .strip_prefix(&canonical_checkout)
        .map_err(|_| ConfigContainmentError::EscapesCheckout {
            path: canonical_selected.clone(),
        })?;
    let relative = relative.to_str().ok_or(ConfigContainmentError::NonUtf8)?;
    DesiredConfigPath::parse(relative).map_err(ConfigContainmentError::from)
}

/// A durable create-transaction failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum CreateWorkspaceError {
    /// Installation identity setup failed before reservation.
    #[error(transparent)]
    Installation(#[from] InstallationError),
    /// Staged host permission could not be bound; the successful checkout is retained.
    #[error(transparent)]
    Credentials(#[from] crate::CredentialCommandError),
    /// Managed directory or log setup failed before reservation.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The Git dependency or clone failed.
    #[error(transparent)]
    Git(#[from] GitError),
    /// Name derivation failed without retaining the raw source.
    #[error(transparent)]
    Name(#[from] cdenv_core::WorkspaceNameSelectionError),
    /// Atomic workspace reservation failed.
    #[error(transparent)]
    Reservation(#[from] ReservationError),
    /// Workspace lifecycle locking failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// State construction had invalid operation details.
    #[error(transparent)]
    Operation(#[from] OperationStateError),
    /// State persistence failed.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// Configuration selection validation failed.
    #[error(transparent)]
    Config(#[from] ConfigContainmentError),
    /// Managed checkout inspection failed.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// A local source could not be canonicalized safely for persistence.
    #[error("cannot canonicalize local Git source: {source}")]
    LocalSource {
        /// Filesystem failure; the raw source path is intentionally omitted.
        #[source]
        source: io::Error,
    },
    /// A canonical local source is not UTF-8.
    #[error("canonical local Git source is not valid UTF-8")]
    NonUtf8LocalSource,
    /// The installed fingerprint key cannot create opaque pending fingerprints.
    #[error("installation fingerprint key is unavailable: {reason:?}")]
    FingerprintKeyUnavailable {
        /// Checked key state.
        reason: crate::FingerprintKeyUnknownReason,
    },
    /// A compile-time profile identifier violated its domain invariant.
    #[error("built-in Dev Container profile identifier is invalid: {source}")]
    Profile {
        /// Identity validation failure.
        #[source]
        source: cdenv_core::ProfileIdError,
    },
    /// The application clock could not produce a supported timestamp.
    #[error("cannot create operation timestamp: {0}")]
    Clock(String),
    /// Cryptographic operation-ID generation failed.
    #[error("cannot generate create operation ID: {source}")]
    Random {
        /// Random-source failure.
        #[source]
        source: getrandom::Error,
    },
    /// Git reported success without producing the requested checkout directory.
    #[error("Git clone completed without a managed checkout at {path:?}")]
    MissingCheckout {
        /// Expected checkout.
        path: PathBuf,
    },
    /// Cancellation arrived after Git committed the checkout.
    #[error("create was cancelled after clone; the checkout was retained for recovery")]
    CancelledAfterClone,
    /// Initial-state rollback could not remove only operation-owned paths.
    #[error("initial state persistence failed and workspace rollback also failed: {cleanup}")]
    InitialStateRollback {
        /// Original state persistence failure.
        #[source]
        state: Box<WorkspaceStateError>,
        /// Cleanup failure without raw source material.
        cleanup: String,
    },
    /// Clone rollback could not remove only operation-owned paths.
    #[error("Git clone failed and incomplete workspace rollback also failed: {cleanup}")]
    CloneRollback {
        /// Original safe Git failure.
        #[source]
        git: Box<GitError>,
        /// Cleanup failure without raw source material.
        cleanup: String,
    },
    /// A post-clone failure was retained but recording its summary also failed.
    #[error(
        "post-clone failure was retained, but recoverable state could not be updated: {source}"
    )]
    RecordFailure {
        /// Persistence failure.
        #[source]
        source: WorkspaceStateError,
    },
    /// Global create-log retention failed.
    #[error("cannot prune managed create operation log {path:?}: {source}")]
    LogRetention {
        /// Managed log path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Restricted operation-log creation failed.
    #[error(transparent)]
    OperationLog(#[from] OperationLogError),
}

/// Creates a checkout and recoverable workspace state without starting Docker.
///
/// A failing/cancelled clone removes only the transaction-owned incomplete
/// workspace. Once Git succeeds, every later failure retains checkout and state.
///
/// # Errors
///
/// Returns [`CreateWorkspaceError`] for dependency, reservation, clone,
/// containment, persistence, cancellation, or cleanup failures.
pub fn create_workspace(
    root: &CdenvRoot,
    request: CreateWorkspaceRequest<'_>,
    git: &GitAdapter,
    cancellation: &CancellationToken,
) -> Result<CreatedWorkspace, CreateWorkspaceError> {
    let prepared = prepare_create(root, request, git, cancellation)?;
    let reservation = reserve_workspace(root, &prepared.name)?;
    let paths = root.workspace(&prepared.name);
    let lock = LockGuard::acquire(&paths.lock_file(), LockMode::Exclusive, LockBehavior::Wait)?;
    let mut state = WorkspaceState::new(
        prepared.installation.record().installation_id().clone(),
        prepared.name.clone(),
        prepared.sanitized_source,
        prepared.profile.clone(),
        prepared.desired_config,
        prepared.fingerprints,
        prepared.timestamp,
    );
    state.set_operation(prepared.operation);
    if let Err(state_error) = persist_workspace_state(&paths.state_file(), &state) {
        drop(lock);
        return rollback_initial_state_failure(&paths.state_file(), reservation, state_error);
    }

    if let Err(git_error) = git.clone_repository(
        request.source,
        &paths.checkout(),
        cancellation,
        &prepared.operation_log,
    ) {
        drop(lock);
        return rollback_clone_failure(
            &paths.checkout(),
            &paths.state_file(),
            reservation,
            git_error,
        );
    }

    match inspect_managed_path(
        &paths.checkout(),
        ManagedPathKind::Directory,
        current_user_id(),
    ) {
        Ok(ManagedPathState::Missing) => {
            reservation.commit();
            return record_post_clone_failure(
                &paths.state_file(),
                &mut state,
                CreateWorkspaceError::MissingCheckout {
                    path: paths.checkout(),
                },
            );
        }
        Ok(ManagedPathState::Valid) => reservation.commit(),
        Err(error) => {
            reservation.commit();
            return record_post_clone_failure(
                &paths.state_file(),
                &mut state,
                CreateWorkspaceError::ManagedPath(error),
            );
        }
    }

    bind_created_permissions(root, &mut state, request.name.is_some())?;

    if cancellation.is_cancelled() {
        return record_post_clone_failure(
            &paths.state_file(),
            &mut state,
            CreateWorkspaceError::CancelledAfterClone,
        );
    }

    if let Some(config) = request.config {
        let validated = match validate_explicit_config(&paths.checkout(), config.as_path()) {
            Ok(validated) => validated,
            Err(error) => {
                return record_post_clone_failure(
                    &paths.state_file(),
                    &mut state,
                    CreateWorkspaceError::Config(error),
                );
            }
        };
        let fingerprints = pending_fingerprints(&prepared.installation, &validated)?;
        state.update_desired(prepared.profile, validated, fingerprints);
    }
    let mut completed_state = state.clone();
    completed_state.set_operation(OperationState::idle());
    completed_state.set_last_error(None);
    if let Err(error) = persist_workspace_state(&paths.state_file(), &completed_state) {
        return record_post_clone_failure(
            &paths.state_file(),
            &mut state,
            CreateWorkspaceError::State(error),
        );
    }
    drop(lock);
    let checkout = paths.checkout();

    Ok(CreatedWorkspace {
        name: prepared.name,
        checkout,
        operation_log: prepared.operation_log_path,
        git_version: prepared.git_version,
    })
}

fn bind_created_permissions(
    root: &CdenvRoot,
    state: &mut WorkspaceState,
    explicit_name: bool,
) -> Result<(), CreateWorkspaceError> {
    if let Err(error) = crate::credentials::bind_created_workspace(root, state, explicit_name) {
        return record_post_clone_failure(
            &root.workspace(state.name()).state_file(),
            state,
            CreateWorkspaceError::Credentials(error),
        );
    }
    Ok(())
}

struct PreparedCreate {
    installation: Installation,
    name: WorkspaceName,
    sanitized_source: SanitizedRepositorySource,
    timestamp: StateTimestamp,
    profile: ProfileId,
    desired_config: DesiredConfigPath,
    fingerprints: PlanFingerprints,
    operation: OperationState,
    operation_log_path: PathBuf,
    operation_log: OperationLog,
    git_version: GitVersion,
}

fn prepare_create(
    root: &CdenvRoot,
    request: CreateWorkspaceRequest<'_>,
    git: &GitAdapter,
    cancellation: &CancellationToken,
) -> Result<PreparedCreate, CreateWorkspaceError> {
    let installation = Installation::open_or_create(root)?;
    ensure_private_directory(&root.logs_dir())?;
    prune_create_logs(&root.logs_dir())?;
    let operation_id = random_operation_id()?;
    let operation_log_path = root.logs_dir().join(format!("create-{operation_id}.log"));
    let operation_log = OperationLog::create(&operation_log_path, MAXIMUM_LOG_BYTES)?;
    let git_version = git.detect(cancellation, &operation_log)?;
    let name = request
        .name
        .cloned()
        .map_or_else(|| derive_workspace_name(request.source), Ok)?;
    let sanitized_source = sanitize_clone_source(request.source)?;
    let timestamp = current_timestamp()?;
    let profile =
        ProfileId::parse(PROFILE_ID).map_err(|source| CreateWorkspaceError::Profile { source })?;
    let lexical_config = match request.config {
        Some(config) => config
            .as_path()
            .to_str()
            .ok_or(ConfigContainmentError::NonUtf8)?,
        None => DEFAULT_CONFIG,
    };
    let desired_config =
        DesiredConfigPath::parse(lexical_config).map_err(ConfigContainmentError::from)?;
    let fingerprints = pending_fingerprints(&installation, &desired_config)?;
    let operation = OperationState::active(
        ForegroundOperation::Creating,
        operation_id,
        timestamp.clone(),
    )?;
    Ok(PreparedCreate {
        installation,
        name,
        sanitized_source,
        timestamp,
        profile,
        desired_config,
        fingerprints,
        operation,
        operation_log_path,
        operation_log,
        git_version,
    })
}

fn pending_fingerprints(
    installation: &Installation,
    config: &DesiredConfigPath,
) -> Result<PlanFingerprints, CreateWorkspaceError> {
    let key = match installation.fingerprint_key() {
        FingerprintKeyState::Available(key) => key,
        FingerprintKeyState::Unknown(reason) => {
            return Err(CreateWorkspaceError::FingerprintKeyUnavailable { reason: *reason });
        }
    };
    let inputs = [PENDING_PLAN_DOMAIN, config.as_str().as_bytes()];
    let digest = |category| key.digest_plan(category, inputs);
    Ok(PlanFingerprints::new(
        digest(PlanFingerprintCategory::Build),
        digest(PlanFingerprintCategory::Create),
        digest(PlanFingerprintCategory::Runtime),
    ))
}

fn record_post_clone_failure<T>(
    state_path: &Path,
    state: &mut WorkspaceState,
    error: CreateWorkspaceError,
) -> Result<T, CreateWorkspaceError> {
    state.set_last_error(Some(SanitizedSummary::redact(&error.to_string(), [])));
    persist_workspace_state(state_path, state)
        .map_err(|source| CreateWorkspaceError::RecordFailure { source })?;
    Err(error)
}

fn rollback_initial_state_failure<T>(
    state_path: &Path,
    reservation: WorkspaceReservation,
    state: WorkspaceStateError,
) -> Result<T, CreateWorkspaceError> {
    let cleanup = remove_regular_if_present(state_path)
        .map_err(|error| error.to_string())
        .and_then(|()| reservation.rollback().map_err(|error| error.to_string()));
    match cleanup {
        Ok(()) => Err(CreateWorkspaceError::State(state)),
        Err(cleanup) => Err(CreateWorkspaceError::InitialStateRollback {
            state: Box::new(state),
            cleanup,
        }),
    }
}

fn rollback_clone_failure<T>(
    checkout: &Path,
    state_path: &Path,
    reservation: WorkspaceReservation,
    git: GitError,
) -> Result<T, CreateWorkspaceError> {
    let cleanup = remove_incomplete_checkout(checkout)
        .and_then(|()| remove_regular_if_present(state_path))
        .map_err(|error| error.to_string())
        .and_then(|()| reservation.rollback().map_err(|error| error.to_string()));
    match cleanup {
        Ok(()) => Err(CreateWorkspaceError::Git(git)),
        Err(cleanup) => Err(CreateWorkspaceError::CloneRollback {
            git: Box::new(git),
            cleanup,
        }),
    }
}

fn remove_incomplete_checkout(checkout: &Path) -> io::Result<()> {
    match fs::symlink_metadata(checkout) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source),
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(checkout),
        Ok(_) => Err(io::Error::other(
            "operation-owned checkout changed to an unsafe file kind",
        )),
    }
}

fn remove_regular_if_present(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(source),
        Ok(metadata) if metadata.is_file() => fs::remove_file(path),
        Ok(_) => Err(io::Error::other(
            "operation-owned state changed to an unsafe file kind",
        )),
    }
}

fn sanitize_clone_source(source: &str) -> Result<SanitizedRepositorySource, CreateWorkspaceError> {
    if let Ok(url) = Url::parse(source) {
        if url.scheme() == "file" {
            let path = url
                .to_file_path()
                .map_err(|()| CreateWorkspaceError::LocalSource {
                    source: io::Error::new(io::ErrorKind::InvalidInput, "invalid file URL"),
                })?;
            return canonical_local_source(&path);
        }
        if matches!(url.scheme(), "http" | "https" | "ssh") {
            return Ok(SanitizedRepositorySource::sanitize(source));
        }
    }
    if is_scp_like(source) {
        return Ok(SanitizedRepositorySource::sanitize(source));
    }
    canonical_local_source(Path::new(source))
}

fn canonical_local_source(path: &Path) -> Result<SanitizedRepositorySource, CreateWorkspaceError> {
    let canonical =
        fs::canonicalize(path).map_err(|source| CreateWorkspaceError::LocalSource { source })?;
    let text = canonical
        .to_str()
        .ok_or(CreateWorkspaceError::NonUtf8LocalSource)?;
    Ok(SanitizedRepositorySource::sanitize(text))
}

fn is_scp_like(source: &str) -> bool {
    let Some((prefix, path)) = source.split_once(':') else {
        return false;
    };
    !prefix.is_empty()
        && !path.is_empty()
        && !prefix.contains(['/', '\\'])
        && !prefix.chars().any(char::is_whitespace)
}

fn random_operation_id() -> Result<String, CreateWorkspaceError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|source| CreateWorkspaceError::Random { source })?;
    Ok(hex::encode(bytes))
}

fn current_timestamp() -> Result<StateTimestamp, CreateWorkspaceError> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|source| CreateWorkspaceError::Clock(source.to_string()))?
        .as_secs();
    let seconds =
        i64::try_from(seconds).map_err(|source| CreateWorkspaceError::Clock(source.to_string()))?;
    StateTimestamp::parse(&format_unix_timestamp(seconds))
        .map_err(|source| CreateWorkspaceError::Clock(source.to_string()))
}

fn format_unix_timestamp(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let seconds_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_date_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = seconds_of_day % 3_600 / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
}

fn civil_date_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let shifted = days_since_epoch + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn prune_create_logs(directory: &Path) -> Result<(), CreateWorkspaceError> {
    let mut logs = Vec::new();
    for entry in fs::read_dir(directory).map_err(|source| CreateWorkspaceError::LogRetention {
        path: directory.to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| CreateWorkspaceError::LogRetention {
            path: directory.to_path_buf(),
            source,
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("create-") || !name.ends_with(".log") {
            continue;
        }
        let metadata = fs::symlink_metadata(entry.path()).map_err(|source| {
            CreateWorkspaceError::LogRetention {
                path: entry.path(),
                source,
            }
        })?;
        if metadata.file_type().is_file() {
            inspect_managed_path(&entry.path(), ManagedPathKind::File, current_user_id())?;
            logs.push((metadata.modified().ok(), entry.path()));
        }
    }
    logs.sort();
    let remove_count = logs.len().saturating_sub(RETAINED_CREATE_LOGS - 1);
    for (_, path) in logs.into_iter().take(remove_count) {
        fs::remove_file(&path)
            .map_err(|source| CreateWorkspaceError::LogRetention { path, source })?;
    }
    Ok(())
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the cross-platform ownership API uses None on non-Unix hosts"
)]
fn current_user_id() -> Option<u32> {
    Some(nix::unistd::geteuid().as_raw())
}

#[cfg(not(unix))]
const fn current_user_id() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_formats_as_rfc3339_utc() {
        assert_eq!(format_unix_timestamp(0), "1970-01-01T00:00:00Z");
    }

    #[test]
    fn leap_day_formats_as_rfc3339_utc() {
        assert_eq!(format_unix_timestamp(1_582_934_400), "2020-02-29T00:00:00Z");
    }
}
