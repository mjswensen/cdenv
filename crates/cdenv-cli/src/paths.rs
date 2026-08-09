//! Validated host paths and the cdenv-managed filesystem layout.

use std::env;
use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cdenv_core::WorkspaceName;
use thiserror::Error;

/// The environment variable that overrides the default cdenv root.
pub const CDENV_HOME: &str = "CDENV_HOME";

/// The source from which a cdenv root was selected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RootSource {
    /// The global `--root` command-line option.
    CommandLine,
    /// The documented [`CDENV_HOME`] environment override.
    Environment,
    /// The `.cdenv` directory below the user's home directory.
    UserHome,
}

impl fmt::Display for RootSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::CommandLine => "--root",
            Self::Environment => CDENV_HOME,
            Self::UserHome => "user home",
        })
    }
}

/// A failure to select or validate the cdenv root.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RootResolutionError {
    /// No home directory was available for the default root.
    #[error("cannot resolve the default cdenv root because the user home directory is unavailable")]
    MissingHome,
    /// A selected root was not absolute.
    #[error("the cdenv root selected from {selected_from} must be an absolute path: {path:?}")]
    Relative {
        /// The selected precedence source.
        selected_from: RootSource,
        /// The rejected path.
        path: PathBuf,
    },
    /// A selected root could not be represented as UTF-8.
    #[error("the cdenv root selected from {selected_from} must be valid UTF-8")]
    NonUtf8 {
        /// The selected precedence source.
        selected_from: RootSource,
    },
    /// A selected root contains text unsafe for generated OpenSSH configuration.
    #[error(
        "the cdenv root selected from {selected_from} contains an unsafe path character at byte {index}"
    )]
    UnsafeOpenSshCharacter {
        /// The selected precedence source.
        selected_from: RootSource,
        /// Byte offset of the rejected character.
        index: usize,
    },
}

/// An environment boundary used when resolving a root.
///
/// Supplying this boundary keeps tests independent of the process environment
/// and guarantees that root selection happens at application wiring time.
pub trait RootEnvironment {
    /// Returns the raw `CDENV_HOME` value, when present.
    fn cdenv_home(&self) -> Option<OsString>;

    /// Returns the current user's home directory, when available.
    fn home_dir(&self) -> Option<PathBuf>;
}

/// The real process environment used by the cdenv executable.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnvironment;

impl RootEnvironment for ProcessEnvironment {
    fn cdenv_home(&self) -> Option<OsString> {
        env::var_os(CDENV_HOME)
    }

    fn home_dir(&self) -> Option<PathBuf> {
        #[cfg(unix)]
        {
            env::var_os("HOME").map(PathBuf::from)
        }
        #[cfg(windows)]
        {
            env::var_os("USERPROFILE").map(PathBuf::from)
        }
        #[cfg(not(any(unix, windows)))]
        {
            None
        }
    }
}

/// One absolute, UTF-8, OpenSSH-safe root for all cdenv-managed host state.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CdenvRoot(PathBuf);

impl CdenvRoot {
    /// Resolves `--root > CDENV_HOME > ~/.cdenv` using an injectable environment.
    ///
    /// # Errors
    ///
    /// Returns [`RootResolutionError`] if no default home is available or the
    /// selected path is relative, non-UTF-8, or unsafe to embed in OpenSSH
    /// configuration.
    pub fn resolve(
        explicit_root: Option<&Path>,
        environment: &impl RootEnvironment,
    ) -> Result<Self, RootResolutionError> {
        let (path, source) = if let Some(path) = explicit_root {
            (path.to_path_buf(), RootSource::CommandLine)
        } else if let Some(path) = environment.cdenv_home() {
            (PathBuf::from(path), RootSource::Environment)
        } else {
            let home = environment
                .home_dir()
                .ok_or(RootResolutionError::MissingHome)?;
            (home.join(".cdenv"), RootSource::UserHome)
        };

        Self::from_selected(path, source)
    }

    fn from_selected(
        path: PathBuf,
        selected_from: RootSource,
    ) -> Result<Self, RootResolutionError> {
        if !path.is_absolute() {
            return Err(RootResolutionError::Relative {
                selected_from,
                path,
            });
        }
        let text = path
            .to_str()
            .ok_or(RootResolutionError::NonUtf8 { selected_from })?;
        if let Some(index) = unsafe_openssh_character(text) {
            return Err(RootResolutionError::UnsafeOpenSshCharacter {
                selected_from,
                index,
            });
        }
        Ok(Self(path))
    }

    /// Returns the selected root path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Returns `installation.json`.
    #[must_use]
    pub fn installation_file(&self) -> PathBuf {
        self.0.join("installation.json")
    }

