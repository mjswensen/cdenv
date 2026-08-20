//! One local Docker endpoint and command-scoped Docker/Compose capability probes.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bollard::{API_DEFAULT_VERSION, Docker};
use serde_json::Value;
use thiserror::Error;

use crate::{
    CancellationToken, ProcessDeadline, ProcessEnvironmentVariable, ProcessError, ProcessRequest,
    ProcessRunner,
};

/// Minimum verified Docker CLI version.
pub const MINIMUM_DOCKER_CLI: Version = Version::new(29, 7, 1);
/// Minimum verified Docker Engine version.
pub const MINIMUM_DOCKER_ENGINE: Version = Version::new(29, 6, 2);
/// Minimum Docker Engine API capability required by the selected Bollard adapter.
pub const MINIMUM_DOCKER_API: ApiVersion = ApiVersion::new(1, 55);
/// Minimum verified Docker Compose V2 version.
pub const MINIMUM_COMPOSE: Version = Version::new(5, 3, 1);
/// Default bound for Docker dependency and capability probes.
pub const DOCKER_PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Process environment inputs needed to resolve one local Docker endpoint.
pub trait DockerEnvironment {
    /// Returns `DOCKER_HOST` exactly as supplied, when present.
    fn docker_host(&self) -> Option<OsString>;
    /// Returns an explicitly selected Docker context, when present.
    fn docker_context(&self) -> Option<OsString>;
    /// Returns the user's home directory, when available.
    fn home_dir(&self) -> Option<PathBuf>;
    /// Returns `XDG_RUNTIME_DIR`, when available.
    fn runtime_dir(&self) -> Option<PathBuf>;
}

/// Real process Docker environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessDockerEnvironment;

impl DockerEnvironment for ProcessDockerEnvironment {
    fn docker_host(&self) -> Option<OsString> {
        env::var_os("DOCKER_HOST")
    }

    fn docker_context(&self) -> Option<OsString> {
        env::var_os("DOCKER_CONTEXT")
    }

    fn home_dir(&self) -> Option<PathBuf> {
        env::var_os("HOME").map(PathBuf::from)
    }

    fn runtime_dir(&self) -> Option<PathBuf> {
        env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
    }
}

/// Static-dispatch test seam for Unix socket discovery.
pub trait DockerSocketProbe {
    /// Reports whether a path currently names a Unix-domain socket.
    fn is_unix_socket(&self, path: &Path) -> bool;
}

/// Real filesystem Unix socket probe.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileSystemDockerSocketProbe;

impl DockerSocketProbe for FileSystemDockerSocketProbe {
    fn is_unix_socket(&self, path: &Path) -> bool {
        is_socket(path)
    }
}

/// One validated local Unix-domain Docker endpoint.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DockerEndpoint {
    socket: PathBuf,
    docker_host: OsString,
}

impl DockerEndpoint {
    /// Resolves `DOCKER_HOST` or the documented Docker Desktop, rootless, and default paths.
    ///
    /// # Errors
    ///
    /// Rejects remote/unsupported configuration, invalid or unavailable configured sockets, and
    /// hosts on which none of the known local Unix sockets exists.
    pub fn resolve(environment: &impl DockerEnvironment) -> Result<Self, DockerEndpointError> {
        Self::resolve_with_probe(environment, &FileSystemDockerSocketProbe)
    }

    /// Resolves through an injected socket probe without dynamic dispatch.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::resolve`].
    pub fn resolve_with_probe(
        environment: &impl DockerEnvironment,
        probe: &impl DockerSocketProbe,
    ) -> Result<Self, DockerEndpointError> {
        if let Some(configured) = environment.docker_host() {
            let text = configured
                .to_str()
                .ok_or(DockerEndpointError::NonUtf8DockerHost)?;
            let socket = parse_unix_docker_host(text)?;
            if !probe.is_unix_socket(&socket) {
                return Err(DockerEndpointError::ConfiguredSocketUnavailable { path: socket });
            }
            return Self::from_socket(socket);
        }
        if let Some(context) = environment
            .docker_context()
            .filter(|context| !context.is_empty() && context != "default")
        {
            return Err(DockerEndpointError::UnsupportedContext { context });
        }

        let candidates = known_socket_paths(environment);
        if let Some(socket) = candidates.iter().find(|path| probe.is_unix_socket(path)) {
            return Self::from_socket(socket.clone());
        }
        Err(DockerEndpointError::NoLocalSocket { tried: candidates })
    }

