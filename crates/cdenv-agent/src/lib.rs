//! Testable container-side boundary for `cdenv-agent`.
//!
//! Distribution targets are Linux. Platform-specific process and filesystem
//! behavior remains isolated behind compile-time platform modules.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The protocol spoken by this agent release.
pub const PROTOCOL_VERSION: u32 = 1;
/// The build identity supplied by the release pipeline.
pub const BUILD_ID: &str = match option_env!("CDENV_AGENT_BUILD_ID") {
    Some(build_id) if !build_id.is_empty() => build_id,
    _ => "development",
};

/// Machine-readable identity emitted by `cdenv-agent version`.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Version<'a> {
    /// Executable name.
    pub name: &'static str,
    /// Cargo package version.
    pub version: &'static str,
    /// Host-to-agent compatibility protocol.
    pub protocol_version: u32,
    /// Build pipeline identity.
    pub build_id: &'a str,
}

/// Effective selected-user identity emitted by `cdenv-agent identity`.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    /// Effective numeric user ID.
    pub uid: u32,
    /// Effective numeric primary group ID.
    pub gid: u32,
    /// Account home directory from the passwd database.
    pub home: String,
    /// Account login shell from the passwd database.
    pub shell: String,
}

/// One file installed atomically by a root provisioning operation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionFile {
    /// Staged regular source file owned by this operation.
    pub source: String,
    /// Final absolute installation path.
    pub destination: String,
    /// Final Unix file mode, encoded as an octal integer in JSON.
    pub mode: u32,
}

/// Root-only provision request read from a host-created manifest.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProvisionRequest {
    /// Agent build identity expected by the host.
    pub build_id: String,
    /// Agent protocol expected by the host.
    pub protocol_version: u32,
    /// UID owning all installed files.
    pub uid: u32,
    /// GID owning all installed files.
    pub gid: u32,
    /// Files to atomically install.
    pub files: Vec<ProvisionFile>,
}

/// Provisioning or platform failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentError {
    /// The current operating system cannot run the distributed agent.
    #[error("cdenv-agent supports Linux only; current operating system is `{operating_system}`")]
    UnsupportedOperatingSystem {
        /// Rust's name for the current operating system.
        operating_system: &'static str,
    },
    /// The identity database lacked the effective account.
    #[error("cannot resolve effective account {uid} in /etc/passwd")]
    MissingAccount {
        /// Effective UID not found in passwd data.
        uid: u32,
    },
    /// The current process was not root.
    #[error("cdenv-agent provision must run as UID 0")]
    NotRoot,
    /// The manifest does not match this distributed agent.
    #[error("provision manifest {field} does not match this agent")]
    ManifestMismatch {
        /// `buildId` or `protocolVersion`.
        field: &'static str,
    },
    /// A manifest path was not an absolute safe path.
    #[error("provision {kind} path `{path}` is unsafe")]
    UnsafePath {
        /// `source` or `destination`.
        kind: &'static str,
        /// Rejected path.
        path: String,
    },
    /// A staged source was not a regular non-symlink file.
    #[error("provision source `{path}` is not a regular file")]
    InvalidSource {
        /// Rejected source path.
        path: String,
    },
    /// A destination parent could not be created or secured.
    #[error("cannot prepare provision destination `{path}`: {source}")]
    PrepareDestination {
        /// Destination path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// An atomic installation operation failed.
    #[error("cannot install provision file `{path}`: {source}")]
    Install {
        /// Destination path.
        path: String,
        /// Underlying filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Ownership could not be set.
    #[error("cannot set ownership on `{path}`: {source}")]
    Ownership {
        /// Destination path.
        path: String,
        /// Underlying OS failure.
        #[source]
        source: nix::Error,
    },
}

/// Returns this executable's machine-readable version identity.
#[must_use]
pub const fn version() -> Version<'static> {
    Version {
        name: "cdenv-agent",
        version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        build_id: BUILD_ID,
    }
}

/// Verifies that the current host can run the agent.
///
/// # Errors
///
/// Returns [`AgentError::UnsupportedOperatingSystem`] on non-Linux hosts.
pub fn ensure_supported_platform() -> Result<(), AgentError> {
    platform::ensure_supported()
}

/// Reads the current effective user without invoking container utilities.
///
/// # Errors
///
/// Returns platform or passwd-database errors.
pub fn identity() -> Result<Identity, AgentError> {
    platform::identity()
}

