//! Docker Compose V2 adapter for isolated cdenv projects.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cdenv_core::{ContainerId, ContainerIdError};
use cdenv_devcontainer::{ComposeModel, ComposePlan, ComposeServiceModel};
use getrandom::fill;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::{
    CancellationToken, DockerEndpoint, ImageId, ProcessDeadline, ProcessEnvironmentVariable,
    ProcessError, ProcessRequest, ProcessResult, ProcessRunner,
};

/// Explicit files, project identity, and working directory shared by Compose commands.
#[derive(Clone, Copy, Debug)]
pub struct ComposeProject<'a> {
    /// Ordered, canonical Compose files from the checkout.
    pub files: &'a [PathBuf],
    /// Stable cdenv Compose project name.
    pub project_name: &'a str,
    /// Canonical checkout/configuration working directory.
    pub working_directory: &'a Path,
}

/// Request to pull/build and identify the exact declared primary-service base image.
pub struct ComposeBaseRequest<'a> {
    /// Compose project and files without a cdenv final override.
    pub project: ComposeProject<'a>,
    /// Exact primary service.
    pub service: &'a str,
    /// Whether the resolved service declares `build`.
    pub has_build: bool,
    /// Operation-owned tag applied to the exact Compose-produced image ID.
    pub base_tag: &'a str,
    /// Disable the Compose build cache.
    pub no_cache: bool,
}

/// Request to reconcile an isolated project using a secret-bearing canonical override.
pub struct ComposeUpRequest<'a> {
    /// Explicit base project inputs.
    pub project: ComposeProject<'a>,
    /// Pure plan containing final-image protection and cdenv labels.
    pub plan: &'a ComposePlan,
    /// Force replacement during create/rebuild reconciliation.
    pub force_recreate: bool,
}

/// Exact image claim emitted by Compose and tagged by the adapter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeBaseClaim {
    /// Content-addressed image ID claimed by `compose images`.
    pub image_id: ImageId,
    /// Operation-owned tag applied to that exact ID.
    pub base_tag: String,
}

/// Container claims returned after project reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComposeUpClaim {
    /// Exact primary-service container ID.
    pub primary: ContainerId,
    /// Exact managed service/container pairs in stable order.
    pub managed: BTreeMap<String, ContainerId>,
}

/// Docker Compose V2 subprocess adapter bound to one resolved endpoint.
#[derive(Clone, Debug)]
pub struct ComposeAdapter {
    executable: PathBuf,
    endpoint: DockerEndpoint,
    runner: ProcessRunner,
    environment: Vec<(OsString, OsString)>,
    temporary_root: PathBuf,
}