    /// Returns the installation fingerprint secret path.
    #[must_use]
    pub fn fingerprint_key(&self) -> PathBuf {
        self.0.join("fingerprint.key")
    }

    /// Returns the managed workspace collection directory.
    #[must_use]
    pub fn workspaces_dir(&self) -> PathBuf {
        self.0.join("workspaces")
    }

    /// Returns the brief global workspace-name reservation lock.
    #[must_use]
    pub fn workspace_namespace_lock(&self) -> PathBuf {
        self.workspaces_dir().join(".namespace.lock")
    }

    /// Derives all paths belonging to one validated workspace name.
    #[must_use]
    pub const fn workspace<'a>(&'a self, name: &'a WorkspaceName) -> WorkspacePaths<'a> {
        WorkspacePaths { root: self, name }
    }

    /// Derives the managed SSH paths.
    #[must_use]
    pub const fn ssh(&self) -> SshPaths<'_> {
        SshPaths { root: self }
    }

    /// Derives the Dev Container artifact-cache paths.
    #[must_use]
    pub const fn cache(&self) -> CachePaths<'_> {
        CachePaths { root: self }
    }

    /// Returns the global managed log directory.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.0.join("logs")
    }

    /// Returns the managed temporary directory.
    #[must_use]
    pub fn temp_dir(&self) -> PathBuf {
        self.0.join("tmp")
    }
}

impl AsRef<Path> for CdenvRoot {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for CdenvRoot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_path().display().fmt(formatter)
    }
}

/// Paths for one workspace, borrowed from a root and validated name.
#[derive(Clone, Copy, Debug)]
pub struct WorkspacePaths<'a> {
    root: &'a CdenvRoot,
    name: &'a WorkspaceName,
}

impl WorkspacePaths<'_> {
    /// Returns `workspaces/<name>`.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.root.workspaces_dir().join(self.name.as_str())
    }

    /// Returns the directory containing the Git checkout.
    #[must_use]
    pub fn checkout_dir(&self) -> PathBuf {
        self.root().join("checkout")
    }

    /// Returns `workspaces/<name>/checkout/<name>`.
    #[must_use]
    pub fn checkout(&self) -> PathBuf {
        self.checkout_dir().join(self.name.as_str())
    }

    /// Returns the persisted workspace state path.
    #[must_use]
    pub fn state_file(&self) -> PathBuf {
        self.root().join("state.json")
    }

    /// Returns the workspace operation lock path.
    #[must_use]
    pub fn lock_file(&self) -> PathBuf {
        self.root().join(".lock")
    }

    /// Returns the private supervisor runtime directory.
    #[must_use]
    pub fn runtime_dir(&self) -> PathBuf {
        self.root().join("runtime")
    }

    /// Returns the private forwarding-supervisor control socket path.
    #[must_use]
    pub fn supervisor_socket(&self) -> PathBuf {
        self.runtime_dir().join("supervisor.sock")
    }

    /// Returns the private forwarding-supervisor identity/state path.
    #[must_use]
    pub fn supervisor_state_file(&self) -> PathBuf {
        self.runtime_dir().join("supervisor.json")
    }

    /// Returns the forwarding-supervisor lifetime lock path.
    #[must_use]
    pub fn supervisor_lifetime_lock(&self) -> PathBuf {
        self.runtime_dir().join(".supervisor.lock")
    }

    /// Returns the workspace log directory.
    #[must_use]
    pub fn logs_dir(&self) -> PathBuf {
        self.root().join("logs")
    }
}

/// Managed OpenSSH configuration and identity paths.
#[derive(Clone, Copy, Debug)]
pub struct SshPaths<'a> {
    root: &'a CdenvRoot,
}

impl SshPaths<'_> {
    /// Returns the managed SSH directory.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.root.as_path().join("ssh")
    }
    /// Returns the generated client configuration.
    #[must_use]
    pub fn config(&self) -> PathBuf {
        self.root().join("config")
    }
    /// Returns the managed known-hosts file.
    #[must_use]
    pub fn known_hosts(&self) -> PathBuf {
        self.root().join("known_hosts")
    }
    /// Returns the client private key.
    #[must_use]
    pub fn private_key(&self) -> PathBuf {
        self.root().join("id_ed25519")
    }
    /// Returns the client public key.
    #[must_use]
    pub fn public_key(&self) -> PathBuf {
        self.root().join("id_ed25519.pub")
    }
    /// Returns the stable per-workspace host-key directory.
    #[must_use]
    pub fn host_keys_dir(&self) -> PathBuf {
        self.root().join("host_keys")
    }
}

