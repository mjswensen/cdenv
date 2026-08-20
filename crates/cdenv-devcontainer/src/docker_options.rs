//! Pure Dockerfile/build and Docker-shaped option planning.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    ContainerPath, HostSubstitutionInputs, RawProfile, RawScenario, ResolvedString, RuntimePlan,
    SubstitutionProperty, substitute_host,
};

/// A normalized path relative to the canonical repository checkout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RepositoryPath(String);

impl RepositoryPath {
    /// Validates a repository-relative path. Empty input denotes the checkout root.
    ///
    /// # Errors
    ///
    /// Rejects absolute paths, traversal above the checkout, and control characters.
    pub fn parse(value: &str) -> Result<Self, RepositoryPathError> {
        resolve_repository_path("", value)
    }

    /// Borrows the normalized slash-separated path.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Invalid repository-contained path.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("path must remain relative to the repository checkout")]
pub struct RepositoryPathError;

/// Inputs needed to plan Docker-shaped options without reading the filesystem.
pub struct DockerOptionPlanningInputs<'a> {
    /// Repository-relative directory containing the selected configuration.
    pub config_directory: &'a str,
    /// Allowed host/workspace substitutions.
    pub substitutions: &'a HostSubstitutionInputs<'a>,
    /// Container targets reserved for cdenv-injected assets.
    pub cdenv_owned_targets: &'a [ContainerPath],
}

/// Effective build source and supported Docker build settings.
pub enum BuildPlan {
    /// Pull/use an existing image.
    Image {
        /// Exact image reference.
        image: String,
    },
    /// Build the repository Dockerfile through `BuildKit`.
    Dockerfile(DockerfileBuildPlan),
    /// Compose owns base service pull/build planning.
    Compose,
}

impl std::fmt::Debug for BuildPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Image { .. } => formatter.write_str("BuildPlan::Image(<redacted>)"),
            Self::Dockerfile(plan) => plan.fmt(formatter),
            Self::Compose => formatter.write_str("BuildPlan::Compose"),
        }
    }
}

/// Immutable Dockerfile build plan. Build argument values are non-serializable and redacted.
pub struct DockerfileBuildPlan {
    /// Repository-contained Dockerfile.
    pub dockerfile: RepositoryPath,
    /// Repository-contained `BuildKit` context.
    pub context: RepositoryPath,
    /// Optional multi-stage target.
    pub target: Option<String>,
    /// Host-stage-resolved build arguments.
    pub arguments: BTreeMap<String, ResolvedString>,
    /// Ordered cache sources.
    pub cache_from: Vec<String>,
    /// Ordered non-reserved Docker build options.
    pub options: Vec<String>,
}

impl std::fmt::Debug for DockerfileBuildPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BuildPlan::Dockerfile")
            .field("dockerfile", &self.dockerfile)
            .field("context", &self.context)
            .field("target_present", &self.target.is_some())
            .field("argument_count", &self.arguments.len())
            .field("cache_source_count", &self.cache_from.len())
            .field("option_count", &self.options.len())
            .finish()
    }
}

/// Ordered create-time arguments that do not conflict with cdenv invariants.
#[derive(Clone, PartialEq, Eq)]
pub struct CreateOptionsPlan {
    /// Substituted arguments retained byte-for-byte and in source order.
    pub run_arguments: Vec<String>,
}

impl std::fmt::Debug for CreateOptionsPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreateOptionsPlan")
            .field("argument_count", &self.run_arguments.len())
            .finish()
    }
}

/// Combined Docker-shaped build/create plan.
#[derive(Debug)]
pub struct DockerOptionsPlan {
    /// Image, Dockerfile, or Compose build source.
    pub build: BuildPlan,
    /// Validated passthrough create arguments.
    pub create: CreateOptionsPlan,
}

