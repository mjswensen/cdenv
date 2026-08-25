//! Stable OpenSSH client and workspace host identities.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cdenv_core::{WorkspaceHost, WorkspaceName};
use ssh_key::{Algorithm, LineEnding, PrivateKey, PublicKey, private::Ed25519Keypair};
use thiserror::Error;
use zeroize::Zeroizing;

use crate::{
    AgentProvisionAsset, CdenvRoot, LockBehavior, LockError, LockGuard, LockMode, ManagedMode,
    ManagedPathError, ManagedPathKind, ManagedPathState, StorageError, atomic_write,
    ensure_lock_file, ensure_private_directory, inspect_managed_path, tighten_managed_file,
};

const CLIENT_COMMENT: &str = "cdenv-client";
const HOST_COMMENT: &str = "cdenv-host";

/// A private key and exact matching public key borrowed for container provisioning.
///
/// The private bytes are zeroized when this value is dropped and are never exposed
/// through `Debug`, serialization, or an owned byte accessor.
pub struct WorkspaceSshAssets {
    host_private_key: Zeroizing<Vec<u8>>,
    authorized_client_key: Vec<u8>,
}

impl std::fmt::Debug for WorkspaceSshAssets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceSshAssets")
            .field("host_private_key", &"[REDACTED]")
            .field("authorized_client_key", &"[PUBLIC KEY]")
            .finish()
    }
}

impl WorkspaceSshAssets {
    /// Borrows the OpenSSH host private-key bytes.
    #[must_use]
    pub fn host_private_key(&self) -> &[u8] {
        &self.host_private_key
    }

    /// Borrows the exact authorized OpenSSH client public-key line.
    #[must_use]
    pub fn authorized_client_key(&self) -> &[u8] {
        &self.authorized_client_key
    }

    /// Builds the exact two restricted assets for an always-upload provisioning pass.
    ///
    /// The host private key and authorized public key are both installed `0600`:
    /// the latter is authorization policy inside the container, not the host-side
    /// shareable `.pub` file. The returned values borrow this zeroizing owner.
    #[must_use]
    pub fn provision_assets<'a>(
        &'a self,
        host_key_destination: &'a str,
        authorized_key_destination: &'a str,
    ) -> [AgentProvisionAsset<'a>; 2] {
        [
            AgentProvisionAsset {
                contents: &self.host_private_key,
                destination: host_key_destination,
                mode: 0o600,
            },
            AgentProvisionAsset {
                contents: &self.authorized_client_key,
                destination: authorized_key_destination,
                mode: 0o600,
            },
        ]
    }
}

/// Stable public identity facts used to generate `known_hosts`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceHostIdentity {
    name: WorkspaceName,
    public_key: String,
}

impl WorkspaceHostIdentity {
    /// Returns the workspace owning this host identity.
    #[must_use]
    pub const fn name(&self) -> &WorkspaceName {
        &self.name
    }

    /// Returns an exact `known_hosts` line for `<name>.cdenv`.
    #[must_use]
    pub fn known_hosts_line(&self) -> String {
        let host = WorkspaceHost::from_workspace_name(self.name.clone());
        let mut fields = self.public_key.split_whitespace();
        let algorithm = fields.next().unwrap_or_default();
        let key = fields.next().unwrap_or_default();
        format!("{host} {algorithm} {key}\n")
    }
}