/// Managed Dev Container cache paths.
#[derive(Clone, Copy, Debug)]
pub struct CachePaths<'a> {
    root: &'a CdenvRoot,
}

impl CachePaths<'_> {
    /// Returns the global cache directory.
    #[must_use]
    pub fn root(&self) -> PathBuf {
        self.root.as_path().join("cache")
    }
    /// Returns the Dev Container cache directory.
    #[must_use]
    pub fn devcontainer(&self) -> PathBuf {
        self.root().join("devcontainer")
    }
    /// Returns the content-addressed artifact directory.
    #[must_use]
    pub fn blobs_dir(&self) -> PathBuf {
        self.devcontainer().join("blobs")
    }
    /// Returns the bounded generated-build-material directory.
    #[must_use]
    pub fn generated_dir(&self) -> PathBuf {
        self.devcontainer().join("generated")
    }
}

/// A required host path validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RequiredPathError {
    /// The path cannot be represented as UTF-8.
    #[error("required path must be valid UTF-8: {path:?}")]
    NonUtf8 {
        /// The rejected path.
        path: PathBuf,
    },
    /// The path contains an unsafe OpenSSH control or expansion token.
    #[error("required path contains an unsafe OpenSSH character at byte {index}: {path:?}")]
    UnsafeOpenSshCharacter {
        /// The rejected path.
        path: PathBuf,
        /// Byte offset of the rejected character.
        index: usize,
    },
}

/// Validates a required cdenv, executable, checkout, config, or local-source path.
///
/// The path is not canonicalized or accessed. Filesystem containment checks
/// remain the responsibility of the workflow that knows the path's role.
///
/// # Errors
///
/// Returns [`RequiredPathError`] when the path is not valid UTF-8.
pub fn validate_required_path(path: &Path) -> Result<&str, RequiredPathError> {
    path.to_str().ok_or_else(|| RequiredPathError::NonUtf8 {
        path: path.to_path_buf(),
    })
}

/// Validates a required path that will be embedded in OpenSSH configuration.
///
/// # Errors
///
/// Returns [`RequiredPathError`] for non-UTF-8 paths, control characters, or
/// OpenSSH expansion tokens (`%` and `$`).
pub fn validate_openssh_path(path: &Path) -> Result<&str, RequiredPathError> {
    let text = validate_required_path(path)?;
    if let Some(index) = unsafe_openssh_character(text) {
        return Err(RequiredPathError::UnsafeOpenSshCharacter {
            path: path.to_path_buf(),
            index,
        });
    }
    Ok(text)
}

fn unsafe_openssh_character(value: &str) -> Option<usize> {
    value.char_indices().find_map(|(index, character)| {
        (character.is_control() || matches!(character, '%' | '$')).then_some(index)
    })
}

/// The expected kind of an existing managed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedPathKind {
    /// A directory is required.
    Directory,
    /// A regular file is required.
    File,
    /// A Unix-domain socket is required.
    Socket,
}

/// The non-mutating result of inspecting a managed path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManagedPathState {
    /// The path does not exist.
    Missing,
    /// The path exists with the requested kind and ownership.
    Valid,
}

/// A secure managed-path inspection failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManagedPathError {
    /// Metadata could not be read for a reason other than absence.
    #[error("cannot inspect managed path {path:?}: {source}")]
    Inspect {
        /// The path being inspected.
        path: PathBuf,
        /// The preserved operating-system failure.
        #[source]
        source: io::Error,
    },
    /// The managed path itself is a symbolic link.
    #[error("managed path must not be a symbolic link: {path:?}")]
    Symlink {
        /// The rejected path.
        path: PathBuf,
    },
    /// The managed path has a different file kind than required.
    #[error("managed path has the wrong file kind (expected {expected:?}): {path:?}")]
    WrongKind {
        /// The rejected path.
        path: PathBuf,
        /// The required kind.
        expected: ManagedPathKind,
    },
    /// The path belongs to a different Unix user.
    #[error("managed path {path:?} is owned by uid {actual}, expected uid {expected}")]
    OwnershipMismatch {
        /// The rejected path.
        path: PathBuf,
        /// Required owner UID.
        expected: u32,
        /// Observed owner UID.
        actual: u32,
    },
}