/// Plans build properties/options and `runArgs` without invoking Docker.
///
/// # Errors
///
/// Returns an exact property and offending argument when an option conflicts with cdenv-owned
/// Dockerfile/context/output, identity, user, mount, or process attachment invariants.
pub fn plan_docker_options(
    profile: &RawProfile,
    runtime: &RuntimePlan,
    inputs: &DockerOptionPlanningInputs<'_>,
) -> Result<DockerOptionsPlan, DockerOptionError> {
    let config_directory = RepositoryPath::parse(inputs.config_directory).map_err(|_| {
        DockerOptionError::without_argument(
            "$.build",
            DockerOptionErrorKind::PathOutsideCheckout,
            "selected configuration directory must remain in the checkout",
        )
    })?;
    let (build, raw_run_arguments): (BuildPlan, &[String]) = match &profile.scenario {
        RawScenario::Image(value) => (
            BuildPlan::Image {
                image: value.image.clone(),
            },
            &value.options.run_args,
        ),
        RawScenario::Dockerfile(value) => {
            validate_build_options(&value.build.options)?;
            let dockerfile =
                resolve_repository_path(config_directory.as_str(), &value.build.dockerfile)
                    .map_err(|_| {
                        DockerOptionError::without_argument(
                            "$.build.dockerfile",
                            DockerOptionErrorKind::PathOutsideCheckout,
                            "Dockerfile must remain in the repository checkout",
                        )
                    })?;
            let context = value
                .build
                .context
                .as_deref()
                .map_or_else(
                    || Ok(config_directory.clone()),
                    |context| resolve_repository_path(config_directory.as_str(), context),
                )
                .map_err(|_| {
                    DockerOptionError::without_argument(
                        "$.build.context",
                        DockerOptionErrorKind::PathOutsideCheckout,
                        "build context must remain in the repository checkout",
                    )
                })?;
            let mut arguments = BTreeMap::new();
            for (name, value) in &value.build.args {
                let resolved = substitute_host(
                    SubstitutionProperty::BuildArgument,
                    value,
                    inputs.substitutions,
                )
                .map_err(|_| {
                    DockerOptionError::without_argument(
                        format!("$.build.args[{}]", serde_json::Value::String(name.clone())),
                        DockerOptionErrorKind::Substitution,
                        "unsupported or malformed build argument substitution",
                    )
                })?
                .resolve(&BTreeMap::new());
                arguments.insert(name.clone(), resolved);
            }
            (
                BuildPlan::Dockerfile(DockerfileBuildPlan {
                    dockerfile,
                    context,
                    target: value.build.target.clone(),
                    arguments,
                    cache_from: value.build.cache_from.clone(),
                    options: value.build.options.clone(),
                }),
                &value.options.run_args,
            )
        }
        RawScenario::Compose(_) => (BuildPlan::Compose, &[]),
    };
    let run_arguments = substitute_run_arguments(raw_run_arguments, inputs.substitutions)?;
    validate_run_arguments(
        raw_run_arguments,
        &run_arguments,
        runtime,
        inputs.cdenv_owned_targets,
    )?;
    Ok(DockerOptionsPlan {
        build,
        create: CreateOptionsPlan { run_arguments },
    })
}

fn substitute_run_arguments(
    arguments: &[String],
    substitutions: &HostSubstitutionInputs<'_>,
) -> Result<Vec<String>, DockerOptionError> {
    arguments
        .iter()
        .enumerate()
        .map(|(index, argument)| {
            substitute_host(SubstitutionProperty::RunArgument, argument, substitutions)
                .map(|value| value.resolve(&BTreeMap::new()).expose().to_owned())
                .map_err(|_| {
                    DockerOptionError::new(
                        format!("$.runArgs[{index}]"),
                        argument,
                        DockerOptionErrorKind::Substitution,
                        "unsupported or malformed run argument substitution",
                    )
                })
        })
        .collect()
}

fn validate_build_options(arguments: &[String]) -> Result<(), DockerOptionError> {
    let mut index = 0;
    while index < arguments.len() {
        let argument = &arguments[index];
        let property = format!("$.build.options[{index}]");
        if reserved_long(
            argument,
            &[
                "--file",
                "--tag",
                "--target",
                "--output",
                "--iidfile",
                "--metadata-file",
            ],
        ) || reserved_short(argument, &['f', 't', 'o'])
        {
            return Err(DockerOptionError::new(
                property,
                argument,
                DockerOptionErrorKind::BuildInvariant,
                "option conflicts with cdenv-owned Dockerfile, target, tag, or build output",
            ));
        }
        if let Some(label) = option_value(arguments, index, "--label", None)
            && cdenv_label(label)
        {
            return Err(DockerOptionError::new(
                property,
                argument,
                DockerOptionErrorKind::IdentityLabel,
                "option conflicts with a cdenv identity label",
            ));
        }
        if !argument.starts_with('-') || argument == "-" || argument == "--" {
            return Err(DockerOptionError::new(
                property,
                argument,
                DockerOptionErrorKind::BuildContext,
                "positional build context is cdenv-owned",
            ));
        }
        let consumes_next = !argument.contains('=')
            && (argument == "--label" || build_option_takes_value(argument));
        index += if consumes_next { 2 } else { 1 };
    }
    Ok(())
}

fn build_option_takes_value(argument: &str) -> bool {
    matches!(
        argument,
        "--add-host"
            | "--allow"
            | "--annotation"
            | "--attest"
            | "--build-arg"
            | "--build-context"
            | "--cache-from"
            | "--cache-to"
            | "--call"
            | "--cgroup-parent"
            | "--cpu-period"
            | "--cpu-quota"
            | "--cpu-shares"
            | "--cpuset-cpus"
            | "--cpuset-mems"
            | "--memory"
            | "--network"
            | "--no-cache-filter"
            | "--platform"
            | "--progress"
            | "--provenance"
            | "--sbom"
            | "--secret"
            | "--shm-size"
            | "--ssh"
            | "--ulimit"
    )
}

