//! Complete V1 command-line grammar and validated CLI-only values.

use std::ffi::OsString;
use std::fmt;
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;

use cdenv_core::{
    TcpPort, WORKSPACE_HOST_SUFFIX, WorkspaceHost, WorkspaceHostError, WorkspaceName,
    WorkspaceNameError,
};
use clap::{
    ArgMatches, Args, CommandFactory, FromArgMatches, Parser, Subcommand, error::ErrorKind,
};
use thiserror::Error;

#[derive(Debug, Parser, PartialEq, Eq)]
#[command(
    name = "cdenv",
    version,
    about = "Manage isolated Docker-backed development environments",
    disable_help_subcommand = true,
    subcommand_required = true,
    arg_required_else_help = true,
    group(
        clap::ArgGroup::new("ssh_config_consent")
            .args(["modify_ssh_config", "no_modify_ssh_config"])
            .multiple(false)
    )
)]
struct ParsedCommandLine {
    /// Use a specific cdenv root instead of environment or home-directory defaults.
    #[arg(long, global = true, value_name = "PATH")]
    root: Option<PathBuf>,

    /// Allow cdenv to add its Include directive to the user SSH config.
    #[arg(long, global = true, conflicts_with = "no_modify_ssh_config")]
    modify_ssh_config: bool,

    /// Record that cdenv must not modify the user SSH config.
    #[arg(long, global = true, conflicts_with = "modify_ssh_config")]
    no_modify_ssh_config: bool,

    #[command(subcommand)]
    command: CliCommand,
}

/// The parsed and cross-scope-validated `cdenv` command line.
#[derive(Debug, PartialEq, Eq)]
pub struct CommandLine(ParsedCommandLine);

impl CommandLine {
    /// Returns the explicit root argument, if one was supplied.
    #[must_use]
    pub fn root(&self) -> Option<&Path> {
        self.0.root.as_deref()
    }

    /// Returns explicit SSH Include consent from the mutually exclusive flags.
    #[must_use]
    pub const fn ssh_config_consent(&self) -> Option<SshConfigConsent> {
        if self.0.modify_ssh_config {
            Some(SshConfigConsent::Accept)
        } else if self.0.no_modify_ssh_config {
            Some(SshConfigConsent::Decline)
        } else {
            None
        }
    }

    /// Returns the selected V1 command.
    #[must_use]
    pub const fn command(&self) -> &CliCommand {
        &self.0.command
    }

    /// Returns the output format requested by the selected command.
    #[must_use]
    pub const fn output_format(&self) -> OutputFormat {
        self.0.command.output_format()
    }

    fn from_parsed(parsed: ParsedCommandLine) -> Result<Self, clap::Error> {
        validate_ssh_config_consent(&parsed)?;
        Ok(Self(parsed))
    }

    fn validate(&self) -> Result<(), clap::Error> {
        validate_ssh_config_consent(&self.0)
    }
}

fn validate_ssh_config_consent(parsed: &ParsedCommandLine) -> Result<(), clap::Error> {
    if parsed.modify_ssh_config && parsed.no_modify_ssh_config {
        let mut command = <ParsedCommandLine as CommandFactory>::command();
        return Err(command.error(
            ErrorKind::ArgumentConflict,
            "the argument '--modify-ssh-config' cannot be used with '--no-modify-ssh-config'",
        ));
    }

    Ok(())
}

impl CommandFactory for CommandLine {
    fn command() -> clap::Command {
        <ParsedCommandLine as CommandFactory>::command()
    }

    fn command_for_update() -> clap::Command {
        <ParsedCommandLine as CommandFactory>::command_for_update()
    }
}

impl FromArgMatches for CommandLine {
    fn from_arg_matches(matches: &ArgMatches) -> Result<Self, clap::Error> {
        let parsed = <ParsedCommandLine as FromArgMatches>::from_arg_matches(matches)?;
        Self::from_parsed(parsed)
    }

    fn from_arg_matches_mut(matches: &mut ArgMatches) -> Result<Self, clap::Error> {
        let parsed = <ParsedCommandLine as FromArgMatches>::from_arg_matches_mut(matches)?;
        Self::from_parsed(parsed)
    }

    fn update_from_arg_matches(&mut self, matches: &ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches(matches)?;
        self.validate()
    }

    fn update_from_arg_matches_mut(&mut self, matches: &mut ArgMatches) -> Result<(), clap::Error> {
        self.0.update_from_arg_matches_mut(matches)?;
        self.validate()
    }
}

