//! Read-side Docker resource discovery, inspection mapping, and verification.

use std::collections::{BTreeMap, HashMap};

use bollard::Docker;
use bollard::models::{ContainerInspectResponse, ContainerSummary, ImageInspect};
use bollard::query_parameters::ListContainersOptionsBuilder;
use cdenv_core::{
    ContainerArchitecture, ContainerId, GenerationId, InstallationId, ProfileId, WorkspaceName,
};
use cdenv_devcontainer::{PortPlan, PublicationBinding, PublicationProtocol};

use super::{
    BollardAdapter, BollardAdapterError, BollardApi, BollardApiError, BollardApiRequest,
    BollardApiResponse, COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL, GENERATION_LABEL,
    INSTALLATION_LABEL, PROFILE_LABEL, WORKSPACE_LABEL,
};
use crate::ImageId;

pub(super) async fn execute_ping(client: &Docker) -> Result<BollardApiResponse, BollardApiError> {
    client
        .ping()
        .await
        .map(BollardApiResponse::Ping)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_list_containers(
    client: &Docker,
    all: bool,
    filters: BTreeMap<String, Vec<String>>,
) -> Result<BollardApiResponse, BollardApiError> {
    let filters: HashMap<_, _> = filters.into_iter().collect();
    let options = ListContainersOptionsBuilder::new()
        .all(all)
        .filters(&filters)
        .build();
    client
        .list_containers(Some(options))
        .await
        .map(BollardApiResponse::Containers)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_inspect_container(
    client: &Docker,
    id: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    client
        .inspect_container(id, None)
        .await
        .map(Box::new)
        .map(BollardApiResponse::Container)
        .map_err(BollardApiError::from_bollard)
}

pub(super) async fn execute_inspect_image(
    client: &Docker,
    id: &str,
) -> Result<BollardApiResponse, BollardApiError> {
    client
        .inspect_image(id)
        .await
        .map(Box::new)
        .map(BollardApiResponse::Image)
        .map_err(BollardApiError::from_bollard)
}

/// The exact scope sent in one `all=true` container-list request.
#[derive(Clone, Copy, Debug)]
pub struct ContainerDiscoveryScope<'a> {
    /// Required installation namespace.
    pub installation: &'a InstallationId,
    /// Optional exact workspace filter. Omit this to correlate many workspaces in memory.
    pub workspace: Option<&'a WorkspaceName>,
    /// Optional exact generation filter.
    pub generation: Option<GenerationId>,
}

impl ContainerDiscoveryScope<'_> {
    fn filters(self) -> BTreeMap<String, Vec<String>> {
        let mut labels = vec![format!("{INSTALLATION_LABEL}={}", self.installation)];
        if let Some(workspace) = self.workspace {
            labels.push(format!("{WORKSPACE_LABEL}={workspace}"));
        }
        if let Some(generation) = self.generation {
            labels.push(format!("{GENERATION_LABEL}={generation}"));
        }
        BTreeMap::from([("label".to_owned(), labels)])
    }
}

/// A lossless, typed container-list entry used for in-memory correlation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveredContainer {
    /// Full, non-abbreviated container ID.
    pub id: ContainerId,
    /// Every Docker-reported name, with the historical leading slash removed.
    pub names: Vec<String>,
    /// Exact image identity reported by Docker, when present.
    pub image_id: Option<ImageId>,
    /// All labels returned by the list API.
    pub labels: BTreeMap<String, String>,
    /// Docker's state text, when present.
    pub state: Option<String>,
}

impl DiscoveredContainer {
    /// Reports whether the list result says this container is running.
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.state.as_deref() == Some("running")
    }
}

/// Inputs for classifying a workspace from one installation-wide list result.
#[derive(Clone, Copy, Debug)]
pub struct WorkspaceCorrelation<'a> {
    /// Workspace to correlate.
    pub workspace: &'a WorkspaceName,
    /// Expected active generation.
    pub generation: GenerationId,
    /// Persisted container identity, when one was successfully provisioned.
    pub recorded_container: Option<&'a ContainerId>,
}

/// All current and stale matches for one workspace; no arbitrary match is selected.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorrelatedContainers {
    /// Workspace identity.
    pub workspace: WorkspaceName,
    /// Every match for the expected generation.
    pub current: Vec<DiscoveredContainer>,
    /// Every match for another or malformed generation.
    pub stale: Vec<DiscoveredContainer>,
    /// Whether the recorded ID occurs among current matches.
    pub recorded_current: bool,
    /// Current-generation matches other than the recorded ID.
    pub external_replacements: Vec<DiscoveredContainer>,
}