fn validate_run_arguments(
    raw_arguments: &[String],
    arguments: &[String],
    runtime: &RuntimePlan,
    cdenv_owned_targets: &[ContainerPath],
) -> Result<(), DockerOptionError> {
    for (index, argument) in arguments.iter().enumerate() {
        let property = format!("$.runArgs[{index}]");
        let diagnostic_argument = &raw_arguments[index];
        if reserved_long(argument, &["--name", "--rm", "--user", "--entrypoint"])
            || reserved_short(argument, &['u'])
        {
            return Err(DockerOptionError::new(
                property,
                diagnostic_argument,
                if argument == "--rm" || argument.starts_with("--rm=") {
                    DockerOptionErrorKind::AutoRemove
                } else if argument == "--name" || argument.starts_with("--name=") {
                    DockerOptionErrorKind::ContainerName
                } else if argument == "--entrypoint" || argument.starts_with("--entrypoint=") {
                    DockerOptionErrorKind::Entrypoint
                } else {
                    DockerOptionErrorKind::ContainerUser
                },
                "option conflicts with a cdenv-owned create setting",
            ));
        }
        if reserved_long(
            argument,
            &[
                "--attach",
                "--detach",
                "--interactive",
                "--tty",
                "--sig-proxy",
            ],
        ) || reserved_attach_short(argument)
        {
            return Err(DockerOptionError::new(
                property,
                diagnostic_argument,
                DockerOptionErrorKind::AttachmentMode,
                "option conflicts with cdenv-owned attach, stdin, or TTY mode",
            ));
        }
        if let Some(label) = option_value(arguments, index, "--label", Some('l'))
            && cdenv_label(label)
        {
            return Err(DockerOptionError::new(
                property,
                diagnostic_argument,
                DockerOptionErrorKind::IdentityLabel,
                "option conflicts with a cdenv identity label",
            ));
        }
        if let Some(mount) = option_value(arguments, index, "--mount", None) {
            let target = mount_target(mount).ok_or_else(|| {
                DockerOptionError::new(
                    property.clone(),
                    diagnostic_argument,
                    DockerOptionErrorKind::InvalidMountArgument,
                    "--mount must contain one target, dst, or destination field",
                )
            })?;
            reject_mount_target(
                target,
                &property,
                diagnostic_argument,
                runtime,
                cdenv_owned_targets,
            )?;
        }
        if let Some(volume) = option_value(arguments, index, "--volume", Some('v')) {
            let target = volume_target(volume).ok_or_else(|| {
                DockerOptionError::new(
                    property.clone(),
                    diagnostic_argument,
                    DockerOptionErrorKind::InvalidMountArgument,
                    "--volume must contain an absolute container target",
                )
            })?;
            reject_mount_target(
                target,
                &property,
                diagnostic_argument,
                runtime,
                cdenv_owned_targets,
            )?;
        }
        if let Some(tmpfs) = option_value(arguments, index, "--tmpfs", None) {
            let target = tmpfs.split(':').next().unwrap_or_default();
            reject_mount_target(
                target,
                &property,
                diagnostic_argument,
                runtime,
                cdenv_owned_targets,
            )?;
        }
    }
    Ok(())
}

fn reject_mount_target(
    target: &str,
    property: &str,
    argument: &str,
    runtime: &RuntimePlan,
    cdenv_owned_targets: &[ContainerPath],
) -> Result<(), DockerOptionError> {
    let target = ContainerPath::parse(target).map_err(|_| {
        DockerOptionError::new(
            property,
            argument,
            DockerOptionErrorKind::InvalidMountArgument,
            "run argument mount target must be an absolute container path",
        )
    })?;
    let conflicts = std::iter::once(&runtime.workspace.mount.target)
        .chain(runtime.mounts.iter().map(|mount| &mount.target))
        .chain(cdenv_owned_targets)
        .any(|owned| paths_overlap(target.as_str(), owned.as_str()));
    if conflicts {
        Err(DockerOptionError::new(
            property,
            argument,
            DockerOptionErrorKind::MountTarget,
            "run argument mount overlaps a workspace or cdenv-owned target",
        ))
    } else {
        Ok(())
    }
}

fn reserved_long(argument: &str, names: &[&str]) -> bool {
    names.iter().any(|name| {
        argument == *name
            || argument
                .strip_prefix(name)
                .is_some_and(|suffix| suffix.starts_with('='))
    })
}

fn reserved_short(argument: &str, names: &[char]) -> bool {
    argument
        .strip_prefix('-')
        .filter(|value| !value.starts_with('-'))
        .and_then(|value| value.chars().next())
        .is_some_and(|name| names.contains(&name))
}

