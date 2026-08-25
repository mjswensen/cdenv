//! Pure Docker Compose project, service-set, and override planning.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{ComposeScenario, MountKind, PlannedMount, PortPlan, RuntimePlan};

/// Maximum Compose project-name length used by cdenv.
pub const MAXIMUM_COMPOSE_PROJECT_NAME_LENGTH: usize = 63;
const PROJECT_HASH_LENGTH: usize = 12;

/// Stable labels attached to every service in an isolated Compose project.
#[derive(Clone, Copy, Debug)]
pub struct ComposeIdentity<'a> {
    /// Installation namespace.
    pub installation: &'a str,
    /// Workspace identity.
    pub workspace: &'a str,
    /// Positive environment generation rendered for Docker labels.
    pub generation: &'a str,
    /// Compatibility profile.
    pub profile: &'a str,
}

/// The required, secret-free part of one resolved Compose service.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ComposeServiceModel {
    /// Exact resolved image reference, when declared.
    pub image: Option<String>,
    /// Whether the service has a Compose build definition.
    pub has_build: bool,
    /// Compose service dependencies.
    pub dependencies: BTreeSet<String>,
    /// Exact service user, when declared.
    pub user: Option<String>,
}

/// Required typed subset of a resolved Compose model.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ComposeModel {
    /// Services keyed by their exact Compose service name.
    pub services: BTreeMap<String, ComposeServiceModel>,
}

impl std::fmt::Debug for ComposeServiceModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComposeServiceModel")
            .field("has_image", &self.image.is_some())
            .field("has_build", &self.has_build)
            .field("dependencies", &self.dependencies)
            .field("has_user", &self.user.is_some())
            .finish()
    }
}

impl std::fmt::Debug for ComposeModel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComposeModel")
            .field("services", &self.services.keys().collect::<Vec<_>>())
            .finish()
    }
}

/// Optional primary-service replacements applied after profile/image enrichment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ComposePrimaryOverride<'a> {
    /// Exact derived image. Supplying it also removes `build` and forbids pulls.
    pub image: Option<&'a str>,
    /// Explicit entrypoint vector.
    pub entrypoint: Option<&'a [String]>,
    /// Explicit command vector.
    pub command: Option<&'a [String]>,
}

/// Inputs to deterministic Compose planning.
pub struct ComposePlanningInputs<'a> {
    /// Stable cdenv resource identity.
    pub identity: ComposeIdentity<'a>,
    /// Effective pure runtime settings for the primary service.
    pub runtime: &'a RuntimePlan,
    /// Validated create-time publications for the primary service.
    pub ports: &'a PortPlan,
    /// Optional final image/command enrichment.
    pub primary_override: ComposePrimaryOverride<'a>,
}

/// An isolated Compose project and canonical override.
pub struct ComposePlan {
    project_name: String,
    primary_service: String,
    requested_services: Vec<String>,
    managed_services: Vec<String>,
    override_json: Vec<u8>,
}

impl ComposePlan {
    /// Returns the Docker-safe isolated project name.
    #[must_use]
    pub fn project_name(&self) -> &str {
        &self.project_name
    }

    /// Returns the exact configured primary service.
    #[must_use]
    pub fn primary_service(&self) -> &str {
        &self.primary_service
    }

    /// Returns services explicitly requested from Compose in deterministic order.
    #[must_use]
    pub fn requested_services(&self) -> &[String] {
        &self.requested_services
    }

    /// Returns requested services and their transitive dependencies.
    #[must_use]
    pub fn managed_services(&self) -> &[String] {
        &self.managed_services
    }

    /// Returns canonical compact JSON. This may contain environment secrets and must not be logged.
    #[must_use]
    pub fn override_json(&self) -> &[u8] {
        &self.override_json
    }
}

impl std::fmt::Debug for ComposePlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComposePlan")
            .field("project_name", &self.project_name)
            .field("primary_service", &self.primary_service)
            .field("requested_services", &self.requested_services)
            .field("managed_services", &self.managed_services)
            .field("override_bytes", &self.override_json.len())
            .finish()
    }
}