impl ComposeAdapter {
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
        environment.retain(|(name, _)| name != "DOCKER_HOST");
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
        }
    }

    /// Returns the exact endpoint injected into every Compose and image-tag command.
    #[must_use]
    pub const fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }

    /// Resolves and parses only the service fields required by cdenv planning.
    ///
    /// Interpolated stdout is held in bounded memory and is never written to an operation log.
    ///
    /// # Errors
    ///
    /// Returns path, process, non-zero-exit, truncation, or typed-output failures.
    pub async fn resolve_model(
        &self,
        project: ComposeProject<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeModel, ComposeAdapterError> {
        validate_project(project)?;
        let arguments = compose_arguments(project, None, &["config", "--format", "json"])?;
        let result = self
            .run(
                "compose-config",
                &arguments,
                project.working_directory,
                true,
                &[],
                cancellation,
            )
            .await?;
        ensure_success("config", &result)?;
        ensure_complete(&result)?;
        parse_compose_model(result.stdout.as_bytes())
    }

    /// Pulls or builds the primary service, identifies that exact image, and tags its image ID.
    ///
    /// # Errors
    ///
    /// Rejects invalid inputs, failed Compose commands, malformed/ambiguous image output, and
    /// image-tag failure.
    pub async fn prepare_base(
        &self,
        request: &ComposeBaseRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeBaseClaim, ComposeAdapterError> {
        validate_project(request.project)?;
        validate_value("service", request.service)?;
        validate_value("base image tag", request.base_tag)?;
        let command = if request.has_build {
            let mut command = vec![OsString::from("build")];
            if request.no_cache {
                command.push(OsString::from("--no-cache"));
            }
            command.push(OsString::from(request.service));
            command
        } else {
            vec![OsString::from("pull"), OsString::from(request.service)]
        };
        let arguments = compose_arguments_os(request.project, None, &command)?;
        let result = self
            .run(
                if request.has_build {
                    "compose-build"
                } else {
                    "compose-pull"
                },
                &arguments,
                request.project.working_directory,
                false,
                &[],
                cancellation,
            )
            .await?;
        ensure_success(if request.has_build { "build" } else { "pull" }, &result)?;

        let arguments = compose_arguments(
            request.project,
            None,
            &["images", "--format", "json", request.service],
        )?;
        let result = self
            .run(
                "compose-images",
                &arguments,
                request.project.working_directory,
                false,
                &[],
                cancellation,
            )
            .await?;
        ensure_success("images", &result)?;
        ensure_complete(&result)?;
        let image_id = parse_single_image(result.stdout.as_bytes(), request.service)?;

        let arguments = vec![
            OsString::from("image"),
            OsString::from("tag"),
            OsString::from(image_id.as_str()),
            OsString::from(request.base_tag),
        ];
        let result = self
            .run(
                "compose-base-tag",
                &arguments,
                request.project.working_directory,
                false,
                &[],
                cancellation,
            )
            .await?;
        ensure_success("tag exact Compose base image", &result)?;
        Ok(ComposeBaseClaim {
            image_id,
            base_tag: request.base_tag.to_owned(),
        })
    }

    /// Reconciles with `--no-build --pull never`, then claims exact managed container IDs.
    ///
    /// # Errors
    ///
    /// Returns materialization, process, or malformed/ambiguous service-output failures.
    pub async fn up(
        &self,
        request: &ComposeUpRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeUpClaim, ComposeAdapterError> {
        validate_project(request.project)?;
        if request.project.project_name != request.plan.project_name() {
            return Err(ComposeAdapterError::ProjectPlanMismatch);
        }
        let override_file = SecretOverrideFile::create(
            &self.temporary_root,
            request.project.working_directory,
            request.plan.override_json(),
        )?;
        let mut command = vec![
            OsString::from("up"),
            OsString::from("--detach"),
            OsString::from("--no-build"),
            OsString::from("--pull"),
            OsString::from("never"),
        ];
        if request.force_recreate {
            command.push(OsString::from("--force-recreate"));
        }
        command.extend(request.plan.requested_services().iter().map(OsString::from));
        let arguments =
            compose_arguments_os(request.project, Some(override_file.path()), &command)?;
        let redactions = override_redactions(request.plan.override_json());
        let result = self
            .run(
                "compose-up",
                &arguments,
                request.project.working_directory,
                false,
                &redactions,
                cancellation,
            )
            .await?;
        ensure_success("up", &result)?;

        let arguments = compose_arguments(
            request.project,
            Some(override_file.path()),
            &["ps", "--all", "--format", "json"],
        )?;
        let result = self
            .run(
                "compose-ps",
                &arguments,
                request.project.working_directory,
                false,
                &redactions,
                cancellation,
            )
            .await?;
        ensure_success("ps", &result)?;
        ensure_complete(&result)?;
        parse_container_claims(
            result.stdout.as_bytes(),
            request.project.project_name,
            request.plan.primary_service(),
            request.plan.managed_services(),
        )
    }

    async fn run(
        &self,
        operation: &'static str,
        arguments: &[OsString],
        cwd: &Path,
        secret_stdout: bool,
        redactions: &[Vec<u8>],
        cancellation: &CancellationToken,
    ) -> Result<ProcessResult, ComposeAdapterError> {
        let argument_refs = arguments
            .iter()
            .map(OsString::as_os_str)
            .collect::<Vec<_>>();
        let environment = self
            .environment
            .iter()
            .map(|(name, value)| ProcessEnvironmentVariable {
                name,
                value,
                sensitive: false,
            })
            .collect::<Vec<_>>();
        let redaction_refs = redactions.iter().map(Vec::as_slice).collect::<Vec<_>>();
        let request = ProcessRequest {
            operation,
            executable: &self.executable,
            arguments: &argument_refs,
            cwd,
            environment: &environment,
            redactions: &redaction_refs,
            deadline: ProcessDeadline::Unbounded,
        };
        if secret_stdout {
            self.runner.run_secret_stdout(&request, cancellation).await
        } else {
            self.runner.run(&request, cancellation).await
        }
        .map_err(ComposeAdapterError::Process)
    }
}

/// Builds the exact Compose argv prefix and command, excluding argv zero.
///
/// # Errors
///
/// Rejects invalid project identity, absent files, or non-absolute file/override paths.
pub fn compose_arguments(
    project: ComposeProject<'_>,
    override_file: Option<&Path>,
    command: &[&str],
) -> Result<Vec<OsString>, ComposeAdapterError> {
    compose_arguments_os(
        project,
        override_file,
        &command.iter().map(OsString::from).collect::<Vec<_>>(),
    )
}

fn compose_arguments_os(
    project: ComposeProject<'_>,
    override_file: Option<&Path>,
    command: &[OsString],
) -> Result<Vec<OsString>, ComposeAdapterError> {
    validate_project(project)?;
    if override_file.is_some_and(|path| !path.is_absolute()) {
        return Err(ComposeAdapterError::RelativeOverride);
    }
    let mut arguments = vec![
        OsString::from("compose"),
        OsString::from("--project-directory"),
        project.working_directory.as_os_str().to_owned(),
        OsString::from("--project-name"),
        OsString::from(project.project_name),
    ];
    for file in project.files {
        arguments.push(OsString::from("--file"));
        arguments.push(file.as_os_str().to_owned());
    }
    if let Some(file) = override_file {
        arguments.push(OsString::from("--file"));
        arguments.push(file.as_os_str().to_owned());
    }
    arguments.extend_from_slice(command);
    Ok(arguments)
}

/// Compose-specific adapter failure. Interpolated model contents are never included.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ComposeAdapterError {
    /// Direct process execution failed.
    #[error(transparent)]
    Process(#[from] ProcessError),
    /// A Compose command failed without reproducing potentially sensitive output.
    #[error(
        "Docker Compose `{operation}` failed with exit code {code:?}; see the restricted operation log"
    )]
    Exited {
        /// Safe operation name.
        operation: &'static str,
        /// Portable exit code.
        code: Option<i32>,
    },
    /// Captured typed output exceeded its bound.
    #[error("Docker Compose `{operation}` output exceeded the safe parsing bound")]
    Truncated {
        /// Safe operation name.
        operation: &'static str,
    },
    /// Compose project fields are invalid.
    #[error("invalid Compose {field}")]
    InvalidValue {
        /// Safe field name.
        field: &'static str,
    },
    /// No explicit Compose file was supplied.
    #[error("at least one explicit Compose file is required")]
    MissingFiles,
    /// A Compose file/cwd is not absolute or escapes the working tree.
    #[error("Compose file {path:?} must be an absolute regular file below {root:?}")]
    InvalidFile {
        /// Rejected file.
        path: PathBuf,
        /// Canonical project root.
        root: PathBuf,
    },
    /// The project working directory was invalid.
    #[error("Compose working directory {path:?} must be an absolute existing directory")]
    InvalidWorkingDirectory {
        /// Rejected path.
        path: PathBuf,
    },
    /// Override paths passed to Compose must be explicit and absolute.
    #[error("generated Compose override path must be absolute")]
    RelativeOverride,
    /// The override root was inside the checkout.
    #[error("generated Compose override root must remain outside the checkout")]
    OverrideInsideCheckout,
    /// Secret override material could not be created safely.
    #[error("cannot materialize private Compose override at {path:?}: {source}")]
    OverrideIo {
        /// Managed path.
        path: PathBuf,
        /// Filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// Resolved configuration output was not the required typed shape.
    #[error("Docker Compose resolved service output was not recognized")]
    InvalidModel,
    /// Image output was absent, ambiguous, mismatched, or malformed.
    #[error("Docker Compose did not identify exactly one canonical image for service `{service}`")]
    InvalidImageClaim {
        /// Expected service.
        service: String,
    },
    /// Container output was absent, ambiguous, mismatched, or malformed.
    #[error(
        "Docker Compose container claim for managed service `{service}` was absent or ambiguous"
    )]
    InvalidContainerClaim {
        /// Expected service.
        service: String,
    },
    /// Compose reported a different project.
    #[error("Docker Compose reported project `{actual}` instead of `{expected}`")]
    WrongProject {
        /// Expected project.
        expected: String,
        /// Reported project.
        actual: String,
    },
    /// The execution project and pure plan disagree.
    #[error("Compose execution project does not match the pure plan")]
    ProjectPlanMismatch,
    /// A claimed Docker container ID was malformed.
    #[error(transparent)]
    ContainerId(#[from] ContainerIdError),
}