fn reserved_attach_short(argument: &str) -> bool {
    let Some(flags) = argument
        .strip_prefix('-')
        .filter(|value| !value.starts_with('-'))
    else {
        return false;
    };
    let flags = flags.split('=').next().unwrap_or_default();
    !flags.is_empty()
        && flags
            .chars()
            .all(|flag| matches!(flag, 'a' | 'd' | 'i' | 't'))
}

fn option_value<'a>(
    arguments: &'a [String],
    index: usize,
    long: &str,
    short: Option<char>,
) -> Option<&'a str> {
    let argument = arguments.get(index)?;
    if argument == long || short.is_some_and(|short| argument == &format!("-{short}")) {
        return arguments.get(index + 1).map(String::as_str);
    }
    if let Some(value) = argument.strip_prefix(&format!("{long}=")) {
        return Some(value);
    }
    short
        .and_then(|short| argument.strip_prefix(&format!("-{short}")))
        .filter(|value| !value.is_empty())
}

fn cdenv_label(value: &str) -> bool {
    value
        .split_once('=')
        .map_or(value, |(name, _)| name)
        .starts_with("cdenv.")
}

fn mount_target(value: &str) -> Option<&str> {
    let mut target = None;
    for field in value.split(',') {
        let Some((name, value)) = field.split_once('=') else {
            continue;
        };
        if matches!(name.trim(), "target" | "dst" | "destination") {
            if target.is_some() {
                return None;
            }
            target = Some(value.trim());
        }
    }
    target.filter(|target| !target.is_empty())
}

fn volume_target(value: &str) -> Option<&str> {
    let mut fields = value.rsplit(':');
    let last = fields.next()?;
    if last.starts_with('/') {
        return Some(last);
    }
    fields.next().filter(|target| target.starts_with('/'))
}

fn paths_overlap(left: &str, right: &str) -> bool {
    path_contains(left, right) || path_contains(right, left)
}

fn path_contains(parent: &str, child: &str) -> bool {
    parent == child
        || (child.starts_with(parent)
            && (parent == "/" || child.as_bytes().get(parent.len()) == Some(&b'/')))
}

fn resolve_repository_path(base: &str, value: &str) -> Result<RepositoryPath, RepositoryPathError> {
    if value.starts_with('/') || value.contains('\\') || value.chars().any(char::is_control) {
        return Err(RepositoryPathError);
    }
    let mut components = Vec::new();
    for component in base.split('/').chain(value.split('/')) {
        match component {
            "" | "." => {}
            ".." => {
                components.pop().ok_or(RepositoryPathError)?;
            }
            component => components.push(component),
        }
    }
    Ok(RepositoryPath(if components.is_empty() {
        ".".to_owned()
    } else {
        components.join("/")
    }))
}

/// Stable conflict category for Docker-shaped options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DockerOptionErrorKind {
    /// Dockerfile or context escaped the checkout.
    PathOutsideCheckout,
    /// Host substitution failed.
    Substitution,
    /// Dockerfile, target, tag, output, IID, or metadata output is cdenv-owned.
    BuildInvariant,
    /// The positional main build context is cdenv-owned.
    BuildContext,
    /// Stable identity labels are cdenv-owned.
    IdentityLabel,
    /// Required container name is cdenv-owned.
    ContainerName,
    /// Automatic removal would violate lifecycle management.
    AutoRemove,
    /// Container/remote user is planned separately.
    ContainerUser,
    /// Generated Feature entrypoint handling is cdenv-owned.
    Entrypoint,
    /// Attach/detach/stdin/TTY mode is cdenv-owned.
    AttachmentMode,
    /// Mount argument could not be safely interpreted.
    InvalidMountArgument,
    /// Mount overlaps a workspace or injected asset target.
    MountTarget,
}

/// Exact Docker option conflict. The offending token is retained as required by the V1 contract.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{property_path}: conflicting Docker argument `{argument}`: {message}")]
pub struct DockerOptionError {
    /// Exact source property/index.
    pub property_path: String,
    /// Exact offending token, or an empty string for path-only failures.
    pub argument: String,
    /// Stable conflict category.
    pub kind: DockerOptionErrorKind,
    message: &'static str,
}

impl DockerOptionError {
    fn new(
        property_path: impl Into<String>,
        argument: impl Into<String>,
        kind: DockerOptionErrorKind,
        message: &'static str,
    ) -> Self {
        Self {
            property_path: property_path.into(),
            argument: argument.into(),
            kind,
            message,
        }
    }

    fn without_argument(
        property_path: impl Into<String>,
        kind: DockerOptionErrorKind,
        message: &'static str,
    ) -> Self {
        Self::new(property_path, "", kind, message)
    }
}
