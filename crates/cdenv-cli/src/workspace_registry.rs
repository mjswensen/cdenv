//! Atomic workspace-name reservation and read-only local enumeration.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cdenv_core::{ForegroundOperation, WorkspaceName};
use thiserror::Error;

use crate::{
    CdenvRoot, LoadedWorkspaceState, LockBehavior, LockError, LockGuard, LockMode,
    ManagedPathError, ManagedPathKind, ManagedPathState, StorageError, WorkspacePaths,
    WorkspaceStateError, ensure_lock_file, ensure_private_directory, inspect_managed_path,
    load_workspace_state,
};

/// Whether persisted foreground intent corresponds to a held lifecycle lock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersistedOperationStatus {
    /// No foreground operation is persisted.
    Idle,
    /// An incompatible lock holder makes the persisted operation active.
    Active(ForegroundOperation),
    /// Persisted non-idle intent has no lock holder and was interrupted.
    Interrupted(ForegroundOperation),
}

/// Correlates persisted operation intent with the live exclusive lock.
///
/// The function never rewrites interrupted intent.
///
/// # Errors
///
/// Returns [`LockError`] if the existing workspace lock is unsafe or cannot be
/// inspected.
pub fn classify_persisted_operation(
    operation: ForegroundOperation,
    lock_path: &Path,
) -> Result<PersistedOperationStatus, LockError> {
    if operation == ForegroundOperation::Idle {
        return match inspect_managed_path(lock_path, ManagedPathKind::File, current_user_id())? {
            ManagedPathState::Valid => Ok(PersistedOperationStatus::Idle),
            ManagedPathState::Missing => Err(LockError::Missing {
                path: lock_path.to_path_buf(),
            }),
        };
    }
    match LockGuard::acquire(lock_path, LockMode::Exclusive, LockBehavior::FailFast) {
        Ok(guard) => {
            drop(guard);
            Ok(PersistedOperationStatus::Interrupted(operation))
        }
        Err(LockError::Contended { .. }) => Ok(PersistedOperationStatus::Active(operation)),
        Err(error) => Err(error),
    }
}

/// A workspace name that has been atomically reserved.
///
/// Dropping an uncommitted reservation performs bounded rollback of only the
/// known operation-owned empty skeleton. It never recursively removes a tree.
#[derive(Debug)]
pub struct WorkspaceReservation {
    workspace_root: PathBuf,
    owned_paths: Vec<PathBuf>,
    committed: bool,
}

impl WorkspaceReservation {
    /// Returns the reserved workspace root.
    #[must_use]
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Keeps the workspace skeleton after a successful create transaction.
    pub fn commit(mut self) {
        self.committed = true;
    }

    /// Explicitly rolls back and reports a nonempty or changed skeleton.
    ///
    /// # Errors
    ///
    /// Returns [`ReservationError::Rollback`] when an operation-owned object
    /// cannot be removed safely.
    pub fn rollback(mut self) -> Result<(), ReservationError> {
        self.cleanup()?;
        self.committed = true;
        Ok(())
    }

    fn cleanup(&mut self) -> Result<(), ReservationError> {
        for path in self.owned_paths.iter().rev() {
            let result = match fs::symlink_metadata(path) {
                Ok(metadata) if metadata.is_dir() => fs::remove_dir(path),
                Ok(metadata) if metadata.is_file() => fs::remove_file(path),
                Ok(_) => {
                    return Err(ReservationError::Rollback {
                        path: path.clone(),
                        source: io::Error::other("operation-owned path changed file kind"),
                    });
                }
                Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
                Err(source) => Err(source),
            };
            if let Err(source) = result {
                return Err(ReservationError::Rollback {
                    path: path.clone(),
                    source,
                });
            }
        }
        Ok(())
    }
}

impl Drop for WorkspaceReservation {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.cleanup();
        }
    }
}

