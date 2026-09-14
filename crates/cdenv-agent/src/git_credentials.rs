//! Static Git HTTPS helper and reversible process-scoped Git configuration.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{self, Write as _};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use cdenv_core::credential_protocol::{
    CredentialHelperOperation, CredentialProtocolError, GitCredentialRequest, GitCredentialResponse,
};
use cdenv_core::credentials::HttpsOrigin;
use thiserror::Error;
use zeroize::Zeroizing;

/// Name of the cdenv-owned Git configuration fragment.
pub const GIT_INTEGRATION_CONFIG_NAME: &str = "git-credentials.gitconfig";
/// Deadline for connecting to and exchanging one helper request with the private bridge.
pub const GIT_HELPER_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_GIT_CONFIG_PARAMETERS: usize = 1024;

/// Runs the installed lookup-only Git credential helper.
///
/// `store` and `erase` return before reading stdin or opening the bridge. `get` bounds and
/// validates the complete request before connecting, and validates the response again before
/// writing only supported fields to stdout.
///
/// # Errors
///
/// Returns only value-free operation, protocol, or private transport errors.
pub fn run_git_credential_helper(
    socket: &Path,
    operation: &OsStr,
) -> Result<(), GitCredentialHelperError> {
    let operation = operation
        .to_str()
        .ok_or(CredentialProtocolError::Operation)?
        .parse::<CredentialHelperOperation>()?;
    if matches!(
        operation,
        CredentialHelperOperation::Store | CredentialHelperOperation::Erase
    ) {
        return Ok(());
    }

    let body = cdenv_core::credential_protocol::read_private_body(&mut io::stdin().lock())?;
    let request = GitCredentialRequest::parse(&body)?;
    let mut stream = UnixStream::connect(socket).map_err(|_| GitCredentialHelperError::Bridge)?;
    stream
        .set_read_timeout(Some(GIT_HELPER_TIMEOUT))
        .map_err(|_| GitCredentialHelperError::Bridge)?;
    stream
        .set_write_timeout(Some(GIT_HELPER_TIMEOUT))
        .map_err(|_| GitCredentialHelperError::Bridge)?;
    stream
        .write_all(&body)
        .map_err(|_| GitCredentialHelperError::Bridge)?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|_| GitCredentialHelperError::Bridge)?;
    let result = cdenv_core::credential_protocol::read_private_body(&mut stream)?;
    let response = GitCredentialResponse::parse(&result, &request, unix_time())?
        .ok_or(GitCredentialHelperError::Unavailable)?;
    if !response.is_current(unix_time()) {
        return Err(GitCredentialHelperError::Unavailable);
    }
    response
        .write_private(&mut io::stdout().lock())
        .map_err(|_| GitCredentialHelperError::Output)
}

fn unix_time() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A cdenv-owned, nonsecret Git integration fragment outside the checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManagedGitCredentialIntegration {
    config_path: PathBuf,
}

impl ManagedGitCredentialIntegration {
    /// Atomically creates or refreshes the exact granted-origin integration.
    ///
    /// Existing Git files are never read or modified. The generated sections reset the effective
    /// helper list only for each normalized granted origin and enable path-sensitive lookups.
    ///
    /// # Errors
    ///
    /// Rejects unsafe paths and reports private owned-file failures without configuration values.
    pub fn refresh(
        directory: &Path,
        helper_executable: &Path,
        credential_socket: &Path,
        origins: &[HttpsOrigin],
    ) -> Result<Self, GitIntegrationError> {
        validate_absolute_path(helper_executable)?;
        validate_absolute_path(credential_socket)?;
        prepare_owned_directory(directory)?;

        let mut sorted = origins.to_vec();
        sorted.sort();
        sorted.dedup();
        let command = format!(
            "!{} git-credential-helper {}",
            shell_quote(helper_executable)?,
            shell_quote(credential_socket)?
        );
        let mut contents = Zeroizing::new(Vec::new());
        for origin in sorted {
            writeln!(
                &mut contents,
                "[credential \"{}\"]",
                config_escape(origin.as_str())
            )
            .map_err(|_| GitIntegrationError::Io)?;
            writeln!(&mut contents, "\thelper =").map_err(|_| GitIntegrationError::Io)?;
            writeln!(&mut contents, "\thelper = {}", config_escape(&command))
                .map_err(|_| GitIntegrationError::Io)?;
            writeln!(&mut contents, "\tuseHttpPath = true").map_err(|_| GitIntegrationError::Io)?;
        }

        let config_path = directory.join(GIT_INTEGRATION_CONFIG_NAME);
        atomic_write(&config_path, &contents)?;
        Ok(Self { config_path })
    }