impl Parser for CommandLine {}

/// Explicit consent supplied for modifying the user SSH configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshConfigConsent {
    /// The user explicitly permits adding the cdenv Include directive.
    Accept,
    /// The user explicitly declines automatic SSH configuration changes.
    Decline,
}

/// The presentation format requested by a command.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// Deterministic human-readable output.
    #[default]
    Human,
    /// One versioned JSON document on standard output.
    Json,
}

/// A V1 cdenv command and its parsed arguments.
#[derive(Debug, Subcommand, PartialEq, Eq)]
pub enum CliCommand {
    /// Clone a Git repository and create its environment.
    Create(CreateArgs),
    /// List managed workspaces.
    List(ListArgs),
    /// Create or start a workspace environment.
    Up(UpArgs),
    /// Stop a workspace environment.
    Down(DownArgs),
    /// Replace a workspace environment from its current configuration.
    Rebuild(RebuildArgs),
    /// Show detailed workspace status.
    Status(StatusArgs),
    /// Connect to a workspace with system OpenSSH.
    Ssh(SshArgs),
    /// Forward local TCP ports to a workspace.
    Forward(ForwardArgs),
    /// Create or update a workspace Feature lockfile.
    Lock(LockArgs),
    /// Bridge an OpenSSH stdio transport to a workspace agent.
    Proxy(ProxyArgs),
    /// Diagnose cdenv without making repairs.
    Doctor(DoctorArgs),
}

impl CliCommand {
    /// Returns the stable command identity.
    #[must_use]
    pub const fn kind(&self) -> CommandKind {
        match self {
            Self::Create(_) => CommandKind::Create,
            Self::List(_) => CommandKind::List,
            Self::Up(_) => CommandKind::Up,
            Self::Down(_) => CommandKind::Down,
            Self::Rebuild(_) => CommandKind::Rebuild,
            Self::Status(_) => CommandKind::Status,
            Self::Ssh(_) => CommandKind::Ssh,
            Self::Forward(_) => CommandKind::Forward,
            Self::Lock(_) => CommandKind::Lock,
            Self::Proxy(_) => CommandKind::Proxy,
            Self::Doctor(_) => CommandKind::Doctor,
        }
    }

    /// Returns the selected human or machine-readable output format.
    #[must_use]
    pub const fn output_format(&self) -> OutputFormat {
        match self {
            Self::List(arguments) if arguments.json => OutputFormat::Json,
            Self::Status(arguments) if arguments.json => OutputFormat::Json,
            Self::Doctor(arguments) if arguments.json => OutputFormat::Json,
            Self::Create(_)
            | Self::List(_)
            | Self::Up(_)
            | Self::Down(_)
            | Self::Rebuild(_)
            | Self::Status(_)
            | Self::Ssh(_)
            | Self::Forward(_)
            | Self::Lock(_)
            | Self::Proxy(_)
            | Self::Doctor(_) => OutputFormat::Human,
        }
    }
}

/// Stable identity for one V1 command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    /// The `create` command.
    Create,
    /// The `list` command.
    List,
    /// The `up` command.
    Up,
    /// The `down` command.
    Down,
    /// The `rebuild` command.
    Rebuild,
    /// The `status` command.
    Status,
    /// The `ssh` command.
    Ssh,
    /// The `forward` command.
    Forward,
    /// The `lock` command.
    Lock,
    /// The `proxy` command.
    Proxy,
    /// The `doctor` command.
    Doctor,
}

impl CommandKind {
    /// Returns the command's exact CLI spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::List => "list",
            Self::Up => "up",
            Self::Down => "down",
            Self::Rebuild => "rebuild",
            Self::Status => "status",
            Self::Ssh => "ssh",
            Self::Forward => "forward",
            Self::Lock => "lock",
            Self::Proxy => "proxy",
            Self::Doctor => "doctor",
        }
    }
}

impl fmt::Display for CommandKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Arguments for `cdenv create`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct CreateArgs {
    /// The Git URL, SCP-like source, or local repository path to clone.
    #[arg(value_name = "GIT_SOURCE")]
    pub git_source: String,
    /// An explicit validated workspace name instead of source-derived naming.
    #[arg(long, value_name = "NAME")]
    pub name: Option<WorkspaceName>,
    /// An explicit repository-relative Dev Container configuration path.
    #[arg(long, value_name = "PATH")]
    pub config: Option<RepoRelativeConfigPath>,
}