#[derive(Deserialize)]
struct RawModel {
    services: BTreeMap<String, RawService>,
}

#[derive(Deserialize)]
struct RawService {
    #[serde(default)]
    image: Option<String>,
    #[serde(default)]
    build: Option<Value>,
    #[serde(default)]
    depends_on: Option<Value>,
    #[serde(default)]
    user: Option<String>,
}

fn parse_compose_model(bytes: &[u8]) -> Result<ComposeModel, ComposeAdapterError> {
    let raw: RawModel =
        serde_json::from_slice(bytes).map_err(|_| ComposeAdapterError::InvalidModel)?;
    if raw.services.is_empty() {
        return Err(ComposeAdapterError::InvalidModel);
    }
    let services = raw
        .services
        .into_iter()
        .map(|(name, service)| {
            let dependencies = parse_dependencies(service.depends_on.as_ref())?;
            Ok((
                name,
                ComposeServiceModel {
                    image: service.image,
                    has_build: service.build.is_some_and(|value| !value.is_null()),
                    dependencies,
                    user: service.user,
                },
            ))
        })
        .collect::<Result<_, ComposeAdapterError>>()?;
    Ok(ComposeModel { services })
}

fn parse_dependencies(value: Option<&Value>) -> Result<BTreeSet<String>, ComposeAdapterError> {
    match value {
        None | Some(Value::Null) => Ok(BTreeSet::new()),
        Some(Value::Object(values)) => Ok(values.keys().cloned().collect()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_owned)
                    .ok_or(ComposeAdapterError::InvalidModel)
            })
            .collect(),
        Some(_) => Err(ComposeAdapterError::InvalidModel),
    }
}