/// Correlates many workspaces in memory while preserving duplicate and stale matches.
#[must_use]
pub fn correlate_containers(
    containers: &[DiscoveredContainer],
    inputs: &[WorkspaceCorrelation<'_>],
) -> Vec<CorrelatedContainers> {
    inputs
        .iter()
        .map(|input| {
            let expected_generation = input.generation.to_string();
            let mut current = Vec::new();
            let mut stale = Vec::new();
            for container in containers.iter().filter(|container| {
                container.labels.get(WORKSPACE_LABEL).map(String::as_str)
                    == Some(input.workspace.as_str())
            }) {
                if container
                    .labels
                    .get(GENERATION_LABEL)
                    .is_some_and(|value| value == &expected_generation)
                {
                    current.push(container.clone());
                } else {
                    stale.push(container.clone());
                }
            }
            let recorded_current = input
                .recorded_container
                .is_some_and(|recorded| current.iter().any(|container| &container.id == recorded));
            let external_replacements = current
                .iter()
                .filter(|container| {
                    input
                        .recorded_container
                        .is_none_or(|recorded| &container.id != recorded)
                })
                .cloned()
                .collect();
            CorrelatedContainers {
                workspace: input.workspace.clone(),
                current,
                stale,
                recorded_current,
                external_replacements,
            }
        })
        .collect()
}

/// Verifies every expected container identity field and state without requiring an adapter value.
///
/// # Errors
///
/// Returns the first exact identity, label, Compose, image, name, or state mismatch.
pub fn verify_container(
    inspection: &ContainerInspection,
    expected: ContainerExpectation<'_>,
) -> Result<(), BollardAdapterError> {
    verify_value("container ID", expected.id.as_str(), inspection.id.as_str())?;
    verify_value("container name", expected.name, &inspection.name)?;
    verify_value(
        "image ID",
        expected.image_id.as_str(),
        inspection.image_id.as_str(),
    )?;
    verify_label(
        &inspection.labels,
        INSTALLATION_LABEL,
        expected.installation.as_str(),
    )?;
    verify_label(
        &inspection.labels,
        WORKSPACE_LABEL,
        expected.workspace.as_str(),
    )?;
    verify_label(
        &inspection.labels,
        GENERATION_LABEL,
        &expected.generation.to_string(),
    )?;
    verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
    verify_optional_label(&inspection.labels, COMPOSE_PROJECT_LABEL, expected.project)?;
    verify_optional_label(&inspection.labels, COMPOSE_SERVICE_LABEL, expected.service)?;
    if let Some(running) = expected.running {
        verify_value(
            "running state",
            &running.to_string(),
            &inspection.running.to_string(),
        )?;
    }
    Ok(())
}

/// One Docker-inspected container mount.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InspectedMount {
    /// Docker mount kind, such as `bind` or `volume`.
    pub kind: String,
    /// Bind source path or named-volume name.
    pub source: Option<String>,
    /// Absolute container destination.
    pub target: String,
    /// Docker-normalized comma-separated mode.
    pub mode: Option<String>,
}

/// One Docker-inspected requested host port binding.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct InspectedPortBinding {
    /// Container port/protocol key, such as `3000/tcp`.
    pub container: String,
    /// Docker-normalized host address.
    pub host_ip: Option<String>,
    /// Requested or daemon-assigned host port.
    pub host_port: Option<String>,
}

/// Authoritative typed container inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContainerInspection {
    /// Full container identity.
    pub id: ContainerId,
    /// Exact name with Docker's historical leading slash removed.
    pub name: String,
    /// Content-addressed image identity used to create the container.
    pub image_id: ImageId,
    /// Image reference supplied at creation, when present.
    pub image_reference: Option<String>,
    /// Exact labels.
    pub labels: BTreeMap<String, String>,
    /// Configured container user.
    pub user: String,
    /// Configured container working directory.
    pub working_directory: String,
    /// Docker-inspected mounts in stable order.
    pub mounts: Vec<InspectedMount>,
    /// Docker-inspected requested port bindings in stable order.
    pub ports: Vec<InspectedPortBinding>,
    /// Whether Docker reports the container running.
    pub running: bool,
}