    fn from_socket(socket: PathBuf) -> Result<Self, DockerEndpointError> {
        if !socket.is_absolute() {
            return Err(DockerEndpointError::RelativeSocket { path: socket });
        }
        let text = socket.to_str().ok_or(DockerEndpointError::NonUtf8Socket)?;
        Ok(Self {
            docker_host: OsString::from(format!("unix://{text}")),
            socket,
        })
    }

    /// Borrows the absolute socket path supplied to Bollard.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket
    }

    /// Borrows the exact `unix://...` value injected into Docker and Compose.
    #[must_use]
    pub fn docker_host(&self) -> &OsStr {
        &self.docker_host
    }

    /// Constructs the pinned Bollard connector against this exact socket.
    ///
    /// # Errors
    ///
    /// Returns an adapter setup error if Bollard cannot represent or open the selected endpoint.
    pub fn bollard_connector(
        &self,
        control_timeout: Duration,
    ) -> Result<BollardConnector, BollardConnectorError> {
        let path = self
            .socket
            .to_str()
            .ok_or(BollardConnectorError::NonUtf8Socket)?;
        let timeout = control_timeout.as_secs().max(1);
        let client = Docker::connect_with_socket(path, timeout, API_DEFAULT_VERSION)?;
        Ok(BollardConnector {
            endpoint: self.clone(),
            client,
        })
    }
}

/// A Bollard client bound to one resolved endpoint.
#[derive(Clone)]
pub struct BollardConnector {
    endpoint: DockerEndpoint,
    client: Docker,
}

impl fmt::Debug for BollardConnector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BollardConnector")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl BollardConnector {
    /// Returns the exact endpoint represented by the client.
    #[must_use]
    pub const fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    /// Borrows the configured Bollard client.
    #[must_use]
    pub const fn client(&self) -> &Docker {
        &self.client
    }
}

/// Endpoint selection failure that never silently falls back to a remote daemon.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DockerEndpointError {
    /// `DOCKER_HOST` was not UTF-8 and cannot match supported Docker syntax.
    #[error("DOCKER_HOST must be valid UTF-8 and use unix:///absolute/path syntax")]
    NonUtf8DockerHost,
    /// A configured scheme or remote form is unsupported.
    #[error(
        "unsupported Docker endpoint {value:?}; cdenv V1 supports only a local unix:///absolute/path socket (unset DOCKER_HOST to probe local sockets)"
    )]
    UnsupportedEndpoint {
        /// Rejected value.
        value: String,
    },
    /// A Unix endpoint did not contain an absolute socket path.
    #[error("Docker Unix socket must be absolute: {path:?}")]
    RelativeSocket {
        /// Rejected path.
        path: PathBuf,
    },
    /// A configured Unix socket does not exist or is not a socket.
    #[error(
        "DOCKER_HOST selects {path:?}, but that path is not an available Unix socket; start the local daemon or correct/unset DOCKER_HOST"
    )]
    ConfiguredSocketUnavailable {
        /// Configured path.
        path: PathBuf,
    },
    /// A non-default context could target a daemon cdenv cannot verify as local.
    #[error(
        "Docker context {context:?} is unsupported; unset DOCKER_CONTEXT and select a local daemon with DOCKER_HOST=unix:///absolute/path"
    )]
    UnsupportedContext {
        /// Rejected context name.
        context: OsString,
    },
    /// No known local socket was available.
    #[error(
        "no local Docker Unix socket was found; tried {tried:?}; start Docker or set DOCKER_HOST=unix:///absolute/path"
    )]
    NoLocalSocket {
        /// Candidate paths in precedence order.
        tried: Vec<PathBuf>,
    },
    /// A discovered path was not representable for Docker/Bollard.
    #[error("the selected Docker Unix socket path must be valid UTF-8")]
    NonUtf8Socket,
}

