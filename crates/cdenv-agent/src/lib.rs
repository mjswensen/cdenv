//! Testable container-side boundary for `cdenv-agent`.
//!
//! Distribution targets are Linux. Platform-specific process and filesystem
//! behavior remains isolated behind compile-time platform modules.

use serde::{Deserialize, Serialize};
use thiserror::Error;

mod environment;
mod forwarding;
#[cfg(target_os = "linux")]
mod lifecycle;

pub use environment::{
    EnvironmentCaptureRequest, EnvironmentCaptureResult, EnvironmentError, EnvironmentProbe,
    EnvironmentSnapshot, EnvironmentTemplate, EnvironmentTemplateSegment, capture_environment,
    emit_current_environment, run_with_environment,
};
pub use forwarding::{
    ForwardTarget, ForwardingError, MAXIMUM_FORWARD_HOST_BYTES, bridge_forwarding_stream,
    verify_forwarding_identity,
};
#[cfg(target_os = "linux")]
pub use lifecycle::{
    LifecycleCommand, LifecycleError, LifecycleInspection, LifecyclePhase, LifecycleProcess,
    LifecycleRunRequest, LifecycleRunnerState, LifecycleStage, LifecycleStagePlan, LifecycleStdin,
    LifecycleValue, LifecycleValueSegment, cancel_lifecycle, execute_lifecycle_command,
    inspect_lifecycle, run_lifecycle,
};

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
    /// UID owning installed private assets.
    pub uid: u32,
    /// GID owning installed private assets.
    pub gid: u32,
    /// Operation-owned staging directory removed on every return path.
    pub staging_directory: String,
    /// Staged agent executable below `stagingDirectory`.
    pub agent_source: String,
    /// Ordered secure executable destinations attempted by the agent.
    pub agent_destinations: Vec<String>,
    /// Private assets to atomically install for the selected user.
    pub files: Vec<ProvisionFile>,
}

/// Successful root provisioning result emitted as typed JSON.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ProvisionResult {
    /// Actual executable path selected from the ordered candidates.
    pub agent_path: String,
}

