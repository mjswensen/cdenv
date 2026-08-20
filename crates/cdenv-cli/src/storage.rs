//! Secure creation and atomic replacement of cdenv-managed filesystem objects.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{ManagedPathError, ManagedPathKind, ManagedPathState, inspect_managed_path};

/// The required Unix mode for a cdenv-managed object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedMode {
    /// A private directory (`0700`).
    PrivateDirectory,
    /// A private file (`0600`).
    PrivateFile,
    /// A world-readable, owner-writable file (`0644`).
    PublicFile,
}

impl ManagedMode {
    /// Returns the Unix permission bits for this policy.
    #[must_use]
    pub const fn unix_bits(self) -> u32 {
        match self {
            Self::PrivateDirectory => 0o700,
            Self::PrivateFile => 0o600,
            Self::PublicFile => 0o644,
        }
    }
}

/// A durability step at which an atomic replacement can fail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AtomicWriteStage {
    /// Creating the operation-owned temporary file.
    CreateTemporary,
    /// Applying final permissions before writing content.
    SetPermissions,
    /// Writing all content.
    Write,
    /// Flushing userspace buffers.
    Flush,
    /// Synchronizing the temporary file.
    SyncFile,
    /// Renaming the temporary file over its destination.
    Rename,
    /// Synchronizing the containing directory.
    SyncParent,
}

impl fmt::Display for AtomicWriteStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CreateTemporary => "create temporary file",
            Self::SetPermissions => "set temporary-file permissions",
            Self::Write => "write temporary file",
            Self::Flush => "flush temporary file",
            Self::SyncFile => "sync temporary file",
            Self::Rename => "rename temporary file",
            Self::SyncParent => "sync parent directory",
        })
    }
}

/// A managed storage operation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum StorageError {
    /// A managed path failed secure inspection.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// The destination has no containing directory or filename.
    #[error("managed file path must have a parent and filename: {path:?}")]
    InvalidDestination {
        /// The rejected destination.
        path: PathBuf,
    },
    /// An ancestor is a symbolic link.
    #[error("managed path parent component must not be a symbolic link: {path:?}")]
    ParentSymlink {
        /// The rejected component.
        path: PathBuf,
    },
    /// An ancestor is not a directory.
    #[error("managed path parent component is not a directory: {path:?}")]
    ParentNotDirectory {
        /// The rejected component.
        path: PathBuf,
    },
    /// Randomness needed for an unguessable temporary name was unavailable.
    #[error("cannot generate a unique temporary filename: {source}")]
    Random {
        /// The random-source failure.
        #[source]
        source: getrandom::Error,
    },
    /// All bounded unique-name attempts collided.
    #[error("cannot allocate a unique temporary file beside {path:?}")]
    TemporaryNameExhausted {
        /// The target file.
        path: PathBuf,
    },
    /// A filesystem durability step failed.
    #[error("cannot {stage} for managed path {path:?}: {source}")]
    Io {
        /// The failed step.
        stage: AtomicWriteStage,
        /// The managed destination.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Creates a managed directory if absent and enforces its exact private mode.
///
/// Existing path components are inspected without following symlinks. This
/// function changes only `path`; callers must never pass a checkout path.
///
/// # Errors
///
/// Returns [`StorageError`] for unsafe components, wrong ownership or kind,
/// and filesystem failures.
pub fn ensure_private_directory(path: &Path) -> Result<(), StorageError> {
    inspect_parent_components(path)?;
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(StorageError::Io {
                stage: AtomicWriteStage::CreateTemporary,
                path: path.to_path_buf(),
                source,
            });
        }
    }
    inspect_managed_path(path, ManagedPathKind::Directory, current_user_id())?;
    set_mode(path, ManagedMode::PrivateDirectory).map_err(|source| StorageError::Io {
        stage: AtomicWriteStage::SetPermissions,
        path: path.to_path_buf(),
        source,
    })
}

/// Tightens an existing managed file to its exact policy mode.
///
/// # Errors
///
/// Returns [`StorageError`] if the path is missing, unsafe, wrongly owned, or
/// cannot have its permissions changed.
pub fn tighten_managed_file(path: &Path, mode: ManagedMode) -> Result<(), StorageError> {
    inspect_parent_components(path)?;
    match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Valid => set_mode(path, mode).map_err(|source| StorageError::Io {
            stage: AtomicWriteStage::SetPermissions,
            path: path.to_path_buf(),
            source,
        }),
        ManagedPathState::Missing => Err(StorageError::Io {
            stage: AtomicWriteStage::SetPermissions,
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::NotFound, "managed file does not exist"),
        }),
    }
}

/// Atomically replaces a managed file with complete content.
///
/// The temporary file is created in the destination directory, receives its
/// final mode before content, and is removed by RAII on every pre-rename exit.
/// The destination is never opened through a symlink.
///
/// # Errors
///
/// Returns [`StorageError`] if path validation or any durability step fails.
pub fn atomic_write(path: &Path, content: &[u8], mode: ManagedMode) -> Result<(), StorageError> {
    atomic_write_with_hook(path, content, mode, &NoopHook)
}