#[derive(Deserialize)]
struct ImageRecord {
    #[serde(rename = "Service", alias = "service")]
    service: String,
    #[serde(rename = "ID", alias = "Id", alias = "id")]
    id: String,
}

fn parse_single_image(bytes: &[u8], service: &str) -> Result<ImageId, ComposeAdapterError> {
    let records = parse_json_records::<ImageRecord>(bytes).map_err(|_| {
        ComposeAdapterError::InvalidImageClaim {
            service: service.to_owned(),
        }
    })?;
    let mut matching = records
        .into_iter()
        .filter(|record| record.service == service);
    let Some(record) = matching.next() else {
        return Err(ComposeAdapterError::InvalidImageClaim {
            service: service.to_owned(),
        });
    };
    if matching.next().is_some() {
        return Err(ComposeAdapterError::InvalidImageClaim {
            service: service.to_owned(),
        });
    }
    ImageId::parse(&record.id).map_err(|_| ComposeAdapterError::InvalidImageClaim {
        service: service.to_owned(),
    })
}

#[derive(Deserialize)]
struct ContainerRecord {
    #[serde(rename = "ID", alias = "Id", alias = "id")]
    id: String,
    #[serde(rename = "Service", alias = "service")]
    service: String,
    #[serde(rename = "Project", alias = "project")]
    project: String,
}

fn parse_container_claims(
    bytes: &[u8],
    project: &str,
    primary: &str,
    managed_services: &[String],
) -> Result<ComposeUpClaim, ComposeAdapterError> {
    let records = parse_json_records::<ContainerRecord>(bytes).map_err(|_| {
        ComposeAdapterError::InvalidContainerClaim {
            service: primary.to_owned(),
        }
    })?;
    let managed_set = managed_services
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let mut managed = BTreeMap::new();
    for record in records {
        if record.project != project {
            return Err(ComposeAdapterError::WrongProject {
                expected: project.to_owned(),
                actual: record.project,
            });
        }
        if managed_set.contains(record.service.as_str()) {
            let id = ContainerId::parse(&record.id)?;
            if managed.insert(record.service.clone(), id).is_some() {
                return Err(ComposeAdapterError::InvalidContainerClaim {
                    service: record.service,
                });
            }
        }
    }
    for service in managed_services {
        if !managed.contains_key(service) {
            return Err(ComposeAdapterError::InvalidContainerClaim {
                service: service.clone(),
            });
        }
    }
    let primary = managed.get(primary).cloned().ok_or_else(|| {
        ComposeAdapterError::InvalidContainerClaim {
            service: primary.to_owned(),
        }
    })?;
    Ok(ComposeUpClaim { primary, managed })
}

