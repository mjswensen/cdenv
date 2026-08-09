//! Standard-library file locks for workspace lifecycle coordination.

use std::error::Error;
use std::fmt;
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    ManagedMode, ManagedPathError, ManagedPathKind, ManagedPathState, inspect_managed_path,
    tighten_managed_file,
};

/// The compatibility requested from a managed lock file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockMode {
    /// Multiple readers may coordinate concurrently.
    Shared,
    /// One lifecycle mutation excludes all other lock holders.
    Exclusive,
}

/// Whether acquisition waits for another process or reports contention.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LockBehavior {
    /// Wait until the lock becomes available.
    Wait,
    /// Return immediately for proxy/editor diagnostics.
    FailFast,
}

/// A managed lock-file operation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum LockError {
    /// Managed path inspection rejected the lock or its parent.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// Managed storage could not tighten a newly created lock file.
    #[error("cannot prepare managed lock file {path:?}: {source}")]
    Prepare {
        /// The lock path.
        path: PathBuf,
        /// The managed storage failure.
        #[source]
        source: crate::StorageError,
    },
    /// The lock file is absent.
    #[error("managed lock file does not exist: {path:?}")]
    Missing {
        /// The absent path.
        path: PathBuf,
    },
    /// Opening or creating the lock file failed.
    #[error("cannot open managed lock file {path:?}: {source}")]
    Open {
        /// The lock path.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// Another process holds an incompatible lock.
    #[error("managed lock is held by another operation: {path:?}")]
    Contended {
        /// The contended path.
        path: PathBuf,
    },
    /// Acquiring a lock failed for a reason other than contention.
    #[error("cannot acquire {mode:?} lock {path:?}: {source}")]
    Acquire {
        /// The lock path.
        path: PathBuf,
        /// Requested compatibility.
        mode: LockMode,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// Explicit lock release failed.
    #[error("cannot release managed lock {path:?}: {source}")]
    Release {
        /// The lock path.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Creates or validates a private empty lock file for a mutating setup flow.
///
/// Existing symlinks, non-regular files, and ownership mismatches are refused.
///
/// # Errors
///
/// Returns [`LockError`] for unsafe paths or filesystem failures.
pub fn ensure_lock_file(path: &Path) -> Result<(), LockError> {
    let parent = path.parent().ok_or_else(|| LockError::Open {
        path: path.to_path_buf(),
        source: io::Error::new(io::ErrorKind::InvalidInput, "lock path has no parent"),
    })?;
    inspect_managed_path(parent, ManagedPathKind::Directory, current_user_id())?;

    loop {
        match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
            ManagedPathState::Valid => {
                return tighten_managed_file(path, ManagedMode::PrivateFile).map_err(|source| {
                    LockError::Prepare {
                        path: path.to_path_buf(),
                        source,
                    }
                });
            }
            ManagedPathState::Missing => {}
        }

        let mut options = OpenOptions::new();
        options.read(true).write(true).create_new(true);
        configure_private_creation(&mut options);
        match options.open(path) {
            Ok(file) => {
                drop(file);
                return tighten_managed_file(path, ManagedMode::PrivateFile).map_err(|source| {
                    LockError::Prepare {
                        path: path.to_path_buf(),
                        source,
                    }
                });
            }
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(LockError::Open {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    }
}

/// An RAII-held standard-library file lock.
///
/// Dropping the guard releases the lock. Proxy/SSH setup code should let this
/// guard leave scope immediately after successful transport attachment and
/// before beginning a long-lived stream.
pub struct LockGuard {
    file: Option<File>,
    path: PathBuf,
    mode: LockMode,
}

impl fmt::Debug for LockGuard {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("LockGuard")
            .field("path", &self.path)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

impl LockGuard {
    /// Acquires an existing managed lock file.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Contended`] for incompatible fail-fast acquisition,
    /// or another [`LockError`] for unsafe paths and operating-system failures.
    pub fn acquire(path: &Path, mode: LockMode, behavior: LockBehavior) -> Result<Self, LockError> {
        match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
            ManagedPathState::Missing => {
                return Err(LockError::Missing {
                    path: path.to_path_buf(),
                });
            }
            ManagedPathState::Valid => {}
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .map_err(|source| LockError::Open {
                path: path.to_path_buf(),
                source,
            })?;
        match behavior {
            LockBehavior::Wait => {
                let result = match mode {
                    LockMode::Shared => file.lock_shared(),
                    LockMode::Exclusive => file.lock(),
                };
                result.map_err(|source| LockError::Acquire {
                    path: path.to_path_buf(),
                    mode,
                    source,
                })?;
            }
            LockBehavior::FailFast => {
                let result = match mode {
                    LockMode::Shared => file.try_lock_shared(),
                    LockMode::Exclusive => file.try_lock(),
                };
                match result {
                    Ok(()) => {}
                    Err(TryLockError::WouldBlock) => {
                        return Err(LockError::Contended {
                            path: path.to_path_buf(),
                        });
                    }
                    Err(TryLockError::Error(source)) => {
                        return Err(LockError::Acquire {
                            path: path.to_path_buf(),
                            mode,
                            source,
                        });
                    }
                }
            }
        }
        Ok(Self {
            file: Some(file),
            path: path.to_path_buf(),
            mode,
        })
    }

    /// Explicitly releases the guard and reports an unlock failure.
    ///
    /// Ordinary lexical use should rely on `Drop`; this method is useful when
    /// setup must prove release before entering a transport stream.
    ///
    /// # Errors
    ///
    /// Returns [`LockError::Release`] if the operating system rejects unlock.
    pub fn release(mut self) -> Result<(), LockError> {
        let Some(file) = self.file.take() else {
            return Ok(());
        };
        file.unlock().map_err(|source| LockError::Release {
            path: self.path.clone(),
            source,
        })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = file.unlock();
        }
    }
}

/// A failure during shared-lock attach setup.
#[derive(Debug, Error)]
pub enum AttachSetupError<E>
where
    E: Error + 'static,
{
    /// Shared lifecycle-lock acquisition failed.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// State/live inspection or transport attachment failed.
    #[error("transport attach setup failed: {0}")]
    Setup(#[source] E),
}

/// Runs transport attach setup under a shared lock, releasing before return.
///
/// A returned stream/session value therefore never owns the lifecycle lock.
///
/// # Errors
///
/// Returns [`AttachSetupError`] for lock or setup failures.
pub fn with_shared_lock_for_attach<T, E>(
    path: &Path,
    behavior: LockBehavior,
    setup: impl FnOnce() -> Result<T, E>,
) -> Result<T, AttachSetupError<E>>
where
    E: Error + 'static,
{
    let guard = LockGuard::acquire(path, LockMode::Shared, behavior)?;
    let attached = setup().map_err(AttachSetupError::Setup)?;
    guard.release()?;
    Ok(attached)
}

#[cfg(unix)]
fn configure_private_creation(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
fn configure_private_creation(_options: &mut OpenOptions) {}

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
    use std::convert::Infallible;
    use std::fs;
    use std::process::{Child, Command};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    const HELPER_PATH: &str = "CDENV_TEST_LOCK_PATH";
    const HELPER_MODE: &str = "CDENV_TEST_LOCK_MODE";
    const HELPER_READY: &str = "CDENV_TEST_LOCK_READY";
    const HELPER_HOLD_MS: &str = "CDENV_TEST_LOCK_HOLD_MS";

    fn prepared_lock(directory: &Path) -> PathBuf {
        let path = directory.join(".lock");
        ensure_lock_file(&path).expect("test lock should be prepared");
        path
    }

    fn spawn_helper(path: &Path, mode: LockMode, ready: &Path, hold_ms: u64) -> Child {
        Command::new(std::env::current_exe().expect("test executable should resolve"))
            .arg("--exact")
            .arg("locking::tests::process_lock_helper")
            .arg("--nocapture")
            .env(HELPER_PATH, path)
            .env(
                HELPER_MODE,
                match mode {
                    LockMode::Shared => "shared",
                    LockMode::Exclusive => "exclusive",
                },
            )
            .env(HELPER_READY, ready)
            .env(HELPER_HOLD_MS, hold_ms.to_string())
            .spawn()
            .expect("lock helper should spawn")
    }

    fn wait_ready(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "lock helper did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn process_lock_helper() {
        let Some(path) = std::env::var_os(HELPER_PATH) else {
            return;
        };
        let mode = if std::env::var(HELPER_MODE).as_deref() == Ok("shared") {
            LockMode::Shared
        } else {
            LockMode::Exclusive
        };
        let ready = PathBuf::from(
            std::env::var_os(HELPER_READY).expect("helper ready path should be supplied"),
        );
        let hold_ms = std::env::var(HELPER_HOLD_MS)
            .expect("helper hold duration should be supplied")
            .parse::<u64>()
            .expect("helper hold duration should be numeric");
        let _guard = LockGuard::acquire(Path::new(&path), mode, LockBehavior::Wait)
            .expect("helper should acquire lock");
        fs::write(ready, b"ready").expect("helper should signal readiness");
        thread::sleep(Duration::from_millis(hold_ms));
    }

    #[test]
    fn exclusive_process_lock_reports_fail_fast_contention() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let ready = temporary.path().join("ready");
        let mut child = spawn_helper(&path, LockMode::Exclusive, &ready, 500);
        wait_ready(&ready);

        let result = LockGuard::acquire(&path, LockMode::Exclusive, LockBehavior::FailFast);

        assert!(matches!(result, Err(LockError::Contended { .. })));
        assert!(child.wait().expect("helper should exit").success());
    }

    #[test]
    fn fail_fast_attach_reports_process_lock_contention() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let ready = temporary.path().join("ready");
        let mut child = spawn_helper(&path, LockMode::Exclusive, &ready, 300);
        wait_ready(&ready);

        let result =
            with_shared_lock_for_attach(&path, LockBehavior::FailFast, || Ok::<_, Infallible>(()));

        assert!(matches!(
            result,
            Err(AttachSetupError::Lock(LockError::Contended { .. }))
        ));
        assert!(child.wait().expect("helper should exit").success());
    }

    #[test]
    fn shared_process_locks_can_coexist() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let ready = temporary.path().join("ready");
        let mut child = spawn_helper(&path, LockMode::Shared, &ready, 300);
        wait_ready(&ready);

        let guard = LockGuard::acquire(&path, LockMode::Shared, LockBehavior::FailFast);

        assert!(guard.is_ok());
        drop(guard);
        assert!(child.wait().expect("helper should exit").success());
    }

    #[test]
    fn process_death_releases_an_exclusive_lock() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let ready = temporary.path().join("ready");
        let mut child = spawn_helper(&path, LockMode::Exclusive, &ready, 10_000);
        wait_ready(&ready);
        child.kill().expect("helper should be terminated");
        let _ = child.wait().expect("terminated helper should be reaped");

        let guard = LockGuard::acquire(&path, LockMode::Exclusive, LockBehavior::FailFast);

        assert!(guard.is_ok());
    }

    #[test]
    fn normal_process_exit_releases_an_exclusive_lock() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let ready = temporary.path().join("ready");
        let mut child = spawn_helper(&path, LockMode::Exclusive, &ready, 10);
        wait_ready(&ready);
        assert!(child.wait().expect("helper should exit").success());

        let guard = LockGuard::acquire(&path, LockMode::Exclusive, LockBehavior::FailFast);

        assert!(guard.is_ok());
    }

    #[test]
    fn attach_setup_releases_shared_lock_before_session_streaming() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let path = prepared_lock(temporary.path());
        let simulated_session = with_shared_lock_for_attach(&path, LockBehavior::FailFast, || {
            Ok::<_, Infallible>(String::from("stream remains alive"))
        })
        .expect("attach setup should succeed");

        let exclusive = LockGuard::acquire(&path, LockMode::Exclusive, LockBehavior::FailFast);

        assert_eq!(simulated_session, "stream remains alive");
        assert!(exclusive.is_ok());
    }
}