    /// Borrows the only persisted integration path.
    #[must_use]
    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// Appends this fragment to existing Git command-environment configuration.
    ///
    /// All existing variables and counts are retained verbatim. Git processes load their normal
    /// system, both global locations, includes, and repository configuration before this final
    /// narrowly-scoped include.
    ///
    /// # Errors
    ///
    /// Rejects malformed or unreasonably large existing `GIT_CONFIG_COUNT` state.
    pub fn enroll_environment(
        &self,
        environment: &mut BTreeMap<OsString, OsString>,
    ) -> Result<(), GitIntegrationError> {
        let count = match environment.get(OsStr::new("GIT_CONFIG_COUNT")) {
            Some(value) => value
                .to_str()
                .and_then(|value| value.parse::<usize>().ok())
                .filter(|count| *count < MAX_GIT_CONFIG_PARAMETERS)
                .ok_or(GitIntegrationError::GitEnvironment)?,
            None => 0,
        };
        let key = OsString::from(format!("GIT_CONFIG_KEY_{count}"));
        let value = OsString::from(format!("GIT_CONFIG_VALUE_{count}"));
        if environment.contains_key(&key) || environment.contains_key(&value) {
            return Err(GitIntegrationError::GitEnvironment);
        }
        environment.insert(key, OsString::from("include.path"));
        environment.insert(value, self.config_path.clone().into_os_string());
        environment.insert(
            OsString::from("GIT_CONFIG_COUNT"),
            OsString::from((count + 1).to_string()),
        );
        Ok(())
    }

    /// Enrolls a standard child command without disturbing any other environment entry.
    ///
    /// # Errors
    ///
    /// Returns malformed preexisting Git command-environment state.
    pub fn enroll_command(&self, command: &mut Command) -> Result<(), GitIntegrationError> {
        let mut git_environment = std::env::vars_os()
            .filter(|(name, _)| {
                let name = name.to_string_lossy();
                name == "GIT_CONFIG_COUNT"
                    || name.starts_with("GIT_CONFIG_KEY_")
                    || name.starts_with("GIT_CONFIG_VALUE_")
            })
            .collect::<BTreeMap<_, _>>();
        self.enroll_environment(&mut git_environment)?;
        command.envs(git_environment);
        Ok(())
    }

    /// Removes only the verified cdenv-owned fragment. Underlying Git behavior is untouched.
    ///
    /// # Errors
    ///
    /// Rejects a replaced/symlinked path and reports owned-file removal failures.
    pub fn remove(&self) -> Result<(), GitIntegrationError> {
        match fs::symlink_metadata(&self.config_path) {
            Ok(metadata)
                if metadata.is_file()
                    && !metadata.file_type().is_symlink()
                    && metadata.uid() == nix::unistd::geteuid().as_raw()
                    && metadata.permissions().mode() & 0o777 == 0o600 =>
            {
                fs::remove_file(&self.config_path).map_err(|_| GitIntegrationError::Io)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Ok(_) => Err(GitIntegrationError::UnsafePath),
            Err(_) => Err(GitIntegrationError::Io),
        }
    }
}

fn prepare_owned_directory(path: &Path) -> Result<(), GitIntegrationError> {
    if !path.is_absolute() {
        return Err(GitIntegrationError::UnsafePath);
    }
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && metadata.uid() == nix::unistd::geteuid().as_raw()
                && metadata.permissions().mode() & 0o777 == 0o700 =>
        {
            Ok(())
        }
        Ok(_) => Err(GitIntegrationError::UnsafePath),
        Err(error) if error.kind() == io::ErrorKind::NotFound => fs::DirBuilder::new()
            .mode(0o700)
            .create(path)
            .map_err(|_| GitIntegrationError::Io),
        Err(_) => Err(GitIntegrationError::Io),
    }
}

fn validate_absolute_path(path: &Path) -> Result<(), GitIntegrationError> {
    let value = path.to_str().ok_or(GitIntegrationError::UnsafePath)?;
    if !path.is_absolute() || value.contains(['\n', '\r', '\0']) {
        return Err(GitIntegrationError::UnsafePath);
    }
    Ok(())
}

fn shell_quote(path: &Path) -> Result<String, GitIntegrationError> {
    validate_absolute_path(path)?;
    Ok(format!(
        "'{}'",
        path.to_string_lossy().replace('\'', "'\\''")
    ))
}

