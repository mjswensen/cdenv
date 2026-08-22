//! Specification-facing Docker CLI pull, `BuildKit` build, and create adapter.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use cdenv_core::{
    ContainerId, ContainerIdError, GenerationId, InstallationId, ProfileId, WorkspaceName,
};
use cdenv_devcontainer::{
    CreateOptionsPlan, DockerOptionError, DockerfileBuildPlan, GpuAccessIntent, MountKind,
    PlannedMount, PortPlan, RuntimePlan, validate_build_options_at_boundary,
    validate_create_options_at_boundary,
};
use getrandom::fill;
use thiserror::Error;

use crate::{
    CancellationToken, DockerEndpoint, OperationId, ProcessDeadline, ProcessEnvironmentVariable,
    ProcessError, ProcessRequest, ProcessRunner,
};

/// Default maximum number of filesystem entries accepted in a build context.
pub const DEFAULT_MAXIMUM_CONTEXT_ENTRIES: usize = 100_000;
/// Default maximum aggregate regular-file bytes accepted in a build context.
pub const DEFAULT_MAXIMUM_CONTEXT_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Default maximum aggregate bytes accepted for generated context material.
pub const DEFAULT_MAXIMUM_GENERATED_BYTES: u64 = 64 * 1024 * 1024;

/// Stable cdenv Docker resource identity applied directly by build/create commands.
#[derive(Clone, Copy, Debug)]
pub struct DockerResourceIdentity<'a> {
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Positive environment generation.
    pub generation: GenerationId,
    /// Versioned compatibility profile.
    pub profile: &'a ProfileId,
}

impl DockerResourceIdentity<'_> {
    fn labels(self) -> [String; 4] {
        [
            format!("cdenv.installation={}", self.installation),
            format!("cdenv.workspace={}", self.workspace),
            format!("cdenv.generation={}", self.generation),
            format!("cdenv.profile={}", self.profile),
        ]
    }
}

/// Explicit limits for repository and generated Docker build contexts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DockerContextLimits {
    /// Maximum files, directories, and symlinks below the context root.
    pub maximum_entries: usize,
    /// Maximum aggregate regular-file size for a repository context.
    pub maximum_bytes: u64,
    /// Maximum aggregate bytes supplied as generated files.
    pub maximum_generated_bytes: u64,
}

impl Default for DockerContextLimits {
    fn default() -> Self {
        Self {
            maximum_entries: DEFAULT_MAXIMUM_CONTEXT_ENTRIES,
            maximum_bytes: DEFAULT_MAXIMUM_CONTEXT_BYTES,
            maximum_generated_bytes: DEFAULT_MAXIMUM_GENERATED_BYTES,
        }
    }
}

/// One generated, relative build-context file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedContextFile {
    /// Slash/platform-relative path below the generated context root.
    pub path: PathBuf,
    /// Exact file contents.
    pub contents: Vec<u8>,
}

/// Build context selection. Generated material is materialized only below the adapter temp root.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum DockerBuildContext {
    /// Use the repository context selected by the typed Dockerfile plan.
    #[default]
    Repository,
    /// Use an entirely generated context.
    Generated(Vec<GeneratedContextFile>),
}

/// Dockerfile selection for a `BuildKit` build.
#[derive(Clone, Copy, Debug, Default)]
pub enum DockerfileInput<'a> {
    /// Use the repository Dockerfile selected by the typed plan.
    #[default]
    Repository,
    /// Materialize these exact Dockerfile bytes below the adapter temp root.
    Generated(&'a [u8]),
}

/// Borrowed `BuildKit` operation input.
pub struct DockerBuildRequest<'a> {
    /// Typed Dockerfile build plan.
    pub plan: &'a DockerfileBuildPlan,
    /// Canonical repository checkout root.
    pub checkout: &'a Path,
    /// Operation-owned result image tag.
    pub tag: &'a str,
    /// Stable labels attached to the result image.
    pub identity: DockerResourceIdentity<'a>,
    /// Repository or generated context.
    pub context: &'a DockerBuildContext,
    /// Repository or generated Dockerfile.
    pub dockerfile: DockerfileInput<'a>,
}