/// Bollard connector construction failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum BollardConnectorError {
    /// Bollard requires a UTF-8 socket path.
    #[error("the selected Docker Unix socket path must be valid UTF-8 for Bollard")]
    NonUtf8Socket,
    /// Bollard rejected the selected local socket.
    #[error("cannot configure Bollard for the selected Docker socket: {0}")]
    Bollard(#[from] bollard::errors::Error),
}

/// A three-component dependency version.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u32,
    minor: u32,
    patch: u32,
}

impl Version {
    /// Constructs a version from exact numeric components.
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Returns the major component.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Returns the minor component.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }

    /// Returns the patch component.
    #[must_use]
    pub const fn patch(self) -> u32 {
        self.patch
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// A Docker Engine API major/minor capability.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ApiVersion {
    major: u32,
    minor: u32,
}

impl ApiVersion {
    /// Constructs an API version.
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Returns the major component.
    #[must_use]
    pub const fn major(self) -> u32 {
        self.major
    }

    /// Returns the minor component.
    #[must_use]
    pub const fn minor(self) -> u32 {
        self.minor
    }
}

impl fmt::Display for ApiVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

/// Versions and API capability established for one Docker-facing command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DockerCapabilities {
    /// Docker CLI version.
    pub cli: Version,
    /// Docker Engine version.
    pub engine: Version,
    /// Negotiable Engine API capability.
    pub api: ApiVersion,
    /// Docker Compose V2 plugin version, when that command requires Compose.
    pub compose: Option<Version>,
}

/// Docker CLI and Compose probe adapter bound to one endpoint.
#[derive(Clone, Debug)]
pub struct DockerCommandProbe {
    executable: PathBuf,
    endpoint: DockerEndpoint,
    runner: ProcessRunner,
    environment: Vec<(OsString, OsString)>,
    timeout: Duration,
}

impl DockerCommandProbe {
    /// Uses `docker` from the configured explicit environment's `PATH`.
    #[must_use]
    pub fn system(endpoint: DockerEndpoint, runner: ProcessRunner) -> Self {
        Self::new(
            PathBuf::from("docker"),
            endpoint,
            runner,
            env::vars_os().collect(),
        )
    }

    /// Uses an explicit executable and complete environment, primarily for deterministic tests.
    #[must_use]
    pub fn new(
        executable: PathBuf,
        endpoint: DockerEndpoint,
        runner: ProcessRunner,
        mut environment: Vec<(OsString, OsString)>,
    ) -> Self {
        environment.retain(|(name, _)| name != "DOCKER_HOST");
        environment.push((OsString::from("DOCKER_HOST"), endpoint.docker_host.clone()));
        Self {
            executable,
            endpoint,
            runner,
            environment,
            timeout: DOCKER_PROBE_TIMEOUT,
        }
    }