fn config_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), GitIntegrationError> {
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|_| GitIntegrationError::Io)?;
    let result = (|| {
        file.write_all(contents)
            .map_err(|_| GitIntegrationError::Io)?;
        file.sync_all().map_err(|_| GitIntegrationError::Io)?;
        fs::rename(&temporary, path).map_err(|_| GitIntegrationError::Io)
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

/// Static helper failure with no request, response, origin, account, or token values.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GitCredentialHelperError {
    /// Input or output violated the supported V1 helper surface.
    #[error(transparent)]
    Protocol(#[from] CredentialProtocolError),
    /// The private bridge was absent, disconnected, or timed out.
    #[error("cdenv Git credential bridge is unavailable")]
    Bridge,
    /// The authorized host backend returned no supported credential.
    #[error("cdenv host Git credential lookup is unavailable")]
    Unavailable,
    /// Supported credential fields could not be written to Git.
    #[error("cannot write Git credential result")]
    Output,
}

/// Owned integration failure that never includes a path or configuration value.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GitIntegrationError {
    /// An integration path was relative, replaced, or had unsafe ownership/type/mode.
    #[error("unsafe cdenv Git integration path")]
    UnsafePath,
    /// Existing `GIT_CONFIG_COUNT` state was malformed or conflicting.
    #[error("invalid existing Git command-environment configuration")]
    GitEnvironment,
    /// A cdenv-owned integration filesystem operation failed.
    #[error("cannot update cdenv Git integration")]
    Io,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn enrollment_appends_without_replacing_existing_git_environment() {
        let temporary = tempfile::tempdir().expect("temporary");
        let directory = temporary.path().join("integration");
        let integration = ManagedGitCredentialIntegration::refresh(
            &directory,
            Path::new("/opt/cdenv agent/bin"),
            Path::new("/run/user/1000/cdenv socket"),
            &[HttpsOrigin::parse("https://example.com").expect("origin")],
        )
        .expect("integration");
        let mut environment = BTreeMap::from([
            (OsString::from("GIT_CONFIG_COUNT"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_KEY_0"),
                OsString::from("core.editor"),
            ),
            (
                OsString::from("GIT_CONFIG_VALUE_0"),
                OsString::from("native"),
            ),
            (OsString::from("OTHER"), OsString::from("preserved")),
        ]);
        integration
            .enroll_environment(&mut environment)
            .expect("enroll");
        assert_eq!(
            environment.get(OsStr::new("GIT_CONFIG_COUNT")),
            Some(&OsString::from("2"))
        );
        assert_eq!(
            environment.get(OsStr::new("GIT_CONFIG_KEY_0")),
            Some(&OsString::from("core.editor"))
        );
        assert_eq!(
            environment.get(OsStr::new("GIT_CONFIG_KEY_1")),
            Some(&OsString::from("include.path"))
        );
        assert_eq!(
            environment.get(OsStr::new("OTHER")),
            Some(&OsString::from("preserved"))
        );
        integration.remove().expect("remove");
        assert!(!integration.config_path().exists());
    }

    #[test]
    fn fragment_is_origin_scoped_resets_helpers_and_quotes_paths_as_data() {
        let temporary = tempfile::tempdir().expect("temporary");
        let directory = temporary.path().join("integration");
        let integration = ManagedGitCredentialIntegration::refresh(
            &directory,
            Path::new("/opt/cdenv's agent"),
            Path::new("/run/cdenv socket"),
            &[
                HttpsOrigin::parse("https://other.example:8443").expect("other"),
                HttpsOrigin::parse("https://example.com").expect("origin"),
            ],
        )
        .expect("integration");
        let contents = fs::read_to_string(integration.config_path()).expect("config");
        assert_eq!(contents.matches("\thelper =\n").count(), 2);
        assert_eq!(contents.matches("\tuseHttpPath = true\n").count(), 2);
        assert!(contents.contains("https://example.com:443"));
        assert!(contents.contains("'\\\\''"));
        assert!(!contents.contains("password="));
        assert_eq!(
            fs::metadata(integration.config_path())
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_existing_count_and_replaced_fragment_fail_closed() {
        let temporary = tempfile::tempdir().expect("temporary");
        let directory = temporary.path().join("integration");
        let integration = ManagedGitCredentialIntegration::refresh(
            &directory,
            Path::new("/agent"),
            Path::new("/socket"),
            &[],
        )
        .expect("integration");
        let mut environment =
            BTreeMap::from([(OsString::from("GIT_CONFIG_COUNT"), OsString::from("bad"))]);
        assert_eq!(
            integration.enroll_environment(&mut environment),
            Err(GitIntegrationError::GitEnvironment)
        );
        fs::remove_file(integration.config_path()).expect("remove");
        fs::create_dir(integration.config_path()).expect("replace");
        assert_eq!(integration.remove(), Err(GitIntegrationError::UnsafePath));
    }

    #[test]
    fn store_erase_and_unknown_operations_never_open_or_read_input() {
        let missing = Path::new("/definitely/missing/cdenv.sock");
        assert!(run_git_credential_helper(missing, OsStr::new("store")).is_ok());
        assert!(run_git_credential_helper(missing, OsStr::new("erase")).is_ok());
        assert!(matches!(
            run_git_credential_helper(missing, OsStr::new("approve")),
            Err(GitCredentialHelperError::Protocol(
                CredentialProtocolError::Operation
            ))
        ));
    }
}
