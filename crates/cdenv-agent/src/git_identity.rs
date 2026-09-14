//! Reversible, fill-only-missing Git author identity defaults.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

use cdenv_core::git_identity::{GitIdentityMetadata, MAX_GIT_IDENTITY_BYTES};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::git_credentials::{atomic_write, prepare_owned_directory};

/// Name of the cdenv-owned identity metadata file.
pub const GIT_IDENTITY_METADATA_NAME: &str = "git-identity.json";

/// A cdenv-owned identity-default integration outside user and repository files.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedGitIdentityIntegration {
    metadata_path: PathBuf,
}

impl ManagedGitIdentityIntegration {
    /// Atomically refreshes only cdenv's validated identity metadata.
    ///
    /// # Errors
    ///
    /// Rejects unsafe owned paths or invalid metadata without changing Git files.
    pub fn refresh(
        directory: &Path,
        metadata: &GitIdentityMetadata,
    ) -> Result<Self, GitIdentityIntegrationError> {
        prepare_owned_directory(directory).map_err(|_| GitIdentityIntegrationError::UnsafePath)?;
        let bytes = Zeroizing::new(
            metadata
                .encode()
                .map_err(|_| GitIdentityIntegrationError::Metadata)?,
        );
        let metadata_path = directory.join(GIT_IDENTITY_METADATA_NAME);
        atomic_write(&metadata_path, &bytes).map_err(|_| GitIdentityIntegrationError::Io)?;
        Ok(Self { metadata_path })
    }

    /// Borrows the cdenv-owned metadata path.
    #[must_use]
    pub fn metadata_path(&self) -> &Path {
        &self.metadata_path
    }

    /// Removes only the verified cdenv-owned metadata file.
    ///
    /// # Errors
    ///
    /// Rejects replacement, ownership, type, or mode drift.
    pub fn remove(&self) -> Result<(), GitIdentityIntegrationError> {
        match fs::symlink_metadata(&self.metadata_path) {
            Ok(metadata) if is_owned_file(&metadata) => {
                fs::remove_file(&self.metadata_path).map_err(|_| GitIdentityIntegrationError::Io)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(GitIdentityIntegrationError::UnsafePath),
            Err(_) => Err(GitIdentityIntegrationError::Io),
        }
    }
}

/// Runs real Git with host defaults inserted only for fields absent from Git's
/// effective configuration in the invocation's original context.
///
/// Existing system/global/local/conditional configuration, `GIT_CONFIG_*`, and
/// command-level `-c`/`--config-env` options are included in the presence probe.
/// Explicit author/committer environment variables are left untouched and retain
/// Git's normal precedence over configuration.
///
/// # Errors
///
/// Rejects unsafe metadata, unsupported global-option syntax, recursive wrapper
/// selection, or a failed configuration presence probe.
pub fn run_git_with_identity(
    git_executable: &Path,
    metadata_path: &Path,
    arguments: &[OsString],
) -> Result<ExitStatus, GitIdentityIntegrationError> {
    validate_executable(git_executable)?;
    let metadata = load_metadata(metadata_path)?;
    let prefix_length = global_prefix_length(arguments)?;
    let (global, command_arguments) = arguments.split_at(prefix_length);

    let name_missing =
        metadata.name().is_some() && !configured(git_executable, global, "user.name")?;
    let email_missing =
        metadata.email().is_some() && !configured(git_executable, global, "user.email")?;

    let mut command = Command::new(git_executable);
    command.args(global);
    if name_missing {
        command.arg("-c").arg(format!(
            "user.name={}",
            metadata
                .name()
                .ok_or(GitIdentityIntegrationError::Metadata)?
        ));
    }
    if email_missing {
        command.arg("-c").arg(format!(
            "user.email={}",
            metadata
                .email()
                .ok_or(GitIdentityIntegrationError::Metadata)?
        ));
    }
    command
        .args(command_arguments)
        .status()
        .map_err(|_| GitIdentityIntegrationError::Git)
}

fn load_metadata(path: &Path) -> Result<GitIdentityMetadata, GitIdentityIntegrationError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| GitIdentityIntegrationError::Io)?;
    if !is_owned_file(&metadata) || metadata.len() > MAX_GIT_IDENTITY_BYTES as u64 {
        return Err(GitIdentityIntegrationError::UnsafePath);
    }
    let bytes = Zeroizing::new(fs::read(path).map_err(|_| GitIdentityIntegrationError::Io)?);
    GitIdentityMetadata::decode(&bytes).map_err(|_| GitIdentityIntegrationError::Metadata)
}