/// A workspace namespace reservation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ReservationError {
    /// Managed directory setup failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Namespace-lock setup or acquisition failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Existing workspace-path inspection failed.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// The requested name already has a workspace root.
    #[error("workspace name `{name}` is already reserved; choose a different `--name`")]
    AlreadyReserved {
        /// The conflicting name.
        name: WorkspaceName,
    },
    /// Atomic creation of the workspace root failed.
    #[error("cannot reserve workspace root {path:?}: {source}")]
    Create {
        /// The intended workspace root.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// Bounded rollback found a changed or nonempty operation-owned path.
    #[error("cannot roll back operation-owned workspace path {path:?}: {source}")]
    Rollback {
        /// The path retained for safety.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Atomically reserves one workspace name and creates its private skeleton.
///
/// The global lock is held only through name inspection and skeleton creation;
/// it is released before this function returns. The returned reservation owns
/// rollback until the create transaction commits.
///
/// # Errors
///
/// Returns [`ReservationError::AlreadyReserved`] without modifying the winner,
/// or another [`ReservationError`] for unsafe paths and filesystem failures.
pub fn reserve_workspace(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<WorkspaceReservation, ReservationError> {
    ensure_private_directory(root.as_path())?;
    ensure_private_directory(&root.workspaces_dir())?;
    ensure_lock_file(&root.workspace_namespace_lock())?;
    let namespace_guard = LockGuard::acquire(
        &root.workspace_namespace_lock(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )?;

    let paths = root.workspace(name);
    match inspect_managed_path(&paths.root(), ManagedPathKind::Directory, current_user_id())? {
        ManagedPathState::Valid => {
            return Err(ReservationError::AlreadyReserved { name: name.clone() });
        }
        ManagedPathState::Missing => {}
    }

    fs::create_dir(paths.root()).map_err(|source| {
        if source.kind() == io::ErrorKind::AlreadyExists {
            ReservationError::AlreadyReserved { name: name.clone() }
        } else {
            ReservationError::Create {
                path: paths.root(),
                source,
            }
        }
    })?;
    let mut reservation = WorkspaceReservation {
        workspace_root: paths.root(),
        owned_paths: vec![paths.root()],
        committed: false,
    };

    ensure_private_directory(&paths.root())?;
    reservation.owned_paths.push(paths.checkout_dir());
    ensure_private_directory(&paths.checkout_dir())?;
    reservation.owned_paths.push(paths.runtime_dir());
    ensure_private_directory(&paths.runtime_dir())?;
    reservation.owned_paths.push(paths.logs_dir());
    ensure_private_directory(&paths.logs_dir())?;
    reservation.owned_paths.push(paths.lock_file());
    ensure_lock_file(&paths.lock_file())?;

    drop(namespace_guard);
    Ok(reservation)
}

/// Read-only interpretation of one workspace directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceEntryStatus {
    /// Current or migrated state decoded successfully.
    Valid {
        /// State and migration guidance; migration remains in memory.
        loaded: Box<LoadedWorkspaceState>,
        /// Lock-correlated foreground operation status.
        operation: PersistedOperationStatus,
    },
    /// The directory component is not a valid workspace name.
    InvalidName,
    /// A state file is absent.
    MissingState,
    /// State is corrupt or violates validation rules.
    Corrupt {
        /// Concise typed error text.
        message: String,
    },
    /// State belongs to a newer unsupported schema.
    NewerSchema {
        /// Encountered schema.
        found: u32,
        /// Highest supported schema.
        supported: u32,
    },
    /// State belongs to an older schema without an explicit migration.
    UnsupportedOlderSchema {
        /// Encountered schema.
        found: u32,
    },
    /// A managed workspace, state, or lock path is unsafe/unreadable.
    Unsafe {
        /// Concise typed error text.
        message: String,
    },
}

/// One deterministic read-only workspace enumeration item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnumeratedWorkspace {
    name: String,
    status: WorkspaceEntryStatus,
}

impl EnumeratedWorkspace {
    /// Returns the directory name used for sorting and diagnostics.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns read-only state interpretation.
    #[must_use]
    pub const fn status(&self) -> &WorkspaceEntryStatus {
        &self.status
    }
}

/// A global workspace enumeration failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EnumerationError {
    /// The workspace collection path is unsafe.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// Reading the collection directory failed.
    #[error("cannot enumerate workspace directory {path:?}: {source}")]
    ReadDirectory {
        /// The collection path.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// A directory entry cannot be represented as UTF-8.
    #[error("workspace directory contains a non-UTF-8 entry: {path:?}")]
    NonUtf8Entry {
        /// The rejected path.
        path: PathBuf,
    },
}

/// Enumerates local workspace directories deterministically without repair.
///
/// State migration is reported but never persisted. Corrupt/newer state is a
/// per-workspace result so one damaged entry does not hide healthy workspaces.
///
/// # Errors
///
/// Returns [`EnumerationError`] only when the collection itself cannot be
/// securely inspected or read.
pub fn enumerate_workspaces(
    root: &CdenvRoot,
) -> Result<Vec<EnumeratedWorkspace>, EnumerationError> {
    match inspect_managed_path(
        &root.workspaces_dir(),
        ManagedPathKind::Directory,
        current_user_id(),
    )? {
        ManagedPathState::Missing => return Ok(Vec::new()),
        ManagedPathState::Valid => {}
    }

    let entries =
        fs::read_dir(root.workspaces_dir()).map_err(|source| EnumerationError::ReadDirectory {
            path: root.workspaces_dir(),
            source,
        })?;
    let mut workspaces = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| EnumerationError::ReadDirectory {
            path: root.workspaces_dir(),
            source,
        })?;
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| EnumerationError::ReadDirectory {
                path: path.clone(),
                source,
            })?;
        if !metadata.is_dir() && !metadata.file_type().is_symlink() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| EnumerationError::NonUtf8Entry { path: path.clone() })?;
        let status = if metadata.file_type().is_symlink() {
            WorkspaceEntryStatus::Unsafe {
                message: format!(
                    "workspace directory must not be a symbolic link: {}",
                    path.display()
                ),
            }
        } else {
            match inspect_managed_path(&path, ManagedPathKind::Directory, current_user_id()) {
                Ok(ManagedPathState::Valid) => match WorkspaceName::parse(&name) {
                    Ok(workspace_name) => {
                        enumerate_one(root.workspace(&workspace_name), &workspace_name)
                    }
                    Err(_) => WorkspaceEntryStatus::InvalidName,
                },
                Ok(ManagedPathState::Missing) => WorkspaceEntryStatus::Unsafe {
                    message: format!("workspace directory disappeared: {}", path.display()),
                },
                Err(error) => WorkspaceEntryStatus::Unsafe {
                    message: error.to_string(),
                },
            }
        };
        workspaces.push(EnumeratedWorkspace { name, status });
    }
    workspaces.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(workspaces)
}

