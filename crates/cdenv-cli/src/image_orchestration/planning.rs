//! Pure image resource classification and plan verification.

use cdenv_core::ContainerId;
use cdenv_devcontainer::{
    MountKind, PlannedMount, PortPlan, PublicationBinding, PublicationProtocol, RuntimePlan,
};

use super::ImageContainerError;
use crate::{
    ContainerInspection, CorrelatedContainers, DockerResourceIdentity, ImageId, ImageInspection,
    InspectedMount, InspectedPortBinding,
};

/// Live discovery classification that never chooses among unsafe matches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageContainerMatchState {
    /// No current or stale labeled containers exist.
    Missing,
    /// Only prior/malformed generations exist.
    StaleOnly,
    /// The exact recorded current generation is stopped.
    RecordedStopped,
    /// The exact recorded current generation is already running.
    RecordedRunning,
    /// One current-generation container exists but does not match persisted identity.
    ExternalReplacement,
    /// More than one current-generation container exists.
    AmbiguousCurrent,
}

/// Classifies correlated matches without selecting an arbitrary container.
#[must_use]
pub fn classify_image_container_matches(
    matches: &CorrelatedContainers,
    recorded: Option<&ContainerId>,
) -> ImageContainerMatchState {
    match matches.current.as_slice() {
        [] if matches.stale.is_empty() => ImageContainerMatchState::Missing,
        [] => ImageContainerMatchState::StaleOnly,
        [_first, _second, ..] => ImageContainerMatchState::AmbiguousCurrent,
        [current] if recorded.is_some_and(|id| id == &current.id) && current.is_running() => {
            ImageContainerMatchState::RecordedRunning
        }
        [current] if recorded.is_some_and(|id| id == &current.id) => {
            ImageContainerMatchState::RecordedStopped
        }
        [_] => ImageContainerMatchState::ExternalReplacement,
    }
}

pub(super) fn verify_built_image(
    image: &ImageInspection,
    claim: &ImageId,
    identity: DockerResourceIdentity<'_>,
) -> Result<(), ImageContainerError> {
    if &image.id != claim {
        return Err(ImageContainerError::ImageMismatch { field: "image ID" });
    }
    for (key, expected) in [
        ("cdenv.installation", identity.installation.to_string()),
        ("cdenv.workspace", identity.workspace.to_string()),
        ("cdenv.generation", identity.generation.to_string()),
        ("cdenv.profile", identity.profile.to_string()),
        ("cdenv.generated", "true".to_owned()),
    ] {
        if image.labels.get(key) != Some(&expected) {
            return Err(ImageContainerError::ImageMismatch { field: key });
        }
    }
    Ok(())
}

pub(super) fn verify_runtime(
    inspection: &ContainerInspection,
    runtime: &RuntimePlan,
    ports: &PortPlan,
) -> Result<(), ImageContainerError> {
    if inspection.user != runtime.container_user.as_str() {
        return Err(ImageContainerError::RuntimeMismatch {
            field: "container user",
        });
    }
    if inspection.working_directory != runtime.workspace.folder.as_str() {
        return Err(ImageContainerError::RuntimeMismatch {
            field: "workspace folder",
        });
    }
    let expected_mounts = std::iter::once(&runtime.workspace.mount)
        .chain(&runtime.mounts)
        .collect::<Vec<_>>();
    if inspection.mounts.len() != expected_mounts.len()
        || expected_mounts.iter().any(|expected| {
            !inspection
                .mounts
                .iter()
                .any(|actual| mount_matches(actual, expected))
        })
    {
        return Err(ImageContainerError::RuntimeMismatch { field: "mounts" });
    }
    if !ports_match(&inspection.ports, ports) {
        return Err(ImageContainerError::RuntimeMismatch { field: "ports" });
    }
    Ok(())
}

fn mount_matches(actual: &InspectedMount, expected: &PlannedMount) -> bool {
    let kind = match expected.kind {
        MountKind::Bind => "bind",
        MountKind::Volume => "volume",
    };
    actual.kind == kind
        && actual.source == expected.source
        && actual.target == expected.target.as_str()
        && expected.options.iter().all(|option| {
            let expected = option.value.as_ref().map_or_else(
                || option.name.clone(),
                |value| format!("{}={value}", option.name),
            );
            actual
                .mode
                .as_deref()
                .unwrap_or_default()
                .split(',')
                .any(|value| value == expected)
        })
}

fn ports_match(actual: &[InspectedPortBinding], expected: &PortPlan) -> bool {
    let expected_count: usize = expected
        .publications
        .iter()
        .map(|publication| {
            usize::from(
                publication.container_ports.end.get() - publication.container_ports.start.get(),
            ) + 1
        })
        .sum();
    if actual.len() != expected_count {
        return false;
    }
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
                        && host_ip_matches(binding.host_ip.as_deref(), publication.binding)
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

fn host_ip_matches(actual: Option<&str>, expected: PublicationBinding) -> bool {
    match expected {
        PublicationBinding::Loopback(address) | PublicationBinding::NonLoopback(address) => {
            actual == Some(address.to_string().as_str())
        }
        PublicationBinding::AllInterfaces => {
            actual.is_none_or(|value| matches!(value, "" | "0.0.0.0" | "::"))
        }
    }
}