/// Arguments for `cdenv list`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ListArgs {
    /// Emit one versioned JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `cdenv up`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct UpArgs {
    /// The workspace to create or start.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// A desired repository-relative Dev Container configuration path.
    #[arg(long, value_name = "PATH")]
    pub config: Option<RepoRelativeConfigPath>,
}

/// Arguments for `cdenv down`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct DownArgs {
    /// The workspace to stop.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
}

/// Arguments for `cdenv rebuild`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct RebuildArgs {
    /// The workspace whose environment will be replaced.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// A desired repository-relative Dev Container configuration path.
    #[arg(long, value_name = "PATH")]
    pub config: Option<RepoRelativeConfigPath>,
    /// Disable Docker and Compose build caches for this rebuild.
    #[arg(long)]
    pub no_cache: bool,
}

/// Arguments for `cdenv status`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct StatusArgs {
    /// The workspace to inspect.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// Emit one versioned JSON document.
    #[arg(long)]
    pub json: bool,
}

/// Arguments for `cdenv ssh`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct SshArgs {
    /// The workspace to connect to.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// Remote command arguments preserved exactly after the `--` delimiter.
    #[arg(last = true, value_name = "REMOTE_ARG", num_args = 0..)]
    pub remote_argv: Vec<OsString>,
}

/// Arguments for `cdenv forward`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ForwardArgs {
    /// The workspace whose container network receives the connections.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// One or more local-to-container TCP port mappings.
    #[arg(
        value_name = "LOCAL_PORT:CONTAINER_PORT",
        required = true,
        num_args = 1..
    )]
    pub mappings: Vec<ForwardMapping>,
    /// The local IP address on which OpenSSH listens.
    #[arg(long, value_name = "ADDRESS", default_value = "127.0.0.1")]
    pub bind: IpAddr,
}

/// Arguments for `cdenv lock`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct LockArgs {
    /// The workspace whose Feature lockfile will be updated.
    #[arg(value_name = "NAME")]
    pub name: WorkspaceName,
    /// A repository-relative configuration selecting the adjacent lockfile.
    #[arg(long, value_name = "PATH")]
    pub config: Option<RepoRelativeConfigPath>,
}

/// Arguments for the internal `cdenv proxy` transport command.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct ProxyArgs {
    /// A workspace name or its exact `<name>.cdenv` SSH host.
    #[arg(value_name = "WORKSPACE_NAME_OR_HOST")]
    pub workspace: WorkspaceSelector,
}

/// Arguments for `cdenv doctor`.
#[derive(Debug, Args, PartialEq, Eq)]
pub struct DoctorArgs {
    /// Emit one versioned JSON document.
    #[arg(long)]
    pub json: bool,
}

/// A validated repository-relative Dev Container configuration path.
///
/// This lexical type rejects absolute paths and parent traversal. Filesystem
/// containment and symlink validation occur when the selected checkout is
/// available.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RepoRelativeConfigPath(PathBuf);

/// An error returned when parsing a [`RepoRelativeConfigPath`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum RepoRelativeConfigPathError {
    /// The path contains no file component.
    #[error("a configuration path must name a repository-relative file")]
    Empty,
    /// The path is rooted or has a platform path prefix.
    #[error("a configuration path must be relative to the repository checkout")]
    Absolute,
    /// The path contains a parent-directory component.
    #[error("a configuration path cannot contain `..`")]
    ParentTraversal,
}

impl RepoRelativeConfigPath {
    /// Parses a lexical repository-relative configuration path.
    ///
    /// # Errors
    ///
    /// Returns [`RepoRelativeConfigPathError`] for an empty, absolute, or
    /// parent-traversing path.
    pub fn parse(value: &str) -> Result<Self, RepoRelativeConfigPathError> {
        Self::from_str(value)
    }

    /// Returns the validated lexical path.
    #[must_use]
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// Consumes the wrapper and returns its path buffer.
    #[must_use]
    pub fn into_path_buf(self) -> PathBuf {
        self.0
    }
}

impl AsRef<Path> for RepoRelativeConfigPath {
    fn as_ref(&self) -> &Path {
        self.as_path()
    }
}

impl fmt::Display for RepoRelativeConfigPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.as_path().display().fmt(formatter)
    }
}