/// Inspects one managed path without following symlinks or making repairs.
///
/// `expected_owner` permits callers and tests to apply the installation's Unix
/// ownership policy. On non-Unix hosts it is ignored because standard metadata
/// does not expose a UID.
///
/// # Errors
///
/// Returns [`ManagedPathError`] for metadata I/O failures, symbolic links,
/// wrong file kinds, or a Unix ownership mismatch.
pub fn inspect_managed_path(
    path: &Path,
    expected_kind: ManagedPathKind,
    expected_owner: Option<u32>,
) -> Result<ManagedPathState, ManagedPathError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            return Ok(ManagedPathState::Missing);
        }
        Err(source) => {
            return Err(ManagedPathError::Inspect {
                path: path.to_path_buf(),
                source,
            });
        }
    };

    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Err(ManagedPathError::Symlink {
            path: path.to_path_buf(),
        });
    }
    let kind_matches = match expected_kind {
        ManagedPathKind::Directory => file_type.is_dir(),
        ManagedPathKind::File => file_type.is_file(),
        ManagedPathKind::Socket => is_socket(file_type),
    };
    if !kind_matches {
        return Err(ManagedPathError::WrongKind {
            path: path.to_path_buf(),
            expected: expected_kind,
        });
    }

    inspect_owner(path, &metadata, expected_owner)?;
    Ok(ManagedPathState::Valid)
}

#[cfg(unix)]
fn is_socket(file_type: fs::FileType) -> bool {
    use std::os::unix::fs::FileTypeExt;

    file_type.is_socket()
}

#[cfg(not(unix))]
const fn is_socket(_file_type: fs::FileType) -> bool {
    false
}