    /// Overrides the bounded capability-probe timeout.
    #[must_use]
    pub const fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Returns the endpoint injected into every probe.
    #[must_use]
    pub const fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    /// Probes Docker CLI, Engine, and API capabilities for a Docker-facing command.
    ///
    /// # Errors
    ///
    /// Returns a process, output-shape, old-version, or missing-capability failure.
    pub async fn probe_docker(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<DockerCapabilities, DockerProbeError> {
        let arguments: [&OsStr; 3] = [
            OsStr::new("version"),
            OsStr::new("--format"),
            OsStr::new("{{json .}}"),
        ];
        let output = self
            .run_probe("docker-version", &arguments, cwd, cancellation)
            .await?;
        let (cli, engine, api) = parse_docker_version(output.stdout.as_bytes())?;
        require_version("Docker CLI", cli, MINIMUM_DOCKER_CLI)?;
        require_version("Docker Engine", engine, MINIMUM_DOCKER_ENGINE)?;
        if api < MINIMUM_DOCKER_API {
            return Err(DockerProbeError::MissingApiCapability {
                detected: api,
                minimum: MINIMUM_DOCKER_API,
            });
        }
        Ok(DockerCapabilities {
            cli,
            engine,
            api,
            compose: None,
        })
    }

    /// Probes Docker and Compose V2 for a command that requires Compose.
    ///
    /// # Errors
    ///
    /// Returns a Docker probe failure or an absent, malformed, or old Compose V2 plugin.
    pub async fn probe_compose(
        &self,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<DockerCapabilities, DockerProbeError> {
        let mut capabilities = self.probe_docker(cwd, cancellation).await?;
        let arguments: [&OsStr; 4] = [
            OsStr::new("compose"),
            OsStr::new("version"),
            OsStr::new("--format"),
            OsStr::new("json"),
        ];
        let output = self
            .run_probe("compose-version", &arguments, cwd, cancellation)
            .await?;
        let compose = parse_compose_version(output.stdout.as_bytes())?;
        require_version("Docker Compose V2", compose, MINIMUM_COMPOSE)?;
        capabilities.compose = Some(compose);
        Ok(capabilities)
    }

    async fn run_probe(
        &self,
        operation: &'static str,
        arguments: &[&OsStr],
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<crate::ProcessResult, DockerProbeError> {
        let environment = self
            .environment
            .iter()
            .map(|(name, value)| ProcessEnvironmentVariable {
                name,
                value,
                sensitive: false,
            })
            .collect::<Vec<_>>();
        let request = ProcessRequest {
            operation,
            executable: &self.executable,
            arguments,
            cwd,
            environment: &environment,
            redactions: &[],
            deadline: ProcessDeadline::Control(self.timeout),
        };
        let result = self.runner.run(&request, cancellation).await?;
        if result.status.success() {
            Ok(result)
        } else {
            Err(DockerProbeError::Exited {
                operation,
                code: result.status.code(),
                stderr: bounded_summary(result.stderr.as_bytes()),
            })
        }
    }
}

/// Typed Docker/Compose dependency probe failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DockerProbeError {
    /// Direct process execution failed.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// A probe exited unsuccessfully.
    #[error("{operation} failed with exit code {code:?}: {stderr}")]
    Exited {
        /// Safe probe label.
        operation: &'static str,
        /// Portable exit code.
        code: Option<i32>,
        /// Bounded, redacted diagnostic output.
        stderr: String,
    },
    /// Docker version JSON was malformed or incomplete.
    #[error("Docker version output was not recognized")]
    InvalidDockerVersion,
    /// Compose V2 version JSON was malformed or incomplete.
    #[error("Docker Compose V2 version output was not recognized")]
    InvalidComposeVersion,
    /// A dependency is older than its verified baseline.
    #[error("{component} {detected} is unsupported; cdenv requires {minimum} or newer")]
    UnsupportedVersion {
        /// Dependency name.
        component: &'static str,
        /// Detected version.
        detected: Version,
        /// Minimum verified version.
        minimum: Version,
    },
    /// Engine version alone was new enough, but its API capability was not.
    #[error(
        "Docker Engine API {detected} lacks required API {minimum}; upgrade or select a compatible local daemon"
    )]
    MissingApiCapability {
        /// Detected API capability.
        detected: ApiVersion,
        /// Required capability.
        minimum: ApiVersion,
    },
}

fn parse_unix_docker_host(value: &str) -> Result<PathBuf, DockerEndpointError> {
    let Some(path) = value.strip_prefix("unix://") else {
        return Err(DockerEndpointError::UnsupportedEndpoint {
            value: value.to_owned(),
        });
    };
    if path.is_empty() || path.contains(['?', '#']) {
        return Err(DockerEndpointError::UnsupportedEndpoint {
            value: value.to_owned(),
        });
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(DockerEndpointError::RelativeSocket { path });
    }
    Ok(path)
}

fn known_socket_paths(environment: &impl DockerEnvironment) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Some(home) = environment.home_dir() {
        paths.push(home.join(".docker/run/docker.sock"));
        paths.push(home.join(".docker/desktop/docker.sock"));
    }
    if let Some(runtime) = environment.runtime_dir() {
        paths.push(runtime.join("docker.sock"));
    }
    paths.push(PathBuf::from("/var/run/docker.sock"));
    paths
}