fn enumerate_one(paths: WorkspacePaths<'_>, expected_name: &WorkspaceName) -> WorkspaceEntryStatus {
    let loaded = match load_workspace_state(&paths.state_file()) {
        Ok(loaded) => loaded,
        Err(WorkspaceStateError::Missing { .. }) => return WorkspaceEntryStatus::MissingState,
        Err(WorkspaceStateError::NewerSchema {
            found, supported, ..
        }) => {
            return WorkspaceEntryStatus::NewerSchema { found, supported };
        }
        Err(WorkspaceStateError::UnsupportedOlderSchema { found, .. }) => {
            return WorkspaceEntryStatus::UnsupportedOlderSchema { found };
        }
        Err(error @ WorkspaceStateError::Corrupt { .. }) => {
            return WorkspaceEntryStatus::Corrupt {
                message: error.to_string(),
            };
        }
        Err(error) => {
            return WorkspaceEntryStatus::Unsafe {
                message: error.to_string(),
            };
        }
    };
    if loaded.state().name() != expected_name {
        return WorkspaceEntryStatus::Corrupt {
            message: format!(
                "workspace state name `{}` does not match directory `{expected_name}`",
                loaded.state().name()
            ),
        };
    }
    let operation =
        match classify_persisted_operation(loaded.state().operation().kind(), &paths.lock_file()) {
            Ok(operation) => operation,
            Err(error) => {
                return WorkspaceEntryStatus::Unsafe {
                    message: error.to_string(),
                };
            }
        };
    WorkspaceEntryStatus::Valid {
        loaded: Box::new(loaded),
        operation,
    }
}

/// Non-mutating inspection of private supervisor runtime objects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SupervisorRuntimeInspection {
    /// Runtime directory state.
    pub directory: ManagedPathState,
    /// Control socket state.
    pub socket: ManagedPathState,
    /// Supervisor identity/state file state.
    pub state_file: ManagedPathState,
    /// Supervisor lifetime-lock state.
    pub lifetime_lock: ManagedPathState,
}