/// Pure Compose planning failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ComposePlanningError {
    /// The configured primary service is absent.
    #[error("Compose primary service `{service}` is not present in the resolved model")]
    MissingPrimary {
        /// Missing service.
        service: String,
    },
    /// A configured run service is absent.
    #[error("Compose run service `{service}` is not present in the resolved model")]
    MissingRunService {
        /// Missing service.
        service: String,
    },
    /// A dependency names an absent service.
    #[error("Compose service `{service}` depends on missing service `{dependency}")]
    MissingDependency {
        /// Depending service.
        service: String,
        /// Missing dependency.
        dependency: String,
    },
    /// An owned label value cannot safely be represented.
    #[error("Compose identity field `{field}` is empty or contains a control character")]
    InvalidIdentity {
        /// Safe field name.
        field: &'static str,
    },
    /// The deterministic override could not be encoded.
    #[error("Compose override could not be encoded")]
    Encoding,
}

/// Derives a stable lowercase Docker-safe Compose project name.
///
/// Names that exceed 63 bytes retain a readable prefix and a SHA-256-derived suffix.
#[must_use]
pub fn compose_project_name(installation: &str, workspace: &str) -> String {
    let canonical = format!(
        "cdenv-{}-{}",
        safe_project_component(installation),
        safe_project_component(workspace)
    );
    if canonical.len() <= MAXIMUM_COMPOSE_PROJECT_NAME_LENGTH {
        return canonical;
    }
    let digest = format!("{:x}", Sha256::digest(canonical.as_bytes()));
    let prefix_length = MAXIMUM_COMPOSE_PROJECT_NAME_LENGTH - PROJECT_HASH_LENGTH - 1;
    let mut prefix = canonical[..prefix_length]
        .trim_end_matches(['-', '_'])
        .to_owned();
    prefix.push('-');
    prefix.push_str(&digest[..PROJECT_HASH_LENGTH]);
    prefix
}

/// Plans the exact service set and canonical JSON-compatible override.
///
/// # Errors
///
/// Rejects absent configured services/dependencies, unsafe identity values, or encoding failure.
pub fn plan_compose(
    scenario: &ComposeScenario,
    model: &ComposeModel,
    inputs: &ComposePlanningInputs<'_>,
) -> Result<ComposePlan, ComposePlanningError> {
    validate_identity(inputs.identity)?;
    if !model.services.contains_key(&scenario.service) {
        return Err(ComposePlanningError::MissingPrimary {
            service: scenario.service.clone(),
        });
    }

    let requested = match &scenario.run_services {
        Some(services) => services.clone(),
        None => model.services.keys().cloned().collect(),
    };
    for service in &requested {
        if !model.services.contains_key(service) {
            return Err(ComposePlanningError::MissingRunService {
                service: service.clone(),
            });
        }
    }
    let mut requested_services = requested.into_iter().collect::<BTreeSet<_>>();
    requested_services.insert(scenario.service.clone());
    let managed_services = dependency_closure(model, &requested_services)?;
    let override_json = render_override(model, &scenario.service, inputs)?;

    Ok(ComposePlan {
        project_name: compose_project_name(inputs.identity.installation, inputs.identity.workspace),
        primary_service: scenario.service.clone(),
        requested_services: requested_services.into_iter().collect(),
        managed_services: managed_services.into_iter().collect(),
        override_json,
    })
}

fn dependency_closure(
    model: &ComposeModel,
    requested: &BTreeSet<String>,
) -> Result<BTreeSet<String>, ComposePlanningError> {
    let mut managed = requested.clone();
    let mut pending = requested.iter().cloned().collect::<Vec<_>>();
    while let Some(service) = pending.pop() {
        let definition = &model.services[&service];
        for dependency in &definition.dependencies {
            if !model.services.contains_key(dependency) {
                return Err(ComposePlanningError::MissingDependency {
                    service: service.clone(),
                    dependency: dependency.clone(),
                });
            }
            if managed.insert(dependency.clone()) {
                pending.push(dependency.clone());
            }
        }
    }
    Ok(managed)
}

#[derive(Serialize)]
struct OverrideDocument {
    services: BTreeMap<String, ServiceOverride>,
}