fn parse_docker_version(bytes: &[u8]) -> Result<(Version, Version, ApiVersion), DockerProbeError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| DockerProbeError::InvalidDockerVersion)?;
    let client = value
        .pointer("/Client/Version")
        .and_then(Value::as_str)
        .ok_or(DockerProbeError::InvalidDockerVersion)?;
    let engine = value
        .pointer("/Server/Version")
        .and_then(Value::as_str)
        .ok_or(DockerProbeError::InvalidDockerVersion)?;
    let api = value
        .pointer("/Server/ApiVersion")
        .and_then(Value::as_str)
        .ok_or(DockerProbeError::InvalidDockerVersion)?;
    Ok((
        parse_version(client).ok_or(DockerProbeError::InvalidDockerVersion)?,
        parse_version(engine).ok_or(DockerProbeError::InvalidDockerVersion)?,
        parse_api_version(api).ok_or(DockerProbeError::InvalidDockerVersion)?,
    ))
}

fn parse_compose_version(bytes: &[u8]) -> Result<Version, DockerProbeError> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| DockerProbeError::InvalidComposeVersion)?;
    value
        .get("version")
        .and_then(Value::as_str)
        .and_then(parse_version)
        .ok_or(DockerProbeError::InvalidComposeVersion)
}

fn parse_version(text: &str) -> Option<Version> {
    let text = text.trim_start_matches(['v', 'V']);
    let mut components = text.split('.');
    Some(Version::new(
        parse_leading_number(components.next()?)?,
        parse_leading_number(components.next()?)?,
        parse_leading_number(components.next()?)?,
    ))
}

fn parse_api_version(text: &str) -> Option<ApiVersion> {
    let mut components = text.split('.');
    Some(ApiVersion::new(
        parse_leading_number(components.next()?)?,
        parse_leading_number(components.next()?)?,
    ))
}

fn parse_leading_number(text: &str) -> Option<u32> {
    let length = text
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(text.len());
    text.get(..length)
        .filter(|digits| !digits.is_empty())?
        .parse()
        .ok()
}

fn require_version(
    component: &'static str,
    detected: Version,
    minimum: Version,
) -> Result<(), DockerProbeError> {
    if detected < minimum {
        Err(DockerProbeError::UnsupportedVersion {
            component,
            detected,
            minimum,
        })
    } else {
        Ok(())
    }
}

fn bounded_summary(bytes: &[u8]) -> String {
    const MAXIMUM: usize = 1024;
    let mut summary = String::from_utf8_lossy(bytes).replace(char::is_control, " ");
    if summary.len() > MAXIMUM {
        let mut boundary = MAXIMUM;
        while !summary.is_char_boundary(boundary) {
            boundary -= 1;
        }
        summary.truncate(boundary);
    }
    summary
}

#[cfg(unix)]
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt;
    fs::metadata(path).is_ok_and(|metadata| metadata.file_type().is_socket())
}