fn is_owned_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file()
        && !metadata.file_type().is_symlink()
        && metadata.uid() == nix::unistd::geteuid().as_raw()
        && metadata.permissions().mode() & 0o777 == 0o600
        && metadata.nlink() == 1
}

fn validate_executable(path: &Path) -> Result<(), GitIdentityIntegrationError> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|value| value.contains(['\n', '\r', '\0']))
    {
        return Err(GitIdentityIntegrationError::UnsafePath);
    }
    let current = std::env::current_exe().map_err(|_| GitIdentityIntegrationError::Git)?;
    if current == path {
        return Err(GitIdentityIntegrationError::RecursiveGit);
    }
    Ok(())
}

fn configured(
    git_executable: &Path,
    global: &[OsString],
    key: &str,
) -> Result<bool, GitIdentityIntegrationError> {
    let status = Command::new(git_executable)
        .args(global)
        .args([OsStr::new("config"), OsStr::new("--get"), OsStr::new(key)])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| GitIdentityIntegrationError::Git)?;
    match status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(GitIdentityIntegrationError::Git),
    }
}

fn global_prefix_length(arguments: &[OsString]) -> Result<usize, GitIdentityIntegrationError> {
    let mut index = 0;
    while let Some(argument) = arguments.get(index) {
        let value = argument
            .to_str()
            .ok_or(GitIdentityIntegrationError::Arguments)?;
        if value == "--" || !value.starts_with('-') || value == "-" {
            break;
        }
        if matches!(
            value,
            "-c" | "-C" | "--config-env" | "--git-dir" | "--work-tree" | "--namespace"
        ) {
            if arguments.get(index + 1).is_none() {
                return Err(GitIdentityIntegrationError::Arguments);
            }
            index += 2;
        } else if value.starts_with("-c") && value.len() > 2
            || value.starts_with("-C") && value.len() > 2
            || [
                "--config-env=",
                "--git-dir=",
                "--work-tree=",
                "--namespace=",
            ]
            .iter()
            .any(|prefix| value.starts_with(prefix))
            || matches!(
                value,
                "--bare"
                    | "--no-replace-objects"
                    | "--literal-pathspecs"
                    | "--glob-pathspecs"
                    | "--noglob-pathspecs"
                    | "--icase-pathspecs"
                    | "--no-optional-locks"
            )
        {
            index += 1;
        } else {
            // Help/version and uncommon execution-control options do not need
            // identity defaults; reject rather than probe a different context.
            return Err(GitIdentityIntegrationError::Arguments);
        }
    }
    Ok(index)
}

/// Value-free identity integration failures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GitIdentityIntegrationError {
    /// An owned path or real Git executable was unsafe.
    #[error("unsafe cdenv Git identity integration path")]
    UnsafePath,
    /// Metadata violated the closed, bounded identity schema.
    #[error("invalid cdenv Git identity metadata")]
    Metadata,
    /// Global Git arguments could not be preserved safely.
    #[error("unsupported Git global argument for identity defaults")]
    Arguments,
    /// The selected real Git executable would recurse into the wrapper.
    #[error("cdenv Git identity wrapper selected itself")]
    RecursiveGit,
    /// Real Git or its configuration presence probe failed.
    #[error("Git identity default invocation failed")]
    Git,
    /// The cdenv-owned metadata file could not be updated or read.
    #[error("cannot update cdenv Git identity integration")]
    Io,
}