/// Borrowed container-create operation input.
pub struct DockerCreateRequest<'a> {
    /// Operation-owned container name.
    pub name: &'a str,
    /// Image reference or adapter-claimed image ID/tag.
    pub image: &'a str,
    /// Stable labels attached at creation time.
    pub identity: DockerResourceIdentity<'a>,
    /// Effective create-time runtime settings.
    pub runtime: &'a RuntimePlan,
    /// Validated ordered `runArgs` passthrough.
    pub options: &'a CreateOptionsPlan,
    /// Create-time publication requests.
    pub ports: &'a PortPlan,
    /// Explicit host-requirement GPU grant decision.
    pub gpu_access: GpuAccessIntent,
    /// Container command appended after the image, in exact order.
    pub command: &'a [String],
    /// Container targets owned by injected cdenv assets.
    pub cdenv_owned_targets: &'a [cdenv_devcontainer::ContainerPath],
}

/// Claimed image identifier emitted through a `BuildKit` IID file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageId(String);

impl ImageId {
    /// Parses `sha256:` followed by exactly 64 lowercase hexadecimal characters.
    ///
    /// # Errors
    ///
    /// Rejects every abbreviated, uppercase, or non-SHA256 image identifier.
    pub fn parse(value: &str) -> Result<Self, DockerCliError> {
        let Some(digest) = value.strip_prefix("sha256:") else {
            return Err(DockerCliError::InvalidImageId);
        };
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(DockerCliError::InvalidImageId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the canonical `sha256:...` identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Successful pull claim. Later Bollard inspection remains authoritative.
#[derive(Debug)]
pub struct DockerPullClaim {
    /// Exact requested image reference.
    pub image: String,
    /// Subprocess operation identity.
    pub operation_id: OperationId,
    /// Restricted bounded log path.
    pub log_path: PathBuf,
}

/// Successful build claim. Later Bollard inspection must verify the ID and labels.
#[derive(Debug)]
pub struct DockerBuildClaim {
    /// `BuildKit`'s claimed image ID.
    pub image_id: ImageId,
    /// Operation-owned result tag.
    pub tag: String,
    /// Subprocess operation identity.
    pub operation_id: OperationId,
    /// Restricted bounded log path.
    pub log_path: PathBuf,
}

/// Successful create claim. Later Bollard inspection must verify all create settings.
#[derive(Debug)]
pub struct DockerCreateClaim {
    /// Docker CLI's claimed full container ID.
    pub container_id: ContainerId,
    /// Subprocess operation identity.
    pub operation_id: OperationId,
    /// Restricted bounded log path.
    pub log_path: PathBuf,
}

/// Specification-facing Docker CLI adapter bound to one resolved daemon.
#[derive(Clone, Debug)]
pub struct DockerCliAdapter {
    executable: PathBuf,
    endpoint: DockerEndpoint,
    runner: ProcessRunner,
    environment: Vec<(OsString, OsString)>,
    temporary_root: PathBuf,
    context_limits: DockerContextLimits,
}

impl DockerCliAdapter {
    /// Creates an adapter using `docker`, the process environment, and a managed temp root.
    #[must_use]
    pub fn system(
        endpoint: DockerEndpoint,
        runner: ProcessRunner,
        temporary_root: PathBuf,
    ) -> Self {
        Self::new(
            PathBuf::from("docker"),
            endpoint,
            runner,
            env::vars_os().collect(),
            temporary_root,
        )
    }

    /// Creates an adapter with an explicit executable and complete base environment.
    #[must_use]
    pub fn new(
        executable: PathBuf,
        endpoint: DockerEndpoint,
        runner: ProcessRunner,
        mut environment: Vec<(OsString, OsString)>,
        temporary_root: PathBuf,
    ) -> Self {
        environment.retain(|(name, _)| name != "DOCKER_HOST" && name != "DOCKER_BUILDKIT");
        environment.push((
            OsString::from("DOCKER_HOST"),
            endpoint.docker_host().to_owned(),
        ));
        Self {
            executable,
            endpoint,
            runner,
            environment,
            temporary_root,
            context_limits: DockerContextLimits::default(),
        }
    }

    /// Overrides context bounds.
    #[must_use]
    pub const fn with_context_limits(mut self, limits: DockerContextLimits) -> Self {
        self.context_limits = limits;
        self
    }

    /// Returns the exact daemon endpoint injected into every Docker process.
    #[must_use]
    pub const fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    /// Pulls an exact image reference with Docker's normal credential behavior.
    ///
    /// # Errors
    ///
    /// Returns validation, process, cancellation, or non-zero-exit failures.
    pub async fn pull(
        &self,
        image: &str,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<DockerPullClaim, DockerCliError> {
        validate_docker_value("image", image)?;
        let arguments = pull_arguments(image)?;
        let result = self
            .run("docker-pull", &arguments, cwd, &[], false, cancellation)
            .await?;
        ensure_success("pull", &result)?;
        Ok(DockerPullClaim {
            image: image.to_owned(),
            operation_id: result.operation_id,
            log_path: result.log_path,
        })
    }

    /// Builds a Dockerfile plan with `BuildKit` and an operation-owned IID file/tag.
    ///
    /// # Errors
    ///
    /// Returns option conflicts, unsafe/oversized contexts, materialization failures, process
    /// failures, non-zero exits, or malformed `BuildKit` image IDs.
    pub async fn build(
        &self,
        request: &DockerBuildRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<DockerBuildClaim, DockerCliError> {
        validate_build_options_at_boundary(&request.plan.options)?;
        validate_docker_value("image tag", request.tag)?;
        let temporary = TemporaryOperationDirectory::create(&self.temporary_root)?;
        let checkout = canonical_directory(request.checkout, "checkout")?;
        let canonical_temporary =
            canonical_directory(temporary.path(), "Docker temporary directory")?;
        if canonical_temporary.starts_with(&checkout) {
            return Err(DockerCliError::PathEscapesCheckout {
                role: "generated Docker material must remain outside the checkout",
                path: canonical_temporary,
            });
        }
        let (context, generated_context) = prepare_context(
            request.context,
            request.plan,
            &checkout,
            temporary.path(),
            self.context_limits,
        )?;
        if let DockerfileInput::Generated(contents) = request.dockerfile
            && u64::try_from(contents.len()).unwrap_or(u64::MAX)
                > self.context_limits.maximum_generated_bytes
        {
            return Err(DockerCliError::ContextTooLarge {
                bytes: u64::try_from(contents.len()).unwrap_or(u64::MAX),
                maximum: self.context_limits.maximum_generated_bytes,
            });
        }
        let dockerfile = prepare_dockerfile(
            request.dockerfile,
            request.plan,
            &checkout,
            temporary.path(),
        )?;
        if matches!(request.dockerfile, DockerfileInput::Repository) {
            ensure_contained(&checkout, &dockerfile, "Dockerfile")?;
        }
        validate_prepared_context(&context, self.context_limits, generated_context)?;
        let iid_file = temporary.path().join("image.id");
        let arguments = build_arguments(
            request.plan,
            &dockerfile,
            &context,
            request.tag,
            &iid_file,
            request.identity,
        )?;
        let redactions = request
            .plan
            .arguments
            .values()
            .map(|value| value.expose().as_bytes())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let result = self
            .run(
                "docker-build",
                &arguments,
                &checkout,
                &redactions,
                true,
                cancellation,
            )
            .await?;
        ensure_success("build", &result)?;
        let image_id =
            fs::read_to_string(&iid_file).map_err(|source| DockerCliError::ReadClaim {
                path: iid_file,
                source,
            })?;
        let image_id = ImageId::parse(image_id.trim())?;
        Ok(DockerBuildClaim {
            image_id,
            tag: request.tag.to_owned(),
            operation_id: result.operation_id,
            log_path: result.log_path,
        })
    }

    /// Creates, but does not start, one image-scenario container.
    ///
    /// # Errors
    ///
    /// Returns boundary option conflicts, invalid owned values, process/non-zero-exit failures, or
    /// an abbreviated/malformed claimed container ID.
    pub async fn create(
        &self,
        request: &DockerCreateRequest<'_>,
        cwd: &Path,
        cancellation: &CancellationToken,
    ) -> Result<DockerCreateClaim, DockerCliError> {
        validate_create_options_at_boundary(
            &request.options.run_arguments,
            request.runtime,
            request.cdenv_owned_targets,
        )?;
        validate_docker_value("container name", request.name)?;
        validate_docker_value("image", request.image)?;
        let arguments = create_arguments(request)?;
        let redactions = request
            .runtime
            .environment
            .container()
            .values()
            .map(|value| value.expose().as_bytes())
            .filter(|value| !value.is_empty())
            .collect::<Vec<_>>();
        let result = self
            .run(
                "docker-create",
                &arguments,
                cwd,
                &redactions,
                false,
                cancellation,
            )
            .await?;
        ensure_success("create", &result)?;
        let output = std::str::from_utf8(result.stdout.as_bytes())
            .map_err(|_| DockerCliError::InvalidContainerId)?;
        let container_id = ContainerId::parse(output.trim())?;
        Ok(DockerCreateClaim {
            container_id,
            operation_id: result.operation_id,
            log_path: result.log_path,
        })
    }

    async fn run(
        &self,
        operation: &'static str,
        arguments: &[OsString],
        cwd: &Path,
        redactions: &[&[u8]],
        buildkit: bool,
        cancellation: &CancellationToken,
    ) -> Result<crate::ProcessResult, DockerCliError> {
        let argument_refs = arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>();
        let mut environment = self
            .environment
            .iter()
            .map(|(name, value)| ProcessEnvironmentVariable {
                name,
                value,
                sensitive: false,
            })
            .collect::<Vec<_>>();
        if buildkit {
            environment.push(ProcessEnvironmentVariable {
                name: OsStr::new("DOCKER_BUILDKIT"),
                value: OsStr::new("1"),
                sensitive: false,
            });
        }
        self.runner
            .run(
                &ProcessRequest {
                    operation,
                    executable: &self.executable,
                    arguments: &argument_refs,
                    cwd,
                    environment: &environment,
                    redactions,
                    deadline: ProcessDeadline::Unbounded,
                },
                cancellation,
            )
            .await
            .map_err(DockerCliError::Process)
    }
}

/// Builds the exact `docker pull` argv, excluding argv zero.
///
/// # Errors
///
/// Rejects empty, option-shaped, or control-containing references.
pub fn pull_arguments(image: &str) -> Result<Vec<OsString>, DockerCliError> {
    validate_docker_value("image", image)?;
    Ok(vec![OsString::from("pull"), OsString::from(image)])
}

/// Builds exact `BuildKit` argv while preserving passthrough option order.
///
/// # Errors
///
/// Defensively rejects reserved passthrough arguments and invalid owned values.
pub fn build_arguments(
    plan: &DockerfileBuildPlan,
    dockerfile: &Path,
    context: &Path,
    tag: &str,
    iid_file: &Path,
    identity: DockerResourceIdentity<'_>,
) -> Result<Vec<OsString>, DockerCliError> {
    validate_build_options_at_boundary(&plan.options)?;
    validate_docker_value("image tag", tag)?;
    let mut arguments = Vec::new();
    arguments.push(OsString::from("build"));
    arguments.extend(plan.options.iter().map(OsString::from));
    arguments.push(OsString::from("--file"));
    arguments.push(dockerfile.as_os_str().to_owned());
    if let Some(target) = &plan.target {
        arguments.push(OsString::from("--target"));
        arguments.push(OsString::from(target));
    }
    for (name, value) in &plan.arguments {
        arguments.push(OsString::from("--build-arg"));
        arguments.push(OsString::from(format!("{name}={}", value.expose())));
    }
    for source in &plan.cache_from {
        arguments.push(OsString::from("--cache-from"));
        arguments.push(OsString::from(source));
    }
    for label in identity.labels() {
        arguments.push(OsString::from("--label"));
        arguments.push(OsString::from(label));
    }
    arguments.push(OsString::from("--tag"));
    arguments.push(OsString::from(tag));
    arguments.push(OsString::from("--iidfile"));
    arguments.push(iid_file.as_os_str().to_owned());
    arguments.push(context.as_os_str().to_owned());
    Ok(arguments)
}

/// Builds exact `docker create` argv while preserving `runArgs` ordering.
///
/// # Errors
///
/// Defensively rejects reserved passthrough arguments and invalid owned values.
pub fn create_arguments(
    request: &DockerCreateRequest<'_>,
) -> Result<Vec<OsString>, DockerCliError> {
    validate_create_options_at_boundary(
        &request.options.run_arguments,
        request.runtime,
        request.cdenv_owned_targets,
    )?;
    validate_docker_value("container name", request.name)?;
    validate_docker_value("image", request.image)?;
    let mut arguments = Vec::new();
    arguments.push(OsString::from("create"));
    arguments.extend(request.options.run_arguments.iter().map(OsString::from));
    arguments.push(OsString::from("--name"));
    arguments.push(OsString::from(request.name));
    for label in request.identity.labels() {
        arguments.push(OsString::from("--label"));
        arguments.push(OsString::from(label));
    }
    for publication in &request.ports.publications {
        arguments.push(OsString::from("--publish"));
        arguments.push(OsString::from(&publication.argument));
    }
    if request.gpu_access == GpuAccessIntent::Requested {
        arguments.push(OsString::from("--gpus"));
        arguments.push(OsString::from("all"));
    }
    arguments.push(OsString::from("--mount"));
    arguments.push(OsString::from(mount_argument(
        &request.runtime.workspace.mount,
    )));
    for mount in &request.runtime.mounts {
        arguments.push(OsString::from("--mount"));
        arguments.push(OsString::from(mount_argument(mount)));
    }
    for (name, value) in request.runtime.environment.container() {
        arguments.push(OsString::from("--env"));
        arguments.push(OsString::from(format!("{name}={}", value.expose())));
    }
    arguments.push(OsString::from("--user"));
    arguments.push(OsString::from(request.runtime.container_user.as_str()));
    arguments.push(OsString::from("--workdir"));
    arguments.push(OsString::from(request.runtime.workspace.folder.as_str()));
    if request.runtime.init {
        arguments.push(OsString::from("--init"));
    }
    if request.runtime.privileged {
        arguments.push(OsString::from("--privileged"));
    }
    for capability in &request.runtime.cap_add {
        arguments.push(OsString::from("--cap-add"));
        arguments.push(OsString::from(capability));
    }
    for option in &request.runtime.security_opt {
        arguments.push(OsString::from("--security-opt"));
        arguments.push(OsString::from(option));
    }
    arguments.push(OsString::from(request.image));
    arguments.extend(request.command.iter().map(OsString::from));
    Ok(arguments)
}

fn mount_argument(mount: &PlannedMount) -> String {
    let kind = match mount.kind {
        MountKind::Bind => "bind",
        MountKind::Volume => "volume",
    };
    let mut value = format!("type={kind}");
    if let Some(source) = &mount.source {
        value.push_str(",source=");
        value.push_str(source);
    }
    value.push_str(",target=");
    value.push_str(mount.target.as_str());
    for option in &mount.options {
        value.push(',');
        value.push_str(&option.name);
        if let Some(option_value) = &option.value {
            value.push('=');
            value.push_str(option_value);
        }
    }
    value
}

fn prepare_context(
    input: &DockerBuildContext,
    plan: &DockerfileBuildPlan,
    checkout: &Path,
    temporary: &Path,
    limits: DockerContextLimits,
) -> Result<(PathBuf, bool), DockerCliError> {
    match input {
        DockerBuildContext::Repository => {
            let context = checkout.join(plan.context.as_str());
            let context = canonical_directory(&context, "build context")?;
            ensure_contained(checkout, &context, "build context")?;
            Ok((context, false))
        }
        DockerBuildContext::Generated(files) => {
            if files.len() > limits.maximum_entries {
                return Err(DockerCliError::TooManyContextEntries {
                    entries: files.len(),
                    maximum: limits.maximum_entries,
                });
            }
            let context = temporary.join("context");
            fs::create_dir(&context).map_err(|source| DockerCliError::Materialize {
                path: context.clone(),
                source,
            })?;
            let mut bytes = 0_u64;
            for file in files {
                validate_relative_generated_path(&file.path)?;
                bytes = bytes
                    .checked_add(u64::try_from(file.contents.len()).unwrap_or(u64::MAX))
                    .ok_or(DockerCliError::ContextTooLarge {
                        bytes: u64::MAX,
                        maximum: limits.maximum_generated_bytes,
                    })?;
                if bytes > limits.maximum_generated_bytes {
                    return Err(DockerCliError::ContextTooLarge {
                        bytes,
                        maximum: limits.maximum_generated_bytes,
                    });
                }
                let path = context.join(&file.path);
                if let Some(parent) = path.parent() {
                    fs::create_dir_all(parent).map_err(|source| DockerCliError::Materialize {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                fs::write(&path, &file.contents).map_err(|source| DockerCliError::Materialize {
                    path: path.clone(),
                    source,
                })?;
            }
            Ok((context, true))
        }
    }
}

fn prepare_dockerfile(
    input: DockerfileInput<'_>,
    plan: &DockerfileBuildPlan,
    checkout: &Path,
    temporary: &Path,
) -> Result<PathBuf, DockerCliError> {
    match input {
        DockerfileInput::Repository => {
            let dockerfile = checkout.join(plan.dockerfile.as_str());
            dockerfile
                .canonicalize()
                .map_err(|source| DockerCliError::InspectPath {
                    role: "Dockerfile",
                    path: dockerfile,
                    source,
                })
        }
        DockerfileInput::Generated(contents) => {
            let path = temporary.join("Dockerfile");
            fs::write(&path, contents).map_err(|source| DockerCliError::Materialize {
                path: path.clone(),
                source,
            })?;
            Ok(path)
        }
    }
}

/// Validates only cdenv-materialized context trees.
///
/// Repository contexts are passed directly to Docker: Docker owns `.dockerignore` and every
/// context-selection rule, while generated contexts have no repository ignore semantics and are
/// bounded before cdenv materializes them.
fn validate_prepared_context(
    root: &Path,
    limits: DockerContextLimits,
    generated: bool,
) -> Result<(), DockerCliError> {
    if generated {
        validate_context_tree(root, limits)
    } else {
        Ok(())
    }
}

fn validate_context_tree(root: &Path, limits: DockerContextLimits) -> Result<(), DockerCliError> {
    let canonical_root = canonical_directory(root, "build context")?;
    let mut pending = vec![canonical_root.clone()];
    let mut entries = 0_usize;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| DockerCliError::InspectPath {
            role: "build context",
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| DockerCliError::InspectPath {
                role: "build context",
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            entries = entries.saturating_add(1);
            if entries > limits.maximum_entries {
                return Err(DockerCliError::TooManyContextEntries {
                    entries,
                    maximum: limits.maximum_entries,
                });
            }
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| DockerCliError::InspectPath {
                    role: "build context entry",
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                let target = path
                    .canonicalize()
                    .map_err(|source| DockerCliError::InspectPath {
                        role: "build context symlink",
                        path: path.clone(),
                        source,
                    })?;
                ensure_contained(&canonical_root, &target, "build context symlink")?;
            } else if metadata.is_dir() {
                pending.push(path);
            } else if metadata.is_file() {
                bytes = bytes.saturating_add(metadata.len());
                let maximum = limits.maximum_generated_bytes;
                if bytes > maximum {
                    return Err(DockerCliError::ContextTooLarge { bytes, maximum });
                }
            } else {
                return Err(DockerCliError::UnsupportedContextEntry { path });
            }
        }
    }
    Ok(())
}

fn validate_relative_generated_path(path: &Path) -> Result<(), DockerCliError> {
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        Err(DockerCliError::InvalidGeneratedPath {
            path: path.to_path_buf(),
        })
    } else {
        Ok(())
    }
}

fn canonical_directory(path: &Path, role: &'static str) -> Result<PathBuf, DockerCliError> {
    let canonical = path
        .canonicalize()
        .map_err(|source| DockerCliError::InspectPath {
            role,
            path: path.to_path_buf(),
            source,
        })?;
    if canonical.is_dir() {
        Ok(canonical)
    } else {
        Err(DockerCliError::NotDirectory {
            role,
            path: canonical,
        })
    }
}

fn ensure_contained(root: &Path, path: &Path, role: &'static str) -> Result<(), DockerCliError> {
    if path.starts_with(root) {
        Ok(())
    } else {
        Err(DockerCliError::PathEscapesCheckout {
            role,
            path: path.to_path_buf(),
        })
    }
}

fn validate_docker_value(role: &'static str, value: &str) -> Result<(), DockerCliError> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        Err(DockerCliError::InvalidOwnedValue {
            role,
            value: value.to_owned(),
        })
    } else {
        Ok(())
    }
}

fn ensure_success(
    operation: &'static str,
    result: &crate::ProcessResult,
) -> Result<(), DockerCliError> {
    if result.status.success() {
        Ok(())
    } else {
        Err(DockerCliError::Exited {
            operation,
            code: result.status.code(),
            stderr: bounded_summary(result.stderr.as_bytes()),
            log_path: result.log_path.clone(),
        })
    }
}

fn bounded_summary(bytes: &[u8]) -> String {
    const MAXIMUM: usize = 1024;
    let accepted = bytes.len().min(MAXIMUM);
    String::from_utf8_lossy(&bytes[..accepted]).replace(char::is_control, " ")
}

struct TemporaryOperationDirectory(PathBuf);

impl TemporaryOperationDirectory {
    fn create(root: &Path) -> Result<Self, DockerCliError> {
        fs::create_dir_all(root).map_err(|source| DockerCliError::Materialize {
            path: root.to_path_buf(),
            source,
        })?;
        for _ in 0..16 {
            let mut random = [0_u8; 16];
            fill(&mut random).map_err(DockerCliError::Random)?;
            let path = root.join(format!("docker-{}", hex::encode(random)));
            match create_private_directory(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {}
                Err(source) => return Err(DockerCliError::Materialize { path, source }),
            }
        }
        Err(DockerCliError::TemporaryCollision)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryOperationDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}

#[cfg(not(unix))]
fn create_private_directory(path: &Path) -> io::Result<()> {
    fs::create_dir(path)
}

/// Docker CLI adapter failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum DockerCliError {
    /// Planner-owned Docker options failed defensive adapter validation.
    #[error(transparent)]
    ReservedOption(#[from] DockerOptionError),
    /// Direct process execution failed.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// An adapter-owned name, tag, or image was unsafe/ambiguous as a Docker argument.
    #[error("invalid Docker {role} {value:?}")]
    InvalidOwnedValue {
        /// Value role.
        role: &'static str,
        /// Rejected value.
        value: String,
    },
    /// A required checkout/context path could not be inspected.
    #[error("cannot inspect {role} path {path:?}: {source}")]
    InspectPath {
        /// Path role.
        role: &'static str,
        /// Inspected path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: io::Error,
    },
    /// A required directory was another file kind.
    #[error("{role} is not a directory: {path:?}")]
    NotDirectory {
        /// Path role.
        role: &'static str,
        /// Rejected path.
        path: PathBuf,
    },
    /// A Dockerfile, context, or symlink escaped its allowed root.
    #[error("{role} escapes its allowed checkout/context root: {path:?}")]
    PathEscapesCheckout {
        /// Path role.
        role: &'static str,
        /// Escaping path.
        path: PathBuf,
    },
    /// A generated context path was absolute or contained traversal/prefix components.
    #[error("generated context path must be relative and traversal-free: {path:?}")]
    InvalidGeneratedPath {
        /// Rejected path.
        path: PathBuf,
    },
    /// A context contained a device, socket, FIFO, or other unsupported entry.
    #[error("unsupported build context entry: {path:?}")]
    UnsupportedContextEntry {
        /// Rejected path.
        path: PathBuf,
    },
    /// Context entry count exceeded its configured bound.
    #[error("build context contains {entries} entries; configured maximum is {maximum}")]
    TooManyContextEntries {
        /// Observed count.
        entries: usize,
        /// Configured maximum.
        maximum: usize,
    },
    /// Context bytes exceeded their configured bound.
    #[error("build context contains at least {bytes} bytes; configured maximum is {maximum}")]
    ContextTooLarge {
        /// Observed bytes.
        bytes: u64,
        /// Configured maximum.
        maximum: u64,
    },
    /// Generated temporary material could not be created.
    #[error("cannot materialize Docker build input {path:?}: {source}")]
    Materialize {
        /// Materialized path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: io::Error,
    },
    /// Secure operation-directory random generation failed.
    #[error("cannot generate a Docker operation directory name: {0}")]
    Random(getrandom::Error),
    /// Repeated secure random operation-directory names collided.
    #[error("cannot allocate a unique Docker operation directory")]
    TemporaryCollision,
    /// Docker exited unsuccessfully.
    #[error("Docker {operation} failed with exit code {code:?}: {stderr} (log: {log_path:?})")]
    Exited {
        /// Operation kind.
        operation: &'static str,
        /// Portable status code.
        code: Option<i32>,
        /// Bounded/redacted diagnostic.
        stderr: String,
        /// Restricted operation log.
        log_path: PathBuf,
    },
    /// `BuildKit`'s IID file could not be read.
    #[error("cannot read BuildKit image claim {path:?}: {source}")]
    ReadClaim {
        /// IID file path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: io::Error,
    },
    /// `BuildKit` did not claim a canonical SHA256 image ID.
    #[error("BuildKit did not return a canonical sha256 image ID")]
    InvalidImageId,
    /// Docker create did not claim a full canonical container ID.
    #[error("Docker create did not return a full canonical container ID")]
    InvalidContainerId,
    /// Docker returned a malformed full container ID.
    #[error(transparent)]
    ContainerId(#[from] ContainerIdError),
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn image_id_requires_a_full_sha256_claim() {
        let value = format!("sha256:{}", "a".repeat(64));

        let result = ImageId::parse(&value).expect("full image ID");

        assert_eq!(result.as_str(), value);
    }

    #[test]
    fn generated_context_rejects_parent_traversal() {
        let result = validate_relative_generated_path(Path::new("../Dockerfile"));

        assert!(matches!(
            result,
            Err(DockerCliError::InvalidGeneratedPath { .. })
        ));
    }

    #[test]
    fn pull_builder_uses_no_shell_or_implicit_flags() {
        let arguments = pull_arguments("example.invalid/base:latest").expect("valid image");

        assert_eq!(arguments, ["pull", "example.invalid/base:latest"]);
    }

    #[test]
    fn context_size_bound_rejects_oversized_material_before_docker() {
        let temporary = tempfile::tempdir().expect("temporary context");
        fs::write(temporary.path().join("large"), [0_u8; 9]).expect("context file");
        let limits = DockerContextLimits {
            maximum_entries: 2,
            maximum_bytes: 8,
            maximum_generated_bytes: 8,
        };

        let result = validate_context_tree(temporary.path(), limits);

        assert!(matches!(
            result,
            Err(DockerCliError::ContextTooLarge { .. })
        ));
    }

    #[test]
    fn repository_context_defers_ignored_size_semantics_to_docker() {
        let temporary = tempfile::tempdir().expect("temporary context");
        fs::write(temporary.path().join(".dockerignore"), "ignored-large\n").expect("ignore file");
        fs::write(temporary.path().join("ignored-large"), [0_u8; 9]).expect("ignored large file");
        let limits = DockerContextLimits {
            maximum_entries: 1,
            maximum_bytes: 8,
            maximum_generated_bytes: 8,
        };

        validate_prepared_context(temporary.path(), limits, false)
            .expect("Docker owns repository ignore matching");
    }

    #[cfg(unix)]
    #[test]
    fn context_symlink_must_not_escape_the_context_root() {
        use std::os::unix::fs::symlink;

        let temporary = tempfile::tempdir().expect("temporary fixture");
        let context = temporary.path().join("context");
        fs::create_dir(&context).expect("context");
        fs::write(temporary.path().join("outside"), "outside").expect("outside file");
        symlink("../outside", context.join("escape")).expect("escaping symlink");

        let result = validate_context_tree(&context, DockerContextLimits::default());

        assert!(matches!(
            result,
            Err(DockerCliError::PathEscapesCheckout {
                role: "build context symlink",
                ..
            })
        ));
    }
}