#[cfg(not(unix))]
const fn is_socket(_path: &Path) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[derive(Default)]
    struct Environment {
        host: Option<OsString>,
        context: Option<OsString>,
        home: Option<PathBuf>,
        runtime: Option<PathBuf>,
    }

    impl DockerEnvironment for Environment {
        fn docker_host(&self) -> Option<OsString> {
            self.host.clone()
        }

        fn docker_context(&self) -> Option<OsString> {
            self.context.clone()
        }

        fn home_dir(&self) -> Option<PathBuf> {
            self.home.clone()
        }

        fn runtime_dir(&self) -> Option<PathBuf> {
            self.runtime.clone()
        }
    }

    struct Probe(BTreeSet<PathBuf>);

    impl DockerSocketProbe for Probe {
        fn is_unix_socket(&self, path: &Path) -> bool {
            self.0.contains(path)
        }
    }

    fn environment() -> Environment {
        Environment {
            home: Some(PathBuf::from("/home/test")),
            runtime: Some(PathBuf::from("/run/user/1000")),
            ..Environment::default()
        }
    }

    #[test]
    fn configured_unix_docker_host_has_precedence() {
        let mut environment = environment();
        environment.host = Some(OsString::from("unix:///chosen/docker.sock"));
        let probe = Probe(BTreeSet::from([
            PathBuf::from("/chosen/docker.sock"),
            PathBuf::from("/home/test/.docker/run/docker.sock"),
        ]));

        let endpoint = DockerEndpoint::resolve_with_probe(&environment, &probe)
            .expect("configured socket should resolve");

        assert_eq!(endpoint.socket_path(), Path::new("/chosen/docker.sock"));
    }

    #[test]
    fn docker_desktop_run_socket_is_first_known_path() {
        let environment = environment();
        let probe = Probe(BTreeSet::from([
            PathBuf::from("/home/test/.docker/run/docker.sock"),
            PathBuf::from("/run/user/1000/docker.sock"),
        ]));

        let endpoint = DockerEndpoint::resolve_with_probe(&environment, &probe)
            .expect("Desktop socket should resolve");

        assert_eq!(
            endpoint.socket_path(),
            Path::new("/home/test/.docker/run/docker.sock")
        );
    }

    #[test]
    fn docker_desktop_linux_socket_is_supported() {
        let environment = environment();
        let probe = Probe(BTreeSet::from([PathBuf::from(
            "/home/test/.docker/desktop/docker.sock",
        )]));

        let endpoint = DockerEndpoint::resolve_with_probe(&environment, &probe)
            .expect("Desktop Linux socket should resolve");

        assert_eq!(
            endpoint.socket_path(),
            Path::new("/home/test/.docker/desktop/docker.sock")
        );
    }

    #[test]
    fn rootless_socket_is_supported() {
        let environment = environment();
        let probe = Probe(BTreeSet::from([PathBuf::from(
            "/run/user/1000/docker.sock",
        )]));

        let endpoint = DockerEndpoint::resolve_with_probe(&environment, &probe)
            .expect("rootless socket should resolve");

        assert_eq!(
            endpoint.socket_path(),
            Path::new("/run/user/1000/docker.sock")
        );
    }

    #[test]
    fn default_socket_is_supported() {
        let environment = environment();
        let probe = Probe(BTreeSet::from([PathBuf::from("/var/run/docker.sock")]));

        let endpoint = DockerEndpoint::resolve_with_probe(&environment, &probe)
            .expect("default socket should resolve");

        assert_eq!(endpoint.socket_path(), Path::new("/var/run/docker.sock"));
    }

    #[test]
    fn absent_sockets_report_every_candidate() {
        let error = DockerEndpoint::resolve_with_probe(&environment(), &Probe(BTreeSet::new()))
            .expect_err("absent sockets should fail");

        assert_eq!(
            error,
            DockerEndpointError::NoLocalSocket {
                tried: vec![
                    PathBuf::from("/home/test/.docker/run/docker.sock"),
                    PathBuf::from("/home/test/.docker/desktop/docker.sock"),
                    PathBuf::from("/run/user/1000/docker.sock"),
                    PathBuf::from("/var/run/docker.sock"),
                ]
            }
        );
    }

    #[test]
    fn every_remote_or_unsupported_endpoint_is_rejected() {
        for value in [
            "tcp://127.0.0.1:2375",
            "tcp://host:2376",
            "ssh://host",
            "http://host",
            "https://host",
            "npipe:////./pipe/docker_engine",
            "context://remote",
            "/var/run/docker.sock",
            "unix://relative/socket",
        ] {
            let environment = Environment {
                host: Some(OsString::from(value)),
                ..environment()
            };
            assert!(
                DockerEndpoint::resolve_with_probe(&environment, &Probe(BTreeSet::new())).is_err(),
                "{value} should be rejected"
            );
        }
    }

    #[test]
    fn newer_versions_and_vendor_suffixes_parse() {
        assert_eq!(
            parse_version("v30.8.4-desktop.1"),
            Some(Version::new(30, 8, 4))
        );
    }

    #[test]
    fn versions_below_the_verified_baseline_are_rejected() {
        let error = require_version("Docker CLI", Version::new(29, 7, 0), MINIMUM_DOCKER_CLI)
            .expect_err("older CLI should fail");

        assert!(matches!(
            error,
            DockerProbeError::UnsupportedVersion {
                component: "Docker CLI",
                detected: Version {
                    major: 29,
                    minor: 7,
                    patch: 0
                },
                minimum: MINIMUM_DOCKER_CLI,
            }
        ));
    }
}