#[derive(Default, Serialize)]
struct ServiceOverride {
    labels: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    build: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_policy: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    working_dir: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    volumes: Vec<MountOverride>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    environment: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    ports: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    init: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    privileged: Option<bool>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    cap_add: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    security_opt: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entrypoint: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    command: Option<Vec<String>>,
}

#[derive(Serialize)]
struct MountOverride {
    #[serde(rename = "type")]
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<String>,
    target: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    read_only: bool,
}

fn render_override(
    model: &ComposeModel,
    primary: &str,
    inputs: &ComposePlanningInputs<'_>,
) -> Result<Vec<u8>, ComposePlanningError> {
    let labels = BTreeMap::from([
        (
            "cdenv.generation".to_owned(),
            inputs.identity.generation.to_owned(),
        ),
        (
            "cdenv.installation".to_owned(),
            inputs.identity.installation.to_owned(),
        ),
        (
            "cdenv.profile".to_owned(),
            inputs.identity.profile.to_owned(),
        ),
        (
            "cdenv.workspace".to_owned(),
            inputs.identity.workspace.to_owned(),
        ),
    ]);
    let mut services = model
        .services
        .keys()
        .map(|name| {
            (
                name.clone(),
                ServiceOverride {
                    labels: labels.clone(),
                    ..ServiceOverride::default()
                },
            )
        })
        .collect::<BTreeMap<_, _>>();
    let service =
        services
            .get_mut(primary)
            .ok_or_else(|| ComposePlanningError::MissingPrimary {
                service: primary.to_owned(),
            })?;
    service.working_dir = Some(inputs.runtime.workspace.folder.as_str().to_owned());
    service
        .volumes
        .push(mount_override(&inputs.runtime.workspace.mount));
    service
        .volumes
        .extend(inputs.runtime.mounts.iter().map(mount_override));
    service.environment = inputs
        .runtime
        .environment
        .container()
        .iter()
        .map(|(name, value)| (name.clone(), value.expose().to_owned()))
        .collect();
    service.ports = inputs
        .ports
        .publications
        .iter()
        .map(|publication| publication.argument.clone())
        .collect();
    service.user = Some(inputs.runtime.container_user.as_str().to_owned());
    service.init = inputs.runtime.init.then_some(true);
    service.privileged = inputs.runtime.privileged.then_some(true);
    service.cap_add.clone_from(&inputs.runtime.cap_add);
    service
        .security_opt
        .clone_from(&inputs.runtime.security_opt);
    service.entrypoint = inputs.primary_override.entrypoint.map(<[String]>::to_vec);
    service.command = inputs.primary_override.command.map(<[String]>::to_vec);
    if let Some(image) = inputs.primary_override.image {
        service.image = Some(image.to_owned());
        service.build = Some(serde_json::Value::Null);
        service.pull_policy = Some("never");
    }
    serde_json::to_vec(&OverrideDocument { services }).map_err(|_| ComposePlanningError::Encoding)
}

fn mount_override(mount: &PlannedMount) -> MountOverride {
    MountOverride {
        kind: match mount.kind {
            MountKind::Bind => "bind",
            MountKind::Volume => "volume",
        },
        source: mount.source.clone(),
        target: mount.target.as_str().to_owned(),
        read_only: mount
            .options
            .iter()
            .any(|option| option.name == "readonly" || option.name == "ro"),
    }
}

fn validate_identity(identity: ComposeIdentity<'_>) -> Result<(), ComposePlanningError> {
    for (field, value) in [
        ("installation", identity.installation),
        ("workspace", identity.workspace),
        ("generation", identity.generation),
        ("profile", identity.profile),
    ] {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(ComposePlanningError::InvalidIdentity { field });
        }
    }
    Ok(())
}

fn safe_project_component(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut separator = false;
    for character in value.chars().flat_map(char::to_lowercase) {
        if character.is_ascii_alphanumeric() {
            output.push(character);
            separator = false;
        } else if !separator && !output.is_empty() {
            output.push('-');
            separator = true;
        }
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        output.push('x');
    }
    output
}