trait WriteHook {
    fn before(&self, _stage: AtomicWriteStage) -> io::Result<()> {
        Ok(())
    }
}

struct NoopHook;
impl WriteHook for NoopHook {}

fn atomic_write_with_hook(
    path: &Path,
    content: &[u8],
    mode: ManagedMode,
    hook: &impl WriteHook,
) -> Result<(), StorageError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| StorageError::InvalidDestination {
            path: path.to_path_buf(),
        })?;
    if path.file_name().is_none() {
        return Err(StorageError::InvalidDestination {
            path: path.to_path_buf(),
        });
    }

    inspect_parent_components(path)?;
    inspect_managed_path(parent, ManagedPathKind::Directory, current_user_id())?;
    let _ = inspect_managed_path(path, ManagedPathKind::File, current_user_id())?;

    hook_stage(hook, AtomicWriteStage::CreateTemporary, path)?;
    let (file, temporary_path) = create_unique_temporary(path, mode)?;
    let mut temporary = TemporaryFile {
        file: Some(file),
        path: temporary_path,
    };

    hook_stage(hook, AtomicWriteStage::SetPermissions, path)?;
    set_file_mode(temporary.file_mut(), mode)
        .map_err(|source| io_error(AtomicWriteStage::SetPermissions, path, source))?;

    hook_stage(hook, AtomicWriteStage::Write, path)?;
    temporary
        .file_mut()
        .write_all(content)
        .map_err(|source| io_error(AtomicWriteStage::Write, path, source))?;
    hook_stage(hook, AtomicWriteStage::Flush, path)?;
    temporary
        .file_mut()
        .flush()
        .map_err(|source| io_error(AtomicWriteStage::Flush, path, source))?;
    hook_stage(hook, AtomicWriteStage::SyncFile, path)?;
    temporary
        .file_mut()
        .sync_all()
        .map_err(|source| io_error(AtomicWriteStage::SyncFile, path, source))?;
    temporary.close();

    hook_stage(hook, AtomicWriteStage::Rename, path)?;
    fs::rename(&temporary.path, path)
        .map_err(|source| io_error(AtomicWriteStage::Rename, path, source))?;
    temporary.disarm();

    hook_stage(hook, AtomicWriteStage::SyncParent, path)?;
    sync_parent(parent).map_err(|source| io_error(AtomicWriteStage::SyncParent, path, source))
}

fn hook_stage(
    hook: &impl WriteHook,
    stage: AtomicWriteStage,
    path: &Path,
) -> Result<(), StorageError> {
    hook.before(stage)
        .map_err(|source| io_error(stage, path, source))
}

fn io_error(stage: AtomicWriteStage, path: &Path, source: io::Error) -> StorageError {
    StorageError::Io {
        stage,
        path: path.to_path_buf(),
        source,
    }
}

fn create_unique_temporary(
    destination: &Path,
    mode: ManagedMode,
) -> Result<(File, PathBuf), StorageError> {
    const MAX_ATTEMPTS: usize = 128;
    let parent = destination
        .parent()
        .ok_or_else(|| StorageError::InvalidDestination {
            path: destination.to_path_buf(),
        })?;
    let filename = destination
        .file_name()
        .ok_or_else(|| StorageError::InvalidDestination {
            path: destination.to_path_buf(),
        })?;

    for _ in 0..MAX_ATTEMPTS {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|source| StorageError::Random { source })?;
        let name = format!(
            ".{}.cdenv-tmp-{}",
            filename.to_string_lossy(),
            hex::encode(random)
        );
        let candidate = parent.join(name);
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        configure_creation_mode(&mut options, mode);
        match options.open(&candidate) {
            Ok(file) => return Ok((file, candidate)),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(io_error(
                    AtomicWriteStage::CreateTemporary,
                    destination,
                    source,
                ));
            }
        }
    }
    Err(StorageError::TemporaryNameExhausted {
        path: destination.to_path_buf(),
    })
}

struct TemporaryFile {
    file: Option<File>,
    path: PathBuf,
}

impl TemporaryFile {
    fn file_mut(&mut self) -> &mut File {
        match self.file.as_mut() {
            Some(file) => file,
            None => unreachable!("temporary file is closed only immediately before rename"),
        }
    }

    fn close(&mut self) {
        self.file.take();
    }

    fn disarm(&mut self) {
        self.path.clear();
    }
}