/// Atomically installs a validated set of staged files without shell utilities.
///
/// # Errors
///
/// Returns typed platform, manifest, permission, path, ownership, or filesystem errors.
pub fn provision(request: &ProvisionRequest) -> Result<(), AgentError> {
    platform::provision(request)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Component, Path};

    use nix::unistd::{Gid, Uid, chown, getegid, geteuid};

    use super::{AgentError, BUILD_ID, Identity, PROTOCOL_VERSION, ProvisionRequest};

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the implementation shares the unsupported-host fallible API"
    )]
    pub(super) const fn ensure_supported() -> Result<(), AgentError> {
        Ok(())
    }

    pub(super) fn identity() -> Result<Identity, AgentError> {
        let uid = geteuid().as_raw();
        let gid = getegid().as_raw();
        let passwd =
            fs::read_to_string("/etc/passwd").map_err(|source| AgentError::PrepareDestination {
                path: "/etc/passwd".to_owned(),
                source,
            })?;
        let entry = passwd
            .lines()
            .find_map(|line| {
                let fields = line.split(':').collect::<Vec<_>>();
                (fields.len() >= 7 && fields[2].parse::<u32>().ok() == Some(uid))
                    .then_some((fields[5], fields[6]))
            })
            .ok_or(AgentError::MissingAccount { uid })?;
        Ok(Identity {
            uid,
            gid,
            home: entry.0.to_owned(),
            shell: entry.1.to_owned(),
        })
    }

    pub(super) fn provision(request: &ProvisionRequest) -> Result<(), AgentError> {
        if geteuid().as_raw() != 0 {
            return Err(AgentError::NotRoot);
        }
        if request.build_id != BUILD_ID {
            return Err(AgentError::ManifestMismatch { field: "buildId" });
        }
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(AgentError::ManifestMismatch {
                field: "protocolVersion",
            });
        }
        for file in &request.files {
            install(
                file.source.as_str(),
                file.destination.as_str(),
                file.mode,
                request.uid,
                request.gid,
            )?;
        }
        Ok(())
    }

    fn install(
        source: &str,
        destination: &str,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<(), AgentError> {
        let source = safe_path(source, "source")?;
        let destination = safe_path(destination, "destination")?;
        let source_metadata =
            fs::symlink_metadata(source).map_err(|_| AgentError::InvalidSource {
                path: source.display().to_string(),
            })?;
        if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
            return Err(AgentError::InvalidSource {
                path: source.display().to_string(),
            });
        }
        let parent = destination.parent().ok_or_else(|| AgentError::UnsafePath {
            kind: "destination",
            path: destination.display().to_string(),
        })?;
        fs::create_dir_all(parent).map_err(|source| AgentError::PrepareDestination {
            path: parent.display().to_string(),
            source,
        })?;
        let temporary = parent.join(format!(".cdenv-install-{}", std::process::id()));
        fs::copy(source, &temporary).map_err(|source| AgentError::Install {
            path: destination.display().to_string(),
            source,
        })?;
        fs::set_permissions(&temporary, fs::Permissions::from_mode(mode)).map_err(|source| {
            AgentError::Install {
                path: destination.display().to_string(),
                source,
            }
        })?;
        chown(
            &temporary,
            Some(Uid::from_raw(uid)),
            Some(Gid::from_raw(gid)),
        )
        .map_err(|source| AgentError::Ownership {
            path: temporary.display().to_string(),
            source,
        })?;
        fs::rename(&temporary, destination).map_err(|source| AgentError::Install {
            path: destination.display().to_string(),
            source,
        })
    }

    fn safe_path<'a>(value: &'a str, kind: &'static str) -> Result<&'a Path, AgentError> {
        let path = Path::new(value);
        if !path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::CurDir | Component::Prefix(_)
                )
            })
        {
            return Err(AgentError::UnsafePath {
                kind,
                path: value.to_owned(),
            });
        }
        Ok(path)
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::{AgentError, Identity, ProvisionRequest};
    const fn unsupported() -> AgentError {
        AgentError::UnsupportedOperatingSystem {
            operating_system: std::env::consts::OS,
        }
    }
    pub(super) const fn ensure_supported() -> Result<(), AgentError> {
        Err(unsupported())
    }
    pub(super) fn identity() -> Result<Identity, AgentError> {
        Err(unsupported())
    }
    pub(super) fn provision(_: &ProvisionRequest) -> Result<(), AgentError> {
        Err(unsupported())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn version_has_the_machine_contract() {
        assert_eq!(super::version().name, "cdenv-agent");
        assert!(!super::version().build_id.is_empty());
    }
    #[cfg(target_os = "linux")]
    #[test]
    fn identity_reports_the_effective_linux_user() {
        assert_eq!(
            super::identity().expect("identity").uid,
            nix::unistd::geteuid().as_raw()
        );
    }
}