/// Authoritative typed image inspection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageInspection {
    /// Content-addressed image identity.
    pub id: ImageId,
    /// Exact image labels.
    pub labels: BTreeMap<String, String>,
    /// Supported container architecture.
    pub architecture: ContainerArchitecture,
}

/// Expected identity and state used to reject drift or external replacement.
#[derive(Clone, Copy, Debug)]
pub struct ContainerExpectation<'a> {
    /// Exact recorded/claimed container ID.
    pub id: &'a ContainerId,
    /// Exact cdenv-owned name.
    pub name: &'a str,
    /// Exact content-addressed image identity.
    pub image_id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
    /// Compose project, for Compose primary containers.
    pub project: Option<&'a str>,
    /// Compose service, for Compose primary containers.
    pub service: Option<&'a str>,
    /// Required running state, or `None` when either state is acceptable.
    pub running: Option<bool>,
}

/// Expected authoritative identity for a Compose primary claim.
#[derive(Clone, Copy, Debug)]
pub struct ComposePrimaryExpectation<'a> {
    /// Exact ID claimed by Compose for the primary service.
    pub id: &'a ContainerId,
    /// Exact content-addressed final image.
    pub image_id: &'a ImageId,
    /// Installation namespace.
    pub installation: &'a InstallationId,
    /// Workspace identity.
    pub workspace: &'a WorkspaceName,
    /// Environment generation.
    pub generation: GenerationId,
    /// Compatibility profile.
    pub profile: &'a ProfileId,
    /// Isolated Compose project.
    pub project: &'a str,
    /// Exact primary service.
    pub service: &'a str,
    /// Required running state.
    pub running: bool,
}

impl<A: BollardApi> BollardAdapter<A> {
    /// Pings the daemon within the adapter's per-call timeout.
    ///
    /// # Errors
    ///
    /// Returns a timeout, transport, or unexpected-response error.
    pub async fn ping(&self) -> Result<(), BollardAdapterError> {
        match self.call("ping", BollardApiRequest::Ping).await? {
            BollardApiResponse::Ping(value) if value == "OK" => Ok(()),
            BollardApiResponse::Ping(value) => Err(BollardAdapterError::UnexpectedPing { value }),
            _ => Err(BollardAdapterError::UnexpectedResponse { operation: "ping" }),
        }
    }