fn parse_json_records<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
) -> Result<Vec<T>, serde_json::Error> {
    if let Ok(records) = serde_json::from_slice(bytes) {
        return Ok(records);
    }
    bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.iter().all(u8::is_ascii_whitespace))
        .map(serde_json::from_slice)
        .collect()
}

fn validate_project(project: ComposeProject<'_>) -> Result<(), ComposeAdapterError> {
    validate_value("project name", project.project_name)?;
    if project.files.is_empty() {
        return Err(ComposeAdapterError::MissingFiles);
    }
    let root = project.working_directory.canonicalize().map_err(|_| {
        ComposeAdapterError::InvalidWorkingDirectory {
            path: project.working_directory.to_path_buf(),
        }
    })?;
    if !root.is_absolute() || !root.is_dir() {
        return Err(ComposeAdapterError::InvalidWorkingDirectory {
            path: project.working_directory.to_path_buf(),
        });
    }
    for file in project.files {
        let canonical = file
            .canonicalize()
            .map_err(|_| ComposeAdapterError::InvalidFile {
                path: file.clone(),
                root: root.clone(),
            })?;
        if !canonical.starts_with(&root) || !canonical.is_file() || !file.is_absolute() {
            return Err(ComposeAdapterError::InvalidFile {
                path: file.clone(),
                root,
            });
        }
    }
    Ok(())
}

fn validate_value(field: &'static str, value: &str) -> Result<(), ComposeAdapterError> {
    if value.is_empty() || value.starts_with('-') || value.chars().any(char::is_control) {
        Err(ComposeAdapterError::InvalidValue { field })
    } else {
        Ok(())
    }
}

fn ensure_success(
    operation: &'static str,
    result: &ProcessResult,
) -> Result<(), ComposeAdapterError> {
    if result.status.success() {
        Ok(())
    } else {
        Err(ComposeAdapterError::Exited {
            operation,
            code: result.status.code(),
        })
    }
}

fn ensure_complete(result: &ProcessResult) -> Result<(), ComposeAdapterError> {
    if result.stdout.is_truncated() {
        Err(ComposeAdapterError::Truncated {
            operation: "typed output",
        })
    } else {
        Ok(())
    }
}

fn override_redactions(bytes: &[u8]) -> Vec<Vec<u8>> {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("services")?.as_object().cloned())
        .into_iter()
        .flat_map(serde_json::Map::into_values)
        .filter_map(|service| service.get("environment")?.as_object().cloned())
        .flat_map(serde_json::Map::into_values)
        .filter_map(|value| value.as_str().map(str::as_bytes).map(<[u8]>::to_vec))
        .filter(|value| !value.is_empty())
        .collect()
}

struct SecretOverrideFile {
    directory: PathBuf,
    path: PathBuf,
}

impl SecretOverrideFile {
    fn create(root: &Path, checkout: &Path, bytes: &[u8]) -> Result<Self, ComposeAdapterError> {
        fs::create_dir_all(root).map_err(|source| ComposeAdapterError::OverrideIo {
            path: root.to_path_buf(),
            source,
        })?;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ComposeAdapterError::OverrideIo {
                path: root.to_path_buf(),
                source,
            }
        })?;
        let root = root
            .canonicalize()
            .map_err(|source| ComposeAdapterError::OverrideIo {
                path: root.to_path_buf(),
                source,
            })?;
        let checkout =
            checkout
                .canonicalize()
                .map_err(|source| ComposeAdapterError::OverrideIo {
                    path: checkout.to_path_buf(),
                    source,
                })?;
        if root.starts_with(&checkout) {
            return Err(ComposeAdapterError::OverrideInsideCheckout);
        }
        let mut random = [0_u8; 16];
        fill(&mut random).map_err(|error| ComposeAdapterError::OverrideIo {
            path: root.clone(),
            source: std::io::Error::other(error),
        })?;
        let directory = root.join(format!("compose-{}", hex::encode(random)));
        fs::create_dir(&directory).map_err(|source| ComposeAdapterError::OverrideIo {
            path: directory.clone(),
            source,
        })?;
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ComposeAdapterError::OverrideIo {
                path: directory.clone(),
                source,
            }
        })?;
        let path = directory.join("override.json");
        fs::write(&path, bytes).map_err(|source| ComposeAdapterError::OverrideIo {
            path: path.clone(),
            source,
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|source| {
            ComposeAdapterError::OverrideIo {
                path: path.clone(),
                source,
            }
        })?;
        Ok(Self { directory, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SecretOverrideFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_dir(&self.directory);
    }
}