impl Drop for TemporaryFile {
    fn drop(&mut self) {
        if !self.path.as_os_str().is_empty() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn inspect_parent_components(path: &Path) -> Result<(), StorageError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    for ancestor in parent.ancestors() {
        if ancestor.as_os_str().is_empty() {
            continue;
        }
        let metadata = match fs::symlink_metadata(ancestor) {
            Ok(metadata) => metadata,
            Err(source) if source.kind() == io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(StorageError::Io {
                    stage: AtomicWriteStage::CreateTemporary,
                    path: ancestor.to_path_buf(),
                    source,
                });
            }
        };
        if metadata.file_type().is_symlink() {
            return Err(StorageError::ParentSymlink {
                path: ancestor.to_path_buf(),
            });
        }
        if !metadata.is_dir() {
            return Err(StorageError::ParentNotDirectory {
                path: ancestor.to_path_buf(),
            });
        }
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

#[cfg(unix)]
fn configure_creation_mode(options: &mut OpenOptions, mode: ManagedMode) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(mode.unix_bits());
}

#[cfg(not(unix))]
fn configure_creation_mode(_options: &mut OpenOptions, _mode: ManagedMode) {}

#[cfg(unix)]
fn set_file_mode(file: &File, mode: ManagedMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode.unix_bits()))
}

#[cfg(not(unix))]
fn set_file_mode(_file: &File, _mode: ManagedMode) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: ManagedMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode.unix_bits()))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: ManagedMode) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_parent(parent: &Path) -> io::Result<()> {
    File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent(_parent: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    struct FailAt(AtomicWriteStage);
    impl WriteHook for FailAt {
        fn before(&self, stage: AtomicWriteStage) -> io::Result<()> {
            if stage == self.0 {
                Err(io::Error::other("injected failure"))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn fault_at_each_step_never_exposes_partial_content() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let destination = temporary.path().join("state.json");
        let stages = [
            AtomicWriteStage::CreateTemporary,
            AtomicWriteStage::SetPermissions,
            AtomicWriteStage::Write,
            AtomicWriteStage::Flush,
            AtomicWriteStage::SyncFile,
            AtomicWriteStage::Rename,
            AtomicWriteStage::SyncParent,
        ];
        for stage in stages {
            fs::write(&destination, b"old-complete").expect("old state should be installed");
            let result = atomic_write_with_hook(
                &destination,
                b"new-complete",
                ManagedMode::PrivateFile,
                &FailAt(stage),
            );
            assert!(result.is_err(), "stage {stage:?} should fail");
            let observed = fs::read(&destination).expect("a complete version should remain");
            assert!(
                observed == b"old-complete" || observed == b"new-complete",
                "stage {stage:?} exposed partial bytes: {observed:?}"
            );
        }
    }

    #[test]
    fn cleanup_removes_only_the_operation_owned_temporary() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let destination = temporary.path().join("state.json");
        let foreign = temporary.path().join(".state.json.cdenv-tmp-foreign");
        fs::write(&foreign, b"foreign").expect("foreign file should exist");

        let _ = atomic_write_with_hook(
            &destination,
            b"new",
            ManagedMode::PrivateFile,
            &FailAt(AtomicWriteStage::Write),
        );

        assert_eq!(
            fs::read(foreign).expect("foreign file should remain readable"),
            b"foreign"
        );
    }

    #[test]
    fn concurrent_writers_use_distinct_temporary_names() {
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let destination = temporary.path().join("state.json");
        let mut threads = Vec::new();
        for value in 0..8 {
            let destination = destination.clone();
            threads.push(std::thread::spawn(move || {
                atomic_write(
                    &destination,
                    format!("complete-{value}").as_bytes(),
                    ManagedMode::PrivateFile,
                )
            }));
        }
        for thread in threads {
            thread
                .join()
                .expect("writer should not panic")
                .expect("writer should succeed");
        }
        let content = fs::read_to_string(destination).expect("final file should exist");
        assert!(content.starts_with("complete-"));
    }

    #[cfg(unix)]
    #[test]
    fn directory_and_file_modes_are_exact() {
        use std::os::unix::fs::PermissionsExt;
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let directory = temporary.path().join("managed");
        ensure_private_directory(&directory).expect("managed directory should be created");
        let private = directory.join("private");
        let public = directory.join("public");
        atomic_write(&private, b"secret", ManagedMode::PrivateFile)
            .expect("private file should be created");
        atomic_write(&public, b"public", ManagedMode::PublicFile)
            .expect("public file should be created");
        let modes = [directory, private, public].map(|path| {
            fs::metadata(path)
                .expect("metadata should exist")
                .permissions()
                .mode()
                & 0o777
        });
        assert_eq!(modes, [0o700, 0o600, 0o644]);
    }

    #[cfg(unix)]
    #[test]
    fn writer_refuses_symlink_and_non_regular_targets() {
        use std::os::unix::fs::symlink;
        let temporary = tempfile::tempdir().expect("temporary directory should exist");
        let real = temporary.path().join("real");
        fs::write(&real, b"real").expect("real file should exist");
        let link = temporary.path().join("link");
        symlink(&real, &link).expect("symlink should exist");
        assert!(matches!(
            atomic_write(&link, b"replacement", ManagedMode::PrivateFile),
            Err(StorageError::ManagedPath(ManagedPathError::Symlink { .. }))
        ));
        let directory = temporary.path().join("directory");
        fs::create_dir(&directory).expect("directory target should exist");
        assert!(matches!(
            atomic_write(&directory, b"replacement", ManagedMode::PrivateFile),
            Err(StorageError::ManagedPath(
                ManagedPathError::WrongKind { .. }
            ))
        ));
    }
}