#[cfg(unix)]
fn inspect_owner(
    path: &Path,
    metadata: &fs::Metadata,
    expected_owner: Option<u32>,
) -> Result<(), ManagedPathError> {
    use std::os::unix::fs::MetadataExt;

    if let Some(expected) = expected_owner {
        let actual = metadata.uid();
        if actual != expected {
            return Err(ManagedPathError::OwnershipMismatch {
                path: path.to_path_buf(),
                expected,
                actual,
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn inspect_owner(
    _path: &Path,
    _metadata: &fs::Metadata,
    _expected_owner: Option<u32>,
) -> Result<(), ManagedPathError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct FakeEnvironment {
        cdenv_home: Option<OsString>,
        home: Option<PathBuf>,
    }

    impl RootEnvironment for FakeEnvironment {
        fn cdenv_home(&self) -> Option<OsString> {
            self.cdenv_home.clone()
        }
        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }
    }

    fn environment(cdenv_home: Option<&str>, home: Option<&str>) -> FakeEnvironment {
        FakeEnvironment {
            cdenv_home: cdenv_home.map(OsString::from),
            home: home.map(PathBuf::from),
        }
    }

    #[test]
    fn explicit_root_has_highest_precedence() {
        let env = environment(Some("/environment"), Some("/home/test"));
        let root = CdenvRoot::resolve(Some(Path::new("/explicit")), &env)
            .expect("absolute explicit root should resolve");
        assert_eq!(root.as_path(), Path::new("/explicit"));
    }

    #[test]
    fn environment_root_precedes_home() {
        let env = environment(Some("/environment"), Some("/home/test"));
        let root =
            CdenvRoot::resolve(None, &env).expect("absolute environment root should resolve");
        assert_eq!(root.as_path(), Path::new("/environment"));
    }

    #[test]
    fn default_root_is_below_injected_home() {
        let env = environment(None, Some("/isolated/home"));
        let root = CdenvRoot::resolve(None, &env).expect("isolated home should resolve");
        assert_eq!(root.as_path(), Path::new("/isolated/home/.cdenv"));
    }

    #[test]
    fn relative_explicit_root_is_rejected() {
        let env = environment(None, Some("/isolated/home"));
        assert!(matches!(
            CdenvRoot::resolve(Some(Path::new("relative")), &env),
            Err(RootResolutionError::Relative {
                selected_from: RootSource::CommandLine,
                ..
            })
        ));
    }

    #[test]
    fn missing_home_is_reported_without_reading_the_real_home() {
        assert_eq!(
            CdenvRoot::resolve(None, &environment(None, None)),
            Err(RootResolutionError::MissingHome)
        );
    }

    #[test]
    fn layout_matches_the_host_contract() {
        let root = CdenvRoot::resolve(Some(Path::new("/root/.cdenv")), &environment(None, None))
            .expect("test root should resolve");
        let name = WorkspaceName::parse("project").expect("test workspace should be valid");
        let workspace = root.workspace(&name);
        assert_eq!(
            [
                root.installation_file(),
                root.fingerprint_key(),
                root.workspace_namespace_lock(),
                workspace.root(),
                workspace.checkout(),
                workspace.state_file(),
                workspace.lock_file(),
                workspace.runtime_dir(),
                workspace.supervisor_socket(),
                workspace.supervisor_state_file(),
                workspace.supervisor_lifetime_lock(),
                workspace.logs_dir(),
                root.ssh().config(),
                root.ssh().known_hosts(),
                root.ssh().private_key(),
                root.ssh().public_key(),
                root.ssh().host_keys_dir(),
                root.cache().blobs_dir(),
                root.cache().generated_dir(),
                root.logs_dir(),
                root.temp_dir(),
            ],
            [
                PathBuf::from("/root/.cdenv/installation.json"),
                PathBuf::from("/root/.cdenv/fingerprint.key"),
                PathBuf::from("/root/.cdenv/workspaces/.namespace.lock"),
                PathBuf::from("/root/.cdenv/workspaces/project"),
                PathBuf::from("/root/.cdenv/workspaces/project/checkout/project"),
                PathBuf::from("/root/.cdenv/workspaces/project/state.json"),
                PathBuf::from("/root/.cdenv/workspaces/project/.lock"),
                PathBuf::from("/root/.cdenv/workspaces/project/runtime"),
                PathBuf::from("/root/.cdenv/workspaces/project/runtime/supervisor.sock"),
                PathBuf::from("/root/.cdenv/workspaces/project/runtime/supervisor.json"),
                PathBuf::from("/root/.cdenv/workspaces/project/runtime/.supervisor.lock"),
                PathBuf::from("/root/.cdenv/workspaces/project/logs"),
                PathBuf::from("/root/.cdenv/ssh/config"),
                PathBuf::from("/root/.cdenv/ssh/known_hosts"),
                PathBuf::from("/root/.cdenv/ssh/id_ed25519"),
                PathBuf::from("/root/.cdenv/ssh/id_ed25519.pub"),
                PathBuf::from("/root/.cdenv/ssh/host_keys"),
                PathBuf::from("/root/.cdenv/cache/devcontainer/blobs"),
                PathBuf::from("/root/.cdenv/cache/devcontainer/generated"),
                PathBuf::from("/root/.cdenv/logs"),
                PathBuf::from("/root/.cdenv/tmp"),
            ]
        );
    }

    #[test]
    fn openssh_path_rejects_control_and_expansion_characters() {
        assert!(validate_openssh_path(Path::new("/tmp/line\nbreak")).is_err());
        assert!(validate_openssh_path(Path::new("/tmp/%h")).is_err());
    }

    #[test]
    fn ordinary_required_path_only_requires_utf8() {
        assert_eq!(
            validate_required_path(Path::new("/tmp/source-$name")),
            Ok("/tmp/source-$name")
        );
    }

    #[test]
    fn managed_inspection_reports_missing_and_wrong_kind() {
        let temporary = tempfile::tempdir().expect("temporary root should be created");
        let missing = temporary.path().join("missing");
        assert_eq!(
            inspect_managed_path(&missing, ManagedPathKind::Directory, None)
                .expect("absence is valid"),
            ManagedPathState::Missing
        );
        let file = temporary.path().join("file");
        fs::write(&file, b"test").expect("test file should be created");
        assert!(matches!(
            inspect_managed_path(&file, ManagedPathKind::Directory, None),
            Err(ManagedPathError::WrongKind { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn managed_inspection_rejects_symlinks_and_simulated_wrong_owner() {
        use std::os::unix::fs::{MetadataExt, symlink};
        let temporary = tempfile::tempdir().expect("temporary root should be created");
        let directory = temporary.path().join("directory");
        fs::create_dir(&directory).expect("test directory should be created");
        let link = temporary.path().join("link");
        symlink(&directory, &link).expect("test symlink should be created");
        assert!(matches!(
            inspect_managed_path(&link, ManagedPathKind::Directory, None),
            Err(ManagedPathError::Symlink { .. })
        ));
        let actual = fs::metadata(&directory)
            .expect("metadata should exist")
            .uid();
        assert!(matches!(
            inspect_managed_path(
                &directory,
                ManagedPathKind::Directory,
                Some(actual.wrapping_add(1))
            ),
            Err(ManagedPathError::OwnershipMismatch { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_roots_and_required_paths_are_rejected() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let value = OsStr::from_bytes(b"/tmp/root-\xff");
        let env = FakeEnvironment {
            cdenv_home: Some(value.to_os_string()),
            home: None,
        };
        assert!(matches!(
            CdenvRoot::resolve(None, &env),
            Err(RootResolutionError::NonUtf8 { .. })
        ));
        assert!(matches!(
            validate_required_path(Path::new(value)),
            Err(RequiredPathError::NonUtf8 { .. })
        ));
    }
}