/// SSH identity generation, validation, or storage failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SshIdentityError {
    /// Secure managed storage setup failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// A managed key path was unsafe, wrongly owned, or the wrong kind.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// The installation-wide SSH setup lock could not be prepared or acquired.
    #[error(transparent)]
    Lock(#[from] LockError),
    /// Operating-system randomness was unavailable.
    #[error("cannot generate an Ed25519 SSH identity: {0}")]
    Random(#[from] getrandom::Error),
    /// An existing or generated OpenSSH key could not be decoded or encoded.
    #[error("invalid OpenSSH {role} key at {path:?}: {source}")]
    Key {
        /// The expected identity role.
        role: &'static str,
        /// The managed key path.
        path: PathBuf,
        /// The key-format failure.
        #[source]
        source: ssh_key::Error,
    },
    /// A managed key could not be read.
    #[error("cannot read managed SSH key {path:?}: {source}")]
    Read {
        /// The managed key path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Managed key text was not valid UTF-8.
    #[error("managed OpenSSH {role} key at {path:?} must be valid UTF-8")]
    NonUtf8 {
        /// The expected identity role.
        role: &'static str,
        /// The managed key path.
        path: PathBuf,
    },
    /// A key had an algorithm other than Ed25519.
    #[error("managed SSH {role} key at {path:?} must be Ed25519")]
    WrongAlgorithm {
        /// The expected identity role.
        role: &'static str,
        /// The managed key path.
        path: PathBuf,
    },
    /// The persisted public key does not match its private key.
    #[error("managed SSH {role} public key does not match its private key")]
    PublicKeyMismatch {
        /// The expected identity role.
        role: &'static str,
    },
}

/// Ensures the installation client identity and one persistent workspace host identity.
///
/// Existing private keys are validated and retained, which keeps host identity stable
/// across rebuilds. Missing public files are re-derived; conflicting public files are
/// rejected rather than silently changing trust material.
///
/// # Errors
///
/// Returns [`SshIdentityError`] for unsafe paths, I/O, randomness, malformed keys,
/// non-Ed25519 keys, or mismatched public material.
pub fn ensure_workspace_ssh_identity(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<WorkspaceHostIdentity, SshIdentityError> {
    ensure_private_directory(root.as_path())?;
    ensure_private_directory(&root.ssh().root())?;
    ensure_lock_file(&root.ssh().setup_lock())?;
    let _lock = LockGuard::acquire(
        &root.ssh().setup_lock(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )?;
    ensure_workspace_ssh_identity_unlocked(root, name)
}

pub(crate) fn ensure_workspace_ssh_identity_unlocked(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<WorkspaceHostIdentity, SshIdentityError> {
    ensure_private_directory(&root.ssh().host_keys_dir())?;
    let _ = ensure_keypair(
        &root.ssh().private_key(),
        &root.ssh().public_key(),
        "client",
        CLIENT_COMMENT,
    )?;
    let public_key = ensure_keypair(
        &root.ssh().host_private_key(name),
        &root.ssh().host_public_key(name),
        "host",
        HOST_COMMENT,
    )?;

    Ok(WorkspaceHostIdentity {
        name: name.clone(),
        public_key,
    })
}

/// Loads the exact private host key and authorized client key for provisioning.
///
/// Call [`ensure_workspace_ssh_identity`] first in the same successful mutating
/// transaction. This loader revalidates both roles and matching public files.
///
/// # Errors
///
/// Returns [`SshIdentityError`] when either identity is missing, malformed,
/// wrongly typed, mismatched, unsafe, or unreadable.
pub fn load_workspace_ssh_assets(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<WorkspaceSshAssets, SshIdentityError> {
    ensure_lock_file(&root.ssh().setup_lock())?;
    let _lock = LockGuard::acquire(
        &root.ssh().setup_lock(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )?;
    let client_public = validate_existing_keypair(
        &root.ssh().private_key(),
        &root.ssh().public_key(),
        "client",
    )?;
    let _ = validate_existing_keypair(
        &root.ssh().host_private_key(name),
        &root.ssh().host_public_key(name),
        "host",
    )?;
    let host_path = root.ssh().host_private_key(name);
    let host_private_key = Zeroizing::new(read_regular_key(&host_path)?);

    Ok(WorkspaceSshAssets {
        host_private_key,
        authorized_client_key: format!("{client_public}\n").into_bytes(),
    })
}

fn ensure_keypair(
    private_path: &Path,
    public_path: &Path,
    role: &'static str,
    comment: &str,
) -> Result<String, SshIdentityError> {
    match inspect_managed_path(private_path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Missing => generate_keypair(private_path, public_path, role, comment),
        ManagedPathState::Valid => validate_existing_keypair(private_path, public_path, role),
    }
}

fn generate_keypair(
    private_path: &Path,
    public_path: &Path,
    role: &'static str,
    comment: &str,
) -> Result<String, SshIdentityError> {
    if inspect_managed_path(public_path, ManagedPathKind::File, current_user_id())?
        == ManagedPathState::Valid
    {
        return Err(SshIdentityError::PublicKeyMismatch { role });
    }
    let mut seed = Zeroizing::new([0_u8; 32]);
    getrandom::fill(seed.as_mut())?;
    let mut private = PrivateKey::from(Ed25519Keypair::from_seed(&seed));
    private.set_comment(comment);
    let private_text =
        private
            .to_openssh(LineEnding::LF)
            .map_err(|source| SshIdentityError::Key {
                role,
                path: private_path.to_path_buf(),
                source,
            })?;
    let mut public = private.public_key().clone();
    public.set_comment(comment);
    let public_text = public
        .to_openssh()
        .map_err(|source| SshIdentityError::Key {
            role,
            path: public_path.to_path_buf(),
            source,
        })?;

    atomic_write(
        private_path,
        private_text.as_bytes(),
        ManagedMode::PrivateFile,
    )?;
    atomic_write(
        public_path,
        format!("{public_text}\n").as_bytes(),
        ManagedMode::PublicFile,
    )?;
    Ok(public_text)
}

fn validate_existing_keypair(
    private_path: &Path,
    public_path: &Path,
    role: &'static str,
) -> Result<String, SshIdentityError> {
    let private_bytes = Zeroizing::new(read_regular_key(private_path)?);
    let private_text =
        std::str::from_utf8(&private_bytes).map_err(|_| SshIdentityError::NonUtf8 {
            role,
            path: private_path.to_path_buf(),
        })?;
    let private =
        PrivateKey::from_openssh(private_text).map_err(|source| SshIdentityError::Key {
            role,
            path: private_path.to_path_buf(),
            source,
        })?;
    if private.algorithm() != Algorithm::Ed25519 {
        return Err(SshIdentityError::WrongAlgorithm {
            role,
            path: private_path.to_path_buf(),
        });
    }
    tighten_managed_file(private_path, ManagedMode::PrivateFile)?;

    let expected = private.public_key();
    match inspect_managed_path(public_path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Missing => {
            let mut public = expected.clone();
            public.set_comment(if role == "client" {
                CLIENT_COMMENT
            } else {
                HOST_COMMENT
            });
            let text = public
                .to_openssh()
                .map_err(|source| SshIdentityError::Key {
                    role,
                    path: public_path.to_path_buf(),
                    source,
                })?;
            atomic_write(
                public_path,
                format!("{text}\n").as_bytes(),
                ManagedMode::PublicFile,
            )?;
            Ok(text)
        }
        ManagedPathState::Valid => {
            let bytes = read_regular_key(public_path)?;
            let text = std::str::from_utf8(&bytes).map_err(|_| SshIdentityError::NonUtf8 {
                role,
                path: public_path.to_path_buf(),
            })?;
            let public = PublicKey::from_openssh(text.trim_end()).map_err(|source| {
                SshIdentityError::Key {
                    role,
                    path: public_path.to_path_buf(),
                    source,
                }
            })?;
            if public.algorithm() != Algorithm::Ed25519 || public.key_data() != expected.key_data()
            {
                return Err(SshIdentityError::PublicKeyMismatch { role });
            }
            tighten_managed_file(public_path, ManagedMode::PublicFile)?;
            Ok(text.trim_end().to_owned())
        }
    }
}

fn read_regular_key(path: &Path) -> Result<Vec<u8>, SshIdentityError> {
    match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Missing => fs::read(path).map_err(|source| SshIdentityError::Read {
            path: path.to_path_buf(),
            source,
        }),
        ManagedPathState::Valid => fs::read(path).map_err(|source| SshIdentityError::Read {
            path: path.to_path_buf(),
            source,
        }),
    }
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
