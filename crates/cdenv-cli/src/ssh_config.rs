//! Explicit managed OpenSSH configuration and executable-path resolution.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cdenv_core::{WorkspaceHost, WorkspaceName};
use thiserror::Error;

use crate::ssh_identity::ensure_workspace_ssh_identity_unlocked;
use crate::{
    CdenvRoot, LockBehavior, LockError, LockGuard, LockMode, ManagedMode, RequiredPathError,
    SshIdentityError, StorageError, atomic_write, ensure_lock_file, ensure_private_directory,
    validate_openssh_path,
};

/// Executable lookup failure while preserving the stable invoked path.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExecutableResolutionError {
    /// Neither `argv[0]` nor the process fallback produced an executable path.
    #[error("cannot resolve the invoked cdenv executable: {0}")]
    CurrentExecutable(#[source] io::Error),
    /// The selected path was unsafe for OpenSSH configuration.
    #[error(transparent)]
    Path(#[from] RequiredPathError),
}

/// Managed SSH configuration generation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SshConfigError {
    /// Stable identity preparation failed.
    #[error(transparent)]
    Identity(#[from] SshIdentityError),
    /// A generated path was unsafe for OpenSSH configuration.
    #[error(transparent)]
    Path(#[from] RequiredPathError),
    /// Managed storage setup or replacement failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// The installation-wide SSH setup lock could not be prepared or acquired.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// The executable path must be absolute.
    #[error("the cdenv executable path used by SSH must be absolute: {path:?}")]
    RelativeExecutable {
        /// The rejected path.
        path: PathBuf,
    },
}

/// Resolves this invocation's executable without canonicalizing the final path.
///
/// A bare `argv[0]` is searched through `PATH`; path-containing values are made
/// absolute relative to the current directory. In both cases the returned final
/// symlink is deliberately retained so package-manager upgrades keep generated
/// configuration stable. [`env::current_exe`] is used only when invoked lookup
/// cannot identify an executable.
///
/// # Errors
///
/// Returns [`ExecutableResolutionError`] when no fallback is available or the
/// result cannot be represented safely in OpenSSH configuration.
pub fn resolve_current_executable(argv0: &OsStr) -> Result<PathBuf, ExecutableResolutionError> {
    let invoked = env::current_dir()
        .ok()
        .and_then(|cwd| resolve_invoked_executable(argv0, env::var_os("PATH").as_deref(), &cwd));
    let candidate = match invoked {
        Some(candidate) => candidate,
        None => env::current_exe().map_err(ExecutableResolutionError::CurrentExecutable)?,
    };
    validate_openssh_path(&candidate)?;
    Ok(candidate)
}

/// Resolves `argv0` through an explicit `PATH` and working directory.
///
/// This deterministic seam has the same no-canonicalization behavior as
/// [`resolve_current_executable`].
#[must_use]
pub fn resolve_invoked_executable(
    argv0: &OsStr,
    path: Option<&OsStr>,
    cwd: &Path,
) -> Option<PathBuf> {
    let invoked = Path::new(argv0);
    if invoked.components().count() > 1 || invoked.is_absolute() {
        let candidate = if invoked.is_absolute() {
            invoked.to_path_buf()
        } else {
            cwd.join(invoked)
        };
        return executable_file(&candidate).then_some(candidate);
    }

    env::split_paths(path.unwrap_or_else(|| OsStr::new("")))
        .map(|directory| {
            if directory.as_os_str().is_empty() {
                cwd.join(invoked)
            } else if directory.is_absolute() {
                directory.join(invoked)
            } else {
                cwd.join(directory).join(invoked)
            }
        })
        .find(|candidate| executable_file(candidate))
}

/// Ensures all stable identities and atomically regenerates managed SSH files.
///
/// Workspaces are sorted and deduplicated. Only explicit `<name>.cdenv` blocks
/// are emitted; no wildcard or workspace-bearing `%h` substitution exists.
///
/// # Errors
///
/// Returns [`SshConfigError`] for identity, path-rendering, or atomic-storage
/// failures.
pub fn regenerate_managed_ssh(
    root: &CdenvRoot,
    executable: &Path,
    workspaces: impl IntoIterator<Item = WorkspaceName>,
) -> Result<(), SshConfigError> {
    if !executable.is_absolute() {
        return Err(SshConfigError::RelativeExecutable {
            path: executable.to_path_buf(),
        });
    }
    validate_openssh_path(executable)?;
    validate_openssh_path(root.as_path())?;
    ensure_private_directory(root.as_path())?;
    ensure_private_directory(&root.ssh().root())?;
    ensure_lock_file(&root.ssh().setup_lock())?;
    let _lock = LockGuard::acquire(
        &root.ssh().setup_lock(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )?;

    let mut names: Vec<_> = workspaces.into_iter().collect();
    names.sort();
    names.dedup();

    let mut known_hosts = String::new();
    for name in &names {
        known_hosts
            .push_str(&ensure_workspace_ssh_identity_unlocked(root, name)?.known_hosts_line());
    }
    let config = render_managed_config(root, executable, &names)?;
    atomic_write(
        &root.ssh().known_hosts(),
        known_hosts.as_bytes(),
        ManagedMode::PrivateFile,
    )?;
    atomic_write(
        &root.ssh().config(),
        config.as_bytes(),
        ManagedMode::PrivateFile,
    )?;
    Ok(())
}

/// Renders the complete managed OpenSSH configuration without writing it.
///
/// # Errors
///
/// Returns [`SshConfigError`] when the executable or managed paths cannot be
/// represented safely.
pub fn render_managed_config(
    root: &CdenvRoot,
    executable: &Path,
    workspaces: &[WorkspaceName],
) -> Result<String, SshConfigError> {
    if !executable.is_absolute() {
        return Err(SshConfigError::RelativeExecutable {
            path: executable.to_path_buf(),
        });
    }
    let executable = validate_openssh_path(executable)?;
    let root_path = validate_openssh_path(root.as_path())?;
    let private_key_path = root.ssh().private_key();
    let private_key = validate_openssh_path(&private_key_path)?;
    let known_hosts_path = root.ssh().known_hosts();
    let known_hosts = validate_openssh_path(&known_hosts_path)?;
    let mut config = String::new();

    for (index, name) in workspaces.iter().enumerate() {
        if index != 0 {
            config.push('\n');
        }
        let host = WorkspaceHost::from_workspace_name(name.clone());
        let _ = write!(
            config,
            "Host {host}\n    HostName {host}\n    Port 22\n    User cdenv\n    ProxyCommand {} --root {} proxy {}\n    IdentityFile {}\n    IdentitiesOnly yes\n    UserKnownHostsFile {}\n    StrictHostKeyChecking yes\n    ServerAliveInterval 30\n    ServerAliveCountMax 3\n",
            shell_quote(executable),
            shell_quote(root_path),
            name,
            ssh_token(private_key),
            ssh_token(known_hosts),
        );
    }
    Ok(config)
}

fn ssh_token(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn executable_file(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Builds the exact system-SSH argument vector for a workspace.
#[must_use]
pub fn system_ssh_arguments(
    root: &CdenvRoot,
    name: &WorkspaceName,
    remote_argv: &[OsString],
) -> Vec<OsString> {
    let host = WorkspaceHost::from_workspace_name(name.clone());
    let mut arguments = vec![
        OsString::from("-F"),
        root.ssh().config().into_os_string(),
        OsString::from(host.to_string()),
    ];
    arguments.extend(remote_argv.iter().cloned());
    arguments
}