/// Checks future supervisor runtime objects without creating or repairing them.
///
/// # Errors
///
/// Returns [`ManagedPathError`] for symlinks, ownership mismatches, or wrong
/// object kinds.
pub fn inspect_supervisor_runtime(
    paths: WorkspacePaths<'_>,
) -> Result<SupervisorRuntimeInspection, ManagedPathError> {
    Ok(SupervisorRuntimeInspection {
        directory: inspect_managed_path(
            &paths.runtime_dir(),
            ManagedPathKind::Directory,
            current_user_id(),
        )?,
        socket: inspect_managed_path(
            &paths.supervisor_socket(),
            ManagedPathKind::Socket,
            current_user_id(),
        )?,
        state_file: inspect_managed_path(
            &paths.supervisor_state_file(),
            ManagedPathKind::File,
            current_user_id(),
        )?,
        lifetime_lock: inspect_managed_path(
            &paths.supervisor_lifetime_lock(),
            ManagedPathKind::File,
            current_user_id(),
        )?,
    })
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
    use std::sync::{Arc, Barrier};
    use std::time::SystemTime;

    use super::*;
    use crate::RootEnvironment;

    struct NoEnvironment;
    impl RootEnvironment for NoEnvironment {
        fn cdenv_home(&self) -> Option<std::ffi::OsString> {
            None
        }
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn test_root(parent: &Path) -> CdenvRoot {
        CdenvRoot::resolve(Some(&parent.join("root")), &NoEnvironment)
            .expect("test root should validate")
    }

    #[test]
    fn concurrent_name_reservation_has_exactly_one_winner() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = test_root(temporary.path());
        let barrier = Arc::new(Barrier::new(3));
        let mut threads = Vec::new();
        for _ in 0..2 {
            let root = root.clone();
            let barrier = Arc::clone(&barrier);
            threads.push(std::thread::spawn(move || {
                let name = WorkspaceName::parse("project").expect("name should be valid");
                barrier.wait();
                reserve_workspace(&root, &name)
            }));
        }
        barrier.wait();
        let mut winner = None;
        let mut failures = 0;
        for thread in threads {
            match thread.join().expect("reservation thread should not panic") {
                Ok(reservation) => winner = Some(reservation),
                Err(ReservationError::AlreadyReserved { .. }) => failures += 1,
                Err(error) => panic!("unexpected reservation error: {error}"),
            }
        }
        let winner = winner.expect("one reservation should win");
        let marker = winner.workspace_root().join("winner-marker");
        fs::write(&marker, b"winner").expect("winner marker should be written");
        winner.commit();

        assert_eq!(
            (
                failures,
                fs::read(marker).expect("winner marker should remain")
            ),
            (1, b"winner".to_vec())
        );
    }

    #[test]
    fn rollback_never_recursively_deletes_foreign_files() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = test_root(temporary.path());
        let name = WorkspaceName::parse("project").expect("name should be valid");
        let reservation = reserve_workspace(&root, &name).expect("reservation should succeed");
        let foreign = reservation.workspace_root().join("foreign");
        fs::write(&foreign, b"foreign").expect("foreign file should be written");

        let result = reservation.rollback();

        assert!(matches!(result, Err(ReservationError::Rollback { .. })));
        assert_eq!(
            fs::read(foreign).expect("foreign file should remain"),
            b"foreign"
        );
    }

    #[test]
    fn enumeration_is_sorted_read_only_and_reports_problem_states() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = test_root(temporary.path());
        let cases = [
            (
                "project",
                include_bytes!("../tests/fixtures/state-no-active.json").as_slice(),
            ),
            (
                "legacy-project",
                include_bytes!("../tests/fixtures/state-v0.json").as_slice(),
            ),
            ("a-corrupt", b"{broken".as_slice()),
            ("m-newer", br#"{"schemaVersion":99}"#.as_slice()),
        ];
        let mut observations = Vec::new();
        for (name_text, bytes) in cases {
            let name = WorkspaceName::parse(name_text).expect("name should be valid");
            let reservation = reserve_workspace(&root, &name).expect("reservation should succeed");
            let state_path = root.workspace(&name).state_file();
            fs::write(&state_path, bytes).expect("state fixture should be written");
            observations.push((
                state_path.clone(),
                fs::metadata(&state_path)
                    .expect("metadata should exist")
                    .modified()
                    .unwrap_or(SystemTime::UNIX_EPOCH),
                bytes.to_vec(),
            ));
            reservation.commit();
        }

        let entries = enumerate_workspaces(&root).expect("enumeration should succeed");

        let summaries: Vec<_> = entries
            .iter()
            .map(|entry| {
                (
                    entry.name().to_owned(),
                    match entry.status() {
                        WorkspaceEntryStatus::Valid { loaded, .. }
                            if loaded.migration_status()
                                == crate::MigrationStatus::Migrated { from: 0 } =>
                        {
                            "migrated"
                        }
                        WorkspaceEntryStatus::Valid { .. } => "valid",
                        WorkspaceEntryStatus::Corrupt { .. } => "corrupt",
                        WorkspaceEntryStatus::NewerSchema { .. } => "newer",
                        _ => "unexpected",
                    },
                )
            })
            .collect();
        assert_eq!(
            summaries,
            [
                ("a-corrupt".into(), "corrupt"),
                ("legacy-project".into(), "migrated"),
                ("m-newer".into(), "newer"),
                ("project".into(), "valid")
            ]
        );
        for (path, modified, bytes) in observations {
            assert_eq!(
                (
                    fs::metadata(&path)
                        .expect("metadata should remain")
                        .modified()
                        .unwrap_or(SystemTime::UNIX_EPOCH),
                    fs::read(path).expect("state should remain"),
                ),
                (modified, bytes)
            );
        }
    }

    #[test]
    fn held_exclusive_lock_makes_persisted_operation_active() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = temporary.path().join(".lock");
        ensure_lock_file(&path).expect("lock should be prepared");
        let _guard = LockGuard::acquire(&path, LockMode::Exclusive, LockBehavior::Wait)
            .expect("exclusive operation should acquire lock");

        let status = classify_persisted_operation(ForegroundOperation::Starting, &path)
            .expect("classification should succeed");

        assert_eq!(
            status,
            PersistedOperationStatus::Active(ForegroundOperation::Starting)
        );
    }

    #[test]
    fn non_idle_operation_without_lock_holder_is_interrupted_and_not_rewritten() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = test_root(temporary.path());
        let name = WorkspaceName::parse("project").expect("name should be valid");
        let reservation = reserve_workspace(&root, &name).expect("reservation should succeed");
        let mut fixture: serde_json::Value =
            serde_json::from_slice(include_bytes!("../tests/fixtures/state-no-active.json"))
                .expect("fixture should decode");
        fixture["operation"] = serde_json::json!({
            "kind": "starting",
            "id": "operation-1",
            "startedAt": "2025-01-02T03:04:06Z"
        });
        let bytes = serde_json::to_vec_pretty(&fixture).expect("fixture should encode");
        let state_path = root.workspace(&name).state_file();
        fs::write(&state_path, &bytes).expect("state should be written");
        reservation.commit();

        let entries = enumerate_workspaces(&root).expect("enumeration should succeed");

        assert!(matches!(
            entries[0].status(),
            WorkspaceEntryStatus::Valid {
                operation: PersistedOperationStatus::Interrupted(ForegroundOperation::Starting),
                ..
            }
        ));
        assert_eq!(
            fs::read(state_path).expect("state should remain readable"),
            bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn supervisor_runtime_inspection_accepts_private_socket_and_files() {
        use std::os::unix::net::UnixListener;
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let root = test_root(temporary.path());
        let name = WorkspaceName::parse("project").expect("name should be valid");
        let reservation = reserve_workspace(&root, &name).expect("reservation should succeed");
        let paths = root.workspace(&name);
        let _listener = UnixListener::bind(paths.supervisor_socket())
            .expect("supervisor socket should be bound");
        fs::write(paths.supervisor_state_file(), b"state").expect("state should be written");
        ensure_lock_file(&paths.supervisor_lifetime_lock()).expect("lifetime lock should exist");

        let inspection = inspect_supervisor_runtime(paths).expect("runtime should be valid");

        assert_eq!(
            inspection,
            SupervisorRuntimeInspection {
                directory: ManagedPathState::Valid,
                socket: ManagedPathState::Valid,
                state_file: ManagedPathState::Valid,
                lifetime_lock: ManagedPathState::Valid,
            }
        );
        reservation.commit();
    }
}