impl FromStr for RepoRelativeConfigPath {
    type Err = RepoRelativeConfigPathError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            return Err(RepoRelativeConfigPathError::Absolute);
        }

        let mut has_file_component = false;
        for component in path.components() {
            match component {
                Component::Prefix(_) | Component::RootDir => {
                    return Err(RepoRelativeConfigPathError::Absolute);
                }
                Component::ParentDir => {
                    return Err(RepoRelativeConfigPathError::ParentTraversal);
                }
                Component::Normal(_) => has_file_component = true,
                Component::CurDir => {}
            }
        }

        if !has_file_component {
            return Err(RepoRelativeConfigPathError::Empty);
        }

        Ok(Self(path))
    }
}

/// A validated local-to-container TCP port pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ForwardMapping {
    local_port: TcpPort,
    container_port: TcpPort,
}

/// An error returned when parsing a [`ForwardMapping`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForwardMappingError {
    /// The mapping does not have exactly two colon-separated fields.
    #[error("forward mapping {mapping:?} must have the form `<local-port>:<container-port>`")]
    InvalidFormat {
        /// The rejected mapping.
        mapping: String,
    },
    /// The local port is not a decimal integer in the TCP range.
    #[error("local port {value:?} must be a decimal integer from 1 through 65535")]
    InvalidLocalPort {
        /// The rejected local-port field.
        value: String,
    },
    /// The container port is not a decimal integer in the TCP range.
    #[error("container port {value:?} must be a decimal integer from 1 through 65535")]
    InvalidContainerPort {
        /// The rejected container-port field.
        value: String,
    },
}

impl ForwardMapping {
    /// Creates a forwarding pair from validated nonzero ports.
    #[must_use]
    pub const fn new(local_port: TcpPort, container_port: TcpPort) -> Self {
        Self {
            local_port,
            container_port,
        }
    }

    /// Returns the local listener port.
    #[must_use]
    pub const fn local_port(self) -> TcpPort {
        self.local_port
    }

    /// Returns the target port inside the container network.
    #[must_use]
    pub const fn container_port(self) -> TcpPort {
        self.container_port
    }
}

impl fmt::Display for ForwardMapping {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}:{}", self.local_port, self.container_port)
    }
}

impl FromStr for ForwardMapping {
    type Err = ForwardMappingError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((local, container)) = value.split_once(':') else {
            return Err(ForwardMappingError::InvalidFormat {
                mapping: value.to_owned(),
            });
        };
        if container.contains(':') {
            return Err(ForwardMappingError::InvalidFormat {
                mapping: value.to_owned(),
            });
        }

        let Some(local_port) = parse_cli_port(local) else {
            return Err(ForwardMappingError::InvalidLocalPort {
                value: local.to_owned(),
            });
        };
        let Some(container_port) = parse_cli_port(container) else {
            return Err(ForwardMappingError::InvalidContainerPort {
                value: container.to_owned(),
            });
        };

        Ok(Self::new(local_port, container_port))
    }
}

fn parse_cli_port(value: &str) -> Option<TcpPort> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    value
        .parse::<u16>()
        .ok()
        .and_then(|port| TcpPort::new(port).ok())
}

/// A proxy argument retaining whether the caller used a name or SSH host.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum WorkspaceSelector {
    /// A direct workspace name.
    Name(WorkspaceName),
    /// An exact `<workspace>.cdenv` SSH host.
    Host(WorkspaceHost),
}

/// An error returned when parsing a [`WorkspaceSelector`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceSelectorError {
    /// A direct name is invalid.
    #[error("invalid workspace name: {0}")]
    InvalidName(#[from] WorkspaceNameError),
    /// A value ending in `.cdenv` is not a valid exact workspace host.
    #[error("invalid workspace host: {0}")]
    InvalidHost(#[source] WorkspaceHostError),
}

impl WorkspaceSelector {
    /// Returns the normalized workspace name.
    #[must_use]
    pub const fn workspace_name(&self) -> &WorkspaceName {
        match self {
            Self::Name(name) => name,
            Self::Host(host) => host.workspace_name(),
        }
    }
}

impl fmt::Display for WorkspaceSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(name) => name.fmt(formatter),
            Self::Host(host) => host.fmt(formatter),
        }
    }
}

impl FromStr for WorkspaceSelector {
    type Err = WorkspaceSelectorError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.ends_with(WORKSPACE_HOST_SUFFIX) {
            WorkspaceHost::parse(value)
                .map(Self::Host)
                .map_err(WorkspaceSelectorError::InvalidHost)
        } else {
            WorkspaceName::parse(value)
                .map(Self::Name)
                .map_err(WorkspaceSelectorError::InvalidName)
        }
    }
}