/// Root-only named-account rewrite used while building a derived image.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateUserRequest {
    /// Existing non-root account name.
    pub account: String,
    /// Host UID applied only to the derived image.
    pub uid: u32,
    /// Host GID applied only to the derived image.
    pub gid: u32,
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
    /// A staged source was not a regular non-symlink file owned by this staging operation.
    #[error("provision source `{path}` is not a regular operation-owned staging file")]
    InvalidSource {
        /// Rejected source path.
        path: String,
    },
    /// A file mode contained special bits or no owner permissions.
    #[error("provision mode {mode:#o} is invalid")]
    InvalidMode {
        /// Rejected Unix permission bits.
        mode: u32,
    },
    /// Manifest destinations must be unique.
    #[error("provision destination `{path}` occurs more than once")]
    DuplicateDestination {
        /// Repeated destination.
        path: String,
    },
    /// No ordered candidate supported a secure executable installation.
    #[error(
        "no secure executable agent location is available; last candidate `{path}` failed: {reason}"
    )]
    NoExecutableLocation {
        /// Last attempted candidate.
        path: String,
        /// Safe OS/filesystem reason.
        reason: String,
    },
    /// Operation-owned staging cleanup failed after otherwise successful provisioning.
    #[error("cannot remove provision staging directory `{path}`: {source}")]
    StagingCleanup {
        /// Staging directory.
        path: String,
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// Provisioning and mandatory staging cleanup both failed.
    #[error("{provision}; staging cleanup for `{path}` also failed: {cleanup}")]
    ProvisionCleanup {
        /// Primary provisioning failure.
        provision: Box<AgentError>,
        /// Staging directory.
        path: String,
        /// Cleanup failure.
        cleanup: std::io::Error,
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
    /// The requested account does not exist.
    #[error("cannot update UID/GID: account `{account}` does not exist")]
    MissingUser {
        /// Requested account.
        account: String,
    },
    /// Root must never be rewritten.
    #[error("cannot update UID/GID for root")]
    RootAccount,
    /// The account's primary group could not be resolved.
    #[error("cannot update UID/GID: primary group {gid} for `{account}` does not exist")]
    MissingPrimaryGroup {
        /// Requested account.
        account: String,
        /// Unresolved group ID.
        gid: u32,
    },
    /// Another account or group owns a requested numeric identity.
    #[error("cannot update UID/GID: {kind} {id} belongs to `{owner}`")]
    IdentityConflict {
        /// `UID` or `GID`.
        kind: &'static str,
        /// Conflicting identity.
        id: u32,
        /// Existing owner.
        owner: String,
    },
    /// The Linux account database was malformed or could not be safely replaced.
    #[error("cannot update Linux account database `{path}`: {message}")]
    AccountDatabase {
        /// `/etc/passwd` or `/etc/group`.
        path: &'static str,
        /// Safe failure detail.
        message: String,
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
pub fn provision(request: &ProvisionRequest) -> Result<ProvisionResult, AgentError> {
    platform::provision(request)
}

/// Removes one validated operation-owned staging directory as root.
///
/// # Errors
///
/// Returns typed platform, permission, path, ownership, or filesystem errors.
pub fn cleanup_staging(path: &str) -> Result<(), AgentError> {
    platform::cleanup_staging(path)
}

/// Rewrites one conflict-free named Linux account without distro utilities.
///
/// # Errors
///
/// Returns typed platform, permission, account, conflict, database, or ownership errors.
pub fn update_user(request: &UpdateUserRequest) -> Result<(), AgentError> {
    platform::update_user(request)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::collections::BTreeSet;
    use std::fs::{self, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    use std::path::{Component, Path, PathBuf};

    use nix::sys::statvfs::{FsFlags, statvfs};
    use nix::unistd::{Gid, Uid, chown, getegid, geteuid};

    use super::{
        AgentError, BUILD_ID, Identity, PROTOCOL_VERSION, ProvisionRequest, ProvisionResult,
        UpdateUserRequest,
    };

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

    pub(super) fn provision(request: &ProvisionRequest) -> Result<ProvisionResult, AgentError> {
        require_root()?;
        let staging = validate_staging_directory(&request.staging_directory)?;
        let cleanup = StagingCleanup::new(staging.clone());
        let result = provision_staged(request, &staging);
        let cleanup_result = cleanup.finish();
        match (result, cleanup_result) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(provision), Err(AgentError::StagingCleanup { path, source })) => {
                Err(AgentError::ProvisionCleanup {
                    provision: Box::new(provision),
                    path,
                    cleanup: source,
                })
            }
            (Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
            (Err(provision), Err(cleanup)) => Err(AgentError::ProvisionCleanup {
                provision: Box::new(provision),
                path: request.staging_directory.clone(),
                cleanup: std::io::Error::other(cleanup),
            }),
        }
    }

    fn provision_staged(
        request: &ProvisionRequest,
        staging: &Path,
    ) -> Result<ProvisionResult, AgentError> {
        if request.build_id != BUILD_ID {
            return Err(AgentError::ManifestMismatch { field: "buildId" });
        }
        if request.protocol_version != PROTOCOL_VERSION {
            return Err(AgentError::ManifestMismatch {
                field: "protocolVersion",
            });
        }
        if request.agent_destinations.is_empty() {
            return Err(AgentError::NoExecutableLocation {
                path: "<none>".to_owned(),
                reason: "manifest supplied no candidates".to_owned(),
            });
        }
        validate_mode(0o555)?;
        let agent_source = validate_staged_source(&request.agent_source, staging)?;
        let mut destinations = BTreeSet::new();
        for file in &request.files {
            validate_mode(file.mode)?;
            validate_staged_source(&file.source, staging)?;
            let destination = safe_path(&file.destination, "destination")?;
            if !destinations.insert(destination.to_path_buf()) {
                return Err(AgentError::DuplicateDestination {
                    path: file.destination.clone(),
                });
            }
        }

        let mut last_failure = None;
        let mut selected = None;
        for destination in &request.agent_destinations {
            let destination = safe_path(destination, "destination")?;
            if !destinations.insert(destination.to_path_buf()) {
                return Err(AgentError::DuplicateDestination {
                    path: destination.display().to_string(),
                });
            }
            match install_agent(agent_source, destination) {
                Ok(()) => {
                    selected = Some(destination.to_path_buf());
                    break;
                }
                Err(error) => {
                    last_failure = Some((destination.to_path_buf(), error.to_string()));
                }
            }
        }
        let agent_path = selected.ok_or_else(|| {
            let (path, reason) = last_failure.unwrap_or_else(|| {
                (
                    PathBuf::from("<none>"),
                    "manifest supplied no candidates".to_owned(),
                )
            });
            AgentError::NoExecutableLocation {
                path: path.display().to_string(),
                reason,
            }
        })?;

        for file in &request.files {
            install(
                validate_staged_source(&file.source, staging)?,
                safe_path(&file.destination, "destination")?,
                file.mode,
                request.uid,
                request.gid,
            )?;
        }
        Ok(ProvisionResult {
            agent_path: agent_path.display().to_string(),
        })
    }

    pub(super) fn cleanup_staging(value: &str) -> Result<(), AgentError> {
        require_root()?;
        let path = validate_staging_directory(value)?;
        fs::remove_dir_all(&path).map_err(|source| AgentError::StagingCleanup {
            path: path.display().to_string(),
            source,
        })
    }

    pub(super) fn update_user(request: &UpdateUserRequest) -> Result<(), AgentError> {
        require_root()?;
        if request.account == "root" || request.uid == 0 {
            return Err(AgentError::RootAccount);
        }
        let passwd = read_database("/etc/passwd")?;
        let group = read_database("/etc/group")?;
        let account = find_account(&passwd, &request.account)?;
        if account.uid == 0 {
            return Err(AgentError::RootAccount);
        }
        reject_uid_conflict(&passwd, request, account.uid)?;
        let primary_group = find_primary_group(&group, &request.account, account.gid)?;
        reject_gid_conflict(&group, request, &primary_group)?;
        if account.uid == request.uid && account.gid == request.gid {
            return Ok(());
        }
        let home = safe_account_home(&account.home)?;
        let updated_passwd = rewrite_passwd(&passwd, request)?;
        let updated_group = rewrite_group(&group, &primary_group, request.gid)?;
        replace_database("/etc/group", updated_group.as_bytes())?;
        replace_database("/etc/passwd", updated_passwd.as_bytes())?;
        chown_tree(home, account.uid, account.gid, request.uid, request.gid)?;
        Ok(())
    }

    #[derive(Clone)]
    struct Account {
        uid: u32,
        gid: u32,
        home: String,
    }

    fn require_root() -> Result<(), AgentError> {
        if geteuid().as_raw() == 0 {
            Ok(())
        } else {
            Err(AgentError::NotRoot)
        }
    }

    fn read_database(path: &'static str) -> Result<String, AgentError> {
        fs::read_to_string(path).map_err(|source| AgentError::AccountDatabase {
            path,
            message: source.to_string(),
        })
    }

    fn fields<'a>(
        line: &'a str,
        path: &'static str,
        minimum: usize,
    ) -> Result<Vec<&'a str>, AgentError> {
        let fields = line.split(':').collect::<Vec<_>>();
        if fields.len() < minimum {
            Err(AgentError::AccountDatabase {
                path,
                message: "malformed entry".to_owned(),
            })
        } else {
            Ok(fields)
        }
    }

    fn find_account(passwd: &str, name: &str) -> Result<Account, AgentError> {
        for line in passwd.lines().filter(|line| !line.is_empty()) {
            let entry = fields(line, "/etc/passwd", 7)?;
            if entry[0] == name {
                return Ok(Account {
                    uid: entry[2].parse().map_err(|_| AgentError::AccountDatabase {
                        path: "/etc/passwd",
                        message: "invalid UID".to_owned(),
                    })?,
                    gid: entry[3].parse().map_err(|_| AgentError::AccountDatabase {
                        path: "/etc/passwd",
                        message: "invalid GID".to_owned(),
                    })?,
                    home: entry[5].to_owned(),
                });
            }
        }
        Err(AgentError::MissingUser {
            account: name.to_owned(),
        })
    }

    fn reject_uid_conflict(
        passwd: &str,
        request: &UpdateUserRequest,
        current_uid: u32,
    ) -> Result<(), AgentError> {
        if request.uid == current_uid {
            return Ok(());
        }
        for line in passwd.lines().filter(|line| !line.is_empty()) {
            let entry = fields(line, "/etc/passwd", 7)?;
            if entry[0] != request.account && entry[2].parse::<u32>().ok() == Some(request.uid) {
                return Err(AgentError::IdentityConflict {
                    kind: "UID",
                    id: request.uid,
                    owner: entry[0].to_owned(),
                });
            }
        }
        Ok(())
    }

    fn find_primary_group(group: &str, account: &str, gid: u32) -> Result<String, AgentError> {
        for line in group.lines().filter(|line| !line.is_empty()) {
            let entry = fields(line, "/etc/group", 4)?;
            if entry[2].parse::<u32>().ok() == Some(gid) {
                return Ok(entry[0].to_owned());
            }
        }
        Err(AgentError::MissingPrimaryGroup {
            account: account.to_owned(),
            gid,
        })
    }

    fn reject_gid_conflict(
        group: &str,
        request: &UpdateUserRequest,
        primary_group: &str,
    ) -> Result<(), AgentError> {
        for line in group.lines().filter(|line| !line.is_empty()) {
            let entry = fields(line, "/etc/group", 4)?;
            if entry[0] != primary_group && entry[2].parse::<u32>().ok() == Some(request.gid) {
                return Err(AgentError::IdentityConflict {
                    kind: "GID",
                    id: request.gid,
                    owner: entry[0].to_owned(),
                });
            }
        }
        Ok(())
    }

    fn rewrite_passwd(passwd: &str, request: &UpdateUserRequest) -> Result<String, AgentError> {
        rewrite_database(passwd, "/etc/passwd", 7, |entry| {
            if entry[0] == request.account {
                entry[2] = request.uid.to_string();
                entry[3] = request.gid.to_string();
            }
        })
    }

    fn rewrite_group(group: &str, name: &str, gid: u32) -> Result<String, AgentError> {
        rewrite_database(group, "/etc/group", 4, |entry| {
            if entry[0] == name {
                entry[2] = gid.to_string();
            }
        })
    }

    fn rewrite_database(
        contents: &str,
        path: &'static str,
        minimum: usize,
        mut update: impl FnMut(&mut [String]),
    ) -> Result<String, AgentError> {
        let mut output = String::with_capacity(contents.len());
        for line in contents.lines() {
            if line.is_empty() {
                output.push('\n');
                continue;
            }
            let parsed = fields(line, path, minimum)?;
            let mut entry = parsed.into_iter().map(str::to_owned).collect::<Vec<_>>();
            update(&mut entry);
            output.push_str(&entry.join(":"));
            output.push('\n');
        }
        Ok(output)
    }

    fn safe_account_home(value: &str) -> Result<&Path, AgentError> {
        let path = Path::new(value);
        if path == Path::new("/")
            || !path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::CurDir | Component::Prefix(_)
                )
            })
        {
            return Err(AgentError::AccountDatabase {
                path: "/etc/passwd",
                message: "account home cannot be safely re-owned".to_owned(),
            });
        }
        Ok(path)
    }

    fn replace_database(path: &'static str, contents: &[u8]) -> Result<(), AgentError> {
        let target = Path::new(path);
        let temporary = target.with_extension(format!("cdenv-{}", std::process::id()));
        let result = (|| {
            let metadata = fs::metadata(target)?;
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?;
            file.set_permissions(metadata.permissions())?;
            file.write_all(contents)?;
            file.sync_all()?;
            fs::rename(&temporary, target)
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(AgentError::AccountDatabase {
                path,
                message: source.to_string(),
            });
        }
        Ok(())
    }

    #[expect(
        clippy::similar_names,
        reason = "UID and GID are the conventional paired Linux account terms"
    )]
    fn chown_tree(
        path: &Path,
        old_uid: u32,
        old_gid: u32,
        new_uid: u32,
        new_gid: u32,
    ) -> Result<(), AgentError> {
        let metadata =
            fs::symlink_metadata(path).map_err(|source| AgentError::AccountDatabase {
                path: "/etc/passwd",
                message: format!("cannot inspect account home: {source}"),
            })?;
        if metadata.file_type().is_symlink() {
            return Ok(());
        }
        let uid = (metadata.uid() == old_uid).then(|| Uid::from_raw(new_uid));
        let gid = (metadata.gid() == old_gid).then(|| Gid::from_raw(new_gid));
        if uid.is_some() || gid.is_some() {
            chown(path, uid, gid).map_err(|source| AgentError::Ownership {
                path: path.display().to_string(),
                source,
            })?;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path).map_err(|source| AgentError::AccountDatabase {
                path: "/etc/passwd",
                message: format!("cannot read account home: {source}"),
            })? {
                let entry = entry.map_err(|source| AgentError::AccountDatabase {
                    path: "/etc/passwd",
                    message: format!("cannot read account home: {source}"),
                })?;
                chown_tree(&entry.path(), old_uid, old_gid, new_uid, new_gid)?;
            }
        }
        Ok(())
    }

    fn install_agent(source: &Path, destination: &Path) -> Result<(), AgentError> {
        let parent = prepare_parent(destination, 0, 0, 0o755)?;
        let flags = statvfs(parent).map_err(|source| AgentError::Install {
            path: destination.display().to_string(),
            source: std::io::Error::from_raw_os_error(source as i32),
        })?;
        if flags.flags().contains(FsFlags::ST_NOEXEC) {
            return Err(AgentError::Install {
                path: destination.display().to_string(),
                source: std::io::Error::from(std::io::ErrorKind::PermissionDenied),
            });
        }
        install(source, destination, 0o555, 0, 0)
    }

    fn install(
        source: &Path,
        destination: &Path,
        mode: u32,
        uid: u32,
        gid: u32,
    ) -> Result<(), AgentError> {
        validate_mode(mode)?;
        let parent_mode = if mode.trailing_zeros() >= 6 {
            0o700
        } else {
            0o755
        };
        let parent = prepare_parent(destination, uid, gid, parent_mode)?;
        let temporary = parent.join(format!(
            ".cdenv-install-{}-{}",
            std::process::id(),
            destination
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
        ));
        let _temporary_cleanup = TemporaryCleanup(temporary.clone());
        let mut input = OpenOptions::new()
            .read(true)
            .open(source)
            .map_err(|source| AgentError::Install {
                path: destination.display().to_string(),
                source,
            })?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| AgentError::Install {
                path: destination.display().to_string(),
                source,
            })?;
        std::io::copy(&mut input, &mut output).map_err(|source| AgentError::Install {
            path: destination.display().to_string(),
            source,
        })?;
        output.sync_all().map_err(|source| AgentError::Install {
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

    fn prepare_parent(
        destination: &Path,
        uid: u32,
        gid: u32,
        mode: u32,
    ) -> Result<&Path, AgentError> {
        let parent = destination.parent().ok_or_else(|| AgentError::UnsafePath {
            kind: "destination",
            path: destination.display().to_string(),
        })?;
        reject_symlink_components(parent)?;
        fs::create_dir_all(parent).map_err(|source| AgentError::PrepareDestination {
            path: parent.display().to_string(),
            source,
        })?;
        reject_symlink_components(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(mode)).map_err(|source| {
            AgentError::PrepareDestination {
                path: parent.display().to_string(),
                source,
            }
        })?;
        chown(parent, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid))).map_err(|source| {
            AgentError::Ownership {
                path: parent.display().to_string(),
                source,
            }
        })?;
        Ok(parent)
    }

    fn reject_symlink_components(path: &Path) -> Result<(), AgentError> {
        let mut current = PathBuf::from("/");
        for component in path.components().skip(1) {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(AgentError::UnsafePath {
                        kind: "destination",
                        path: current.display().to_string(),
                    });
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(source) => {
                    return Err(AgentError::PrepareDestination {
                        path: current.display().to_string(),
                        source,
                    });
                }
            }
        }
        Ok(())
    }

    fn validate_mode(mode: u32) -> Result<(), AgentError> {
        if mode == 0 || mode & !0o777 != 0 || mode & 0o700 == 0 {
            Err(AgentError::InvalidMode { mode })
        } else {
            Ok(())
        }
    }

    fn validate_staging_directory(value: &str) -> Result<PathBuf, AgentError> {
        let path = safe_path(value, "staging")?;
        let valid_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| {
                name.starts_with(".cdenv-stage-") && name.len() > ".cdenv-stage-".len()
            });
        if !valid_name {
            return Err(AgentError::UnsafePath {
                kind: "staging",
                path: value.to_owned(),
            });
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| AgentError::InvalidSource {
            path: value.to_owned(),
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != 0 {
            return Err(AgentError::InvalidSource {
                path: value.to_owned(),
            });
        }
        Ok(path.to_path_buf())
    }

    fn validate_staged_source<'a>(value: &'a str, staging: &Path) -> Result<&'a Path, AgentError> {
        let path = safe_path(value, "source")?;
        if path.parent() != Some(staging) {
            return Err(AgentError::InvalidSource {
                path: value.to_owned(),
            });
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| AgentError::InvalidSource {
            path: value.to_owned(),
        })?;
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.uid() != 0 {
            return Err(AgentError::InvalidSource {
                path: value.to_owned(),
            });
        }
        Ok(path)
    }

    struct TemporaryCleanup(PathBuf);

    impl Drop for TemporaryCleanup {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    struct StagingCleanup(Option<PathBuf>);

    impl StagingCleanup {
        fn new(path: PathBuf) -> Self {
            Self(Some(path))
        }

        fn finish(mut self) -> Result<(), AgentError> {
            let Some(path) = self.0.take() else {
                return Ok(());
            };
            fs::remove_dir_all(&path).map_err(|source| AgentError::StagingCleanup {
                path: path.display().to_string(),
                source,
            })
        }
    }

    impl Drop for StagingCleanup {
        fn drop(&mut self) {
            if let Some(path) = self.0.take() {
                let _ = fs::remove_dir_all(path);
            }
        }
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

    #[cfg(test)]
    mod tests {
        use super::*;

        const PASSWD: &str = "root:x:0:0:root:/root:/bin/sh\ndev:x:1000:1000::/home/dev:/bin/sh\n";
        const GROUP: &str = "root:x:0:\ndev:x:1000:\n";

        #[test]
        fn account_rewrite_changes_only_the_selected_numeric_fields() {
            let request = UpdateUserRequest {
                account: "dev".to_owned(),
                uid: 501,
                gid: 20,
            };
            assert_eq!(
                rewrite_passwd(PASSWD, &request).expect("passwd"),
                "root:x:0:0:root:/root:/bin/sh\ndev:x:501:20::/home/dev:/bin/sh\n"
            );
            assert_eq!(
                rewrite_group(GROUP, "dev", 20).expect("group"),
                "root:x:0:\ndev:x:20:\n"
            );
        }

        #[test]
        fn account_validation_rejects_conflicts_and_root_home() {
            let request = UpdateUserRequest {
                account: "dev".to_owned(),
                uid: 0,
                gid: 20,
            };
            assert!(matches!(
                reject_uid_conflict(PASSWD, &request, 1000),
                Err(AgentError::IdentityConflict { kind: "UID", .. })
            ));
            assert!(safe_account_home("/").is_err());
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::{AgentError, Identity, ProvisionRequest, ProvisionResult, UpdateUserRequest};
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
    pub(super) fn provision(_: &ProvisionRequest) -> Result<ProvisionResult, AgentError> {
        Err(unsupported())
    }
    pub(super) fn cleanup_staging(_: &str) -> Result<(), AgentError> {
        Err(unsupported())
    }
    pub(super) fn update_user(_: &UpdateUserRequest) -> Result<(), AgentError> {
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

    #[test]
    fn provision_manifest_requires_operation_owned_staging() {
        let result = serde_json::from_str::<super::ProvisionRequest>(
            r#"{"buildId":"development","protocolVersion":1,"uid":1,"gid":1,"agentSource":"/tmp/agent","agentDestinations":["/tmp/final"],"files":[]}"#,
        );

        assert!(result.is_err());
    }

    #[test]
    fn provision_manifest_rejects_unknown_fields() {
        let result = serde_json::from_str::<super::ProvisionRequest>(
            r#"{"buildId":"development","protocolVersion":1,"uid":1,"gid":1,"stagingDirectory":"/tmp/.cdenv-stage-x","agentSource":"/tmp/.cdenv-stage-x/agent","agentDestinations":["/tmp/final"],"files":[],"unexpected":true}"#,
        );

        assert!(result.is_err());
    }
}