    /// Lists all matching containers in exactly one API call.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, response-shape, or identity-mapping error.
    pub async fn discover(
        &self,
        scope: ContainerDiscoveryScope<'_>,
    ) -> Result<Vec<DiscoveredContainer>, BollardAdapterError> {
        let response = self
            .call(
                "list containers",
                BollardApiRequest::ListContainers {
                    all: true,
                    filters: scope.filters(),
                },
            )
            .await?;
        let BollardApiResponse::Containers(containers) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "list containers",
            });
        };
        containers.into_iter().map(map_summary).collect()
    }

    /// Inspects and maps one exact container ID.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, incomplete-response, or malformed-identity error.
    pub async fn inspect_container(
        &self,
        id: &ContainerId,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        let response = self
            .call(
                "inspect container",
                BollardApiRequest::InspectContainer { id: id.to_string() },
            )
            .await?;
        let BollardApiResponse::Container(container) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "inspect container",
            });
        };
        map_container(*container)
    }

    /// Inspects an image and maps its exact identity, labels, and supported architecture.
    ///
    /// # Errors
    ///
    /// Returns an API, timeout, incomplete-response, or unsupported-architecture error.
    pub async fn inspect_image(&self, id: &str) -> Result<ImageInspection, BollardAdapterError> {
        let response = self
            .call(
                "inspect image",
                BollardApiRequest::InspectImage { id: id.to_owned() },
            )
            .await?;
        let BollardApiResponse::Image(image) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "inspect image",
            });
        };
        map_image(*image)
    }

    /// Verifies every expected container identity field and state.
    ///
    /// # Errors
    ///
    /// Returns the first exact identity, label, Compose, image, name, or state mismatch.
    pub fn verify_container(
        inspection: &ContainerInspection,
        expected: ContainerExpectation<'_>,
    ) -> Result<(), BollardAdapterError> {
        verify_container(inspection, expected)
    }

    /// Verifies a Compose primary claim and rejects any second matching primary container.
    ///
    /// # Errors
    ///
    /// Returns an inspect/discovery failure, exact identity mismatch, or ambiguous-primary error.
    pub async fn verify_compose_primary(
        &self,
        expected: ComposePrimaryExpectation<'_>,
    ) -> Result<ContainerInspection, BollardAdapterError> {
        let inspection = self.inspect_container(expected.id).await?;
        verify_value("container ID", expected.id.as_str(), inspection.id.as_str())?;
        verify_value(
            "image ID",
            expected.image_id.as_str(),
            inspection.image_id.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            INSTALLATION_LABEL,
            expected.installation.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            WORKSPACE_LABEL,
            expected.workspace.as_str(),
        )?;
        verify_label(
            &inspection.labels,
            GENERATION_LABEL,
            &expected.generation.to_string(),
        )?;
        verify_label(&inspection.labels, PROFILE_LABEL, expected.profile.as_str())?;
        verify_label(&inspection.labels, COMPOSE_PROJECT_LABEL, expected.project)?;
        verify_label(&inspection.labels, COMPOSE_SERVICE_LABEL, expected.service)?;
        verify_value(
            "running state",
            &expected.running.to_string(),
            &inspection.running.to_string(),
        )?;

        let matches = self
            .discover(ContainerDiscoveryScope {
                installation: expected.installation,
                workspace: Some(expected.workspace),
                generation: Some(expected.generation),
            })
            .await?
            .into_iter()
            .filter(|container| {
                container
                    .labels
                    .get(COMPOSE_PROJECT_LABEL)
                    .map(String::as_str)
                    == Some(expected.project)
                    && container
                        .labels
                        .get(COMPOSE_SERVICE_LABEL)
                        .map(String::as_str)
                        == Some(expected.service)
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 || matches[0].id != *expected.id {
            return Err(BollardAdapterError::AmbiguousComposePrimary {
                project: expected.project.to_owned(),
                service: expected.service.to_owned(),
                matches: matches.len(),
            });
        }
        Ok(inspection)
    }
}

/// Verifies every requested publication against authoritative daemon port bindings.
///
/// # Errors
///
/// Returns a typed mismatch containing the requested arguments and inspected bindings.
pub fn verify_port_bindings(
    inspection: &ContainerInspection,
    expected: &PortPlan,
) -> Result<(), BollardAdapterError> {
    if port_bindings_match(&inspection.ports, expected) {
        Ok(())
    } else {
        Err(BollardAdapterError::PortBindingMismatch {
            expected: expected
                .publications
                .iter()
                .map(|publication| publication.argument.clone())
                .collect(),
            actual: inspection.ports.clone(),
        })
    }
}

fn port_bindings_match(actual: &[InspectedPortBinding], expected: &PortPlan) -> bool {
    expected.publications.iter().all(|publication| {
        let protocol = match publication.protocol {
            PublicationProtocol::Tcp => "tcp",
            PublicationProtocol::Udp => "udp",
            PublicationProtocol::Sctp => "sctp",
        };
        (publication.container_ports.start.get()..=publication.container_ports.end.get()).all(
            |port| {
                let key = format!("{port}/{protocol}");
                let host_port = publication.host_ports.map(|range| {
                    range.start.get() + (port - publication.container_ports.start.get())
                });
                actual.iter().any(|binding| {
                    binding.container == key
                        && binding_ip_matches(binding.host_ip.as_deref(), publication.binding)
                        && host_port.map_or_else(
                            || {
                                binding.host_port.as_deref().is_some_and(|value| {
                                    value.parse::<u16>().is_ok_and(|value| value > 0)
                                })
                            },
                            |port| binding.host_port.as_deref() == Some(&port.to_string()),
                        )
                })
            },
        )
    })
}

fn binding_ip_matches(actual: Option<&str>, expected: PublicationBinding) -> bool {
    match expected {
        PublicationBinding::Loopback(address) | PublicationBinding::NonLoopback(address) => {
            actual == Some(address.to_string().as_str())
        }
        PublicationBinding::AllInterfaces => {
            matches!(actual, None | Some("" | "0.0.0.0" | "::"))
        }
    }
}

fn map_summary(summary: ContainerSummary) -> Result<DiscoveredContainer, BollardAdapterError> {
    let id = required(summary.id, "container list", "Id")?;
    let id = ContainerId::parse(&id)
        .map_err(|source| BollardAdapterError::InvalidContainerId { value: id, source })?;
    Ok(DiscoveredContainer {
        id,
        names: summary
            .names
            .unwrap_or_default()
            .into_iter()
            .map(|name| name.trim_start_matches('/').to_owned())
            .collect(),
        image_id: summary
            .image_id
            .map(|value| parse_image_id(value, "container list"))
            .transpose()?,
        labels: summary.labels.unwrap_or_default().into_iter().collect(),
        state: summary.state.map(|state| state.to_string()),
    })
}

pub(super) fn map_container(
    container: ContainerInspectResponse,
) -> Result<ContainerInspection, BollardAdapterError> {
    let id = required(container.id, "container inspect", "Id")?;
    let id = ContainerId::parse(&id)
        .map_err(|source| BollardAdapterError::InvalidContainerId { value: id, source })?;
    let config = required(container.config, "container inspect", "Config")?;
    let state = required(container.state, "container inspect", "State")?;
    let mut mounts = container
        .mounts
        .unwrap_or_default()
        .into_iter()
        .map(|mount| {
            let kind = required(mount.typ, "container inspect mount", "Type")?;
            let source = if kind == "volume" {
                mount.name
            } else {
                mount.source
            };
            Ok(InspectedMount {
                kind,
                source,
                target: required(mount.destination, "container inspect mount", "Destination")?,
                mode: mount.mode,
            })
        })
        .collect::<Result<Vec<_>, BollardAdapterError>>()?;
    mounts.sort();
    let mut ports = container
        .host_config
        .and_then(|host| host.port_bindings)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|(container, bindings)| {
            bindings
                .unwrap_or_default()
                .into_iter()
                .map(move |binding| InspectedPortBinding {
                    container: container.clone(),
                    host_ip: binding.host_ip,
                    host_port: binding.host_port,
                })
        })
        .collect::<Vec<_>>();
    ports.sort();
    Ok(ContainerInspection {
        id,
        name: required(container.name, "container inspect", "Name")?
            .trim_start_matches('/')
            .to_owned(),
        image_id: parse_image_id(
            required(container.image, "container inspect", "Image")?,
            "container inspect",
        )?,
        image_reference: config.image,
        labels: config.labels.unwrap_or_default().into_iter().collect(),
        user: config.user.unwrap_or_default(),
        working_directory: config.working_dir.unwrap_or_default(),
        mounts,
        ports,
        running: state.running.unwrap_or(false),
    })
}

fn map_image(image: ImageInspect) -> Result<ImageInspection, BollardAdapterError> {
    let architecture = required(image.architecture, "image inspect", "Architecture")?;
    Ok(ImageInspection {
        id: parse_image_id(required(image.id, "image inspect", "Id")?, "image inspect")?,
        labels: image
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default()
            .into_iter()
            .collect(),
        architecture: ContainerArchitecture::parse(&architecture)?,
    })
}

fn parse_image_id(value: String, resource: &'static str) -> Result<ImageId, BollardAdapterError> {
    ImageId::parse(&value).map_err(|_| BollardAdapterError::InvalidImageId { resource, value })
}

fn required<T>(
    value: Option<T>,
    resource: &'static str,
    field: &'static str,
) -> Result<T, BollardAdapterError> {
    value.ok_or(BollardAdapterError::MissingField { resource, field })
}

pub(super) fn verify_value(
    field: &'static str,
    expected: &str,
    actual: &str,
) -> Result<(), BollardAdapterError> {
    if expected == actual {
        Ok(())
    } else {
        Err(BollardAdapterError::VerificationMismatch {
            field,
            expected: expected.to_owned(),
            actual: Some(actual.to_owned()),
        })
    }
}

pub(super) fn verify_label(
    labels: &BTreeMap<String, String>,
    key: &'static str,
    expected: &str,
) -> Result<(), BollardAdapterError> {
    match labels.get(key) {
        Some(actual) if actual == expected => Ok(()),
        actual => Err(BollardAdapterError::VerificationMismatch {
            field: key,
            expected: expected.to_owned(),
            actual: actual.cloned(),
        }),
    }
}

fn verify_optional_label(
    labels: &BTreeMap<String, String>,
    key: &'static str,
    expected: Option<&str>,
) -> Result<(), BollardAdapterError> {
    match expected {
        Some(expected) => verify_label(labels, key, expected),
        None if labels.contains_key(key) => Err(BollardAdapterError::VerificationMismatch {
            field: key,
            expected: "<absent>".to_owned(),
            actual: labels.get(key).cloned(),
        }),
        None => Ok(()),
    }
}
