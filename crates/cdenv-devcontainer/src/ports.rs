//! Pure publication and declared-forward planning.

use std::net::IpAddr;
use std::num::NonZeroU16;

use serde::Serialize;
use thiserror::Error;

use crate::{
    AppPort, AutoForwardAction, EffectiveMetadata, ForwardPort, PortAttributes, PortProtocol,
    RawProfile, RawScenario,
};

/// A validated non-zero TCP/UDP port.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct PortNumber(NonZeroU16);

impl PortNumber {
    /// Creates a non-zero port.
    ///
    /// # Errors
    ///
    /// Returns an error for zero. Values above 65535 cannot be represented by the input type.
    pub fn new(value: u16) -> Result<Self, PortNumberError> {
        NonZeroU16::new(value).map(Self).ok_or(PortNumberError)
    }

    /// Returns the numeric port.
    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

/// Zero is not a valid publication or forwarding port.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("port must be between 1 and 65535")]
pub struct PortNumberError;

/// One port or an inclusive Docker publication range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortRange {
    /// First port.
    pub start: PortNumber,
    /// Last port.
    pub end: PortNumber,
}

impl PortRange {
    fn overlaps(self, other: Self) -> bool {
        self.start <= other.end && other.start <= self.end
    }

    fn width(self) -> u16 {
        self.end.get() - self.start.get()
    }
}

/// Protocol suffix accepted by Docker publication syntax.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PublicationProtocol {
    /// TCP publication.
    #[default]
    Tcp,
    /// UDP publication.
    Udp,
    /// SCTP publication.
    Sctp,
}

/// Host address semantics of a requested publication.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "address")]
pub enum PublicationBinding {
    /// Explicit IPv4 or IPv6 loopback.
    Loopback(IpAddr),
    /// Explicit non-loopback address.
    NonLoopback(IpAddr),
    /// No address was supplied, so Docker may bind all host interfaces.
    AllInterfaces,
}

impl PublicationBinding {
    const fn may_expose_non_loopback(self) -> bool {
        !matches!(self, Self::Loopback(_))
    }

    const fn address_family(self) -> Option<bool> {
        match self {
            Self::Loopback(address) | Self::NonLoopback(address) => Some(address.is_ipv6()),
            Self::AllInterfaces => None,
        }
    }
}

/// Immutable requested Docker publication.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PublicationRequest {
    /// Exact Docker `--publish` argument generated or retained from configuration.
    pub argument: String,
    /// Host bind semantics.
    pub binding: PublicationBinding,
    /// Fixed host range, or `None` when Docker should assign a port.
    pub host_ports: Option<PortRange>,
    /// Container port/range.
    pub container_ports: PortRange,
    /// Transport protocol.
    pub protocol: PublicationProtocol,
}

/// Host target used by a declared forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind", content = "service")]
pub enum ForwardTargetHost {
    /// Primary container localhost.
    ContainerLoopback,
    /// A named Compose service on the Compose network.
    ComposeService(String),
}

/// Effective attributes attached to an explicit requested forward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EffectivePortAttributes {
    /// Notification/rendering action. `ignore` cannot suppress an explicit request.
    pub on_auto_forward: AutoForwardAction,
    /// V1 reports elevation guidance but never elevates silently.
    pub elevate_if_needed: bool,
    /// Optional endpoint label.
    pub label: Option<String>,
    /// Whether failure to acquire the requested local port degrades `up`.
    pub require_local_port: bool,
    /// Optional URL protocol.
    pub protocol: Option<PortProtocol>,
}

/// Immutable requested forwarding endpoint. Planning never binds or assigns listeners.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardRequest {
    /// Requested local loopback port.
    pub requested_local: PortNumber,
    /// Assigned port remains absent until the forwarding adapter binds a listener.
    pub assigned_local: Option<PortNumber>,
    /// Container/service destination host.
    pub target_host: ForwardTargetHost,
    /// Destination port.
    pub target_port: PortNumber,
    /// Effective rendering and allocation attributes.
    pub attributes: EffectivePortAttributes,
}

/// Structured, non-fatal V1 port interpretation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PortPlanningWarning {
    /// Exact source property.
    pub property_path: String,
    /// Stable warning category.
    pub kind: PortPlanningWarningKind,
}

/// Stable port warning category.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PortPlanningWarningKind {
    /// Publication may be reachable from a non-loopback interface.
    NonLoopbackPublication,
    /// An attribute trigger requires unsupported process/listener discovery.
    DeferredDiscovery,
    /// An explicit `forwardPorts` entry overrides `onAutoForward: ignore`.
    ExplicitForwardOverridesIgnore,
    /// Browser launch was requested; cdenv reports the URL without launching UI.
    BrowserLaunchSuppressed,
    /// Embedded preview was requested, but cdenv has no embedded preview UI.
    EmbeddedPreviewUnsupported,
}

/// Complete requested port plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortPlan {
    /// Create-time Docker publications.
    pub publications: Vec<PublicationRequest>,
    /// Runtime host-supervisor forwards.
    pub forwards: Vec<ForwardRequest>,
    /// Structured warnings produced without probing or binding.
    pub warnings: Vec<PortPlanningWarning>,
}

impl PortPlan {
    /// Produces small requested-versus-assigned rendering inputs.
    #[must_use]
    pub fn rendering_inputs(&self) -> Vec<ForwardingRenderInput> {
        self.forwards
            .iter()
            .map(|forward| ForwardingRenderInput {
                requested_local: forward.requested_local,
                assigned_local: forward.assigned_local,
                target_host: forward.target_host.clone(),
                target_port: forward.target_port,
                label: forward.attributes.label.clone(),
                protocol: forward.attributes.protocol,
                url: forward.assigned_local.and_then(|assigned| {
                    forward.attributes.protocol.map(|protocol| {
                        let scheme = match protocol {
                            PortProtocol::Http => "http",
                            PortProtocol::Https => "https",
                        };
                        format!("{scheme}://127.0.0.1:{}", assigned.get())
                    })
                }),
            })
            .collect()
    }
}

/// Persistence-safe endpoint rendering data containing no captured environment values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ForwardingRenderInput {
    /// Configuration-requested local port.
    pub requested_local: PortNumber,
    /// Actual bound local port, absent during planning.
    pub assigned_local: Option<PortNumber>,
    /// Destination host semantics.
    pub target_host: ForwardTargetHost,
    /// Destination port.
    pub target_port: PortNumber,
    /// Optional display label.
    pub label: Option<String>,
    /// Optional URL protocol.
    pub protocol: Option<PortProtocol>,
    /// Resolved assigned endpoint URL, once a listener has been assigned.
    pub url: Option<String>,
}

/// Plans publications and forwards without Docker, sockets, or process discovery.
///
/// # Errors
///
/// Returns an exact property-path error for invalid Docker publication syntax, conflicting
/// publications/forwards, or Compose service targets used outside a Compose scenario.
#[expect(
    clippy::too_many_lines,
    reason = "straight-line assembly keeps warning and exact property-path ordering auditable"
)]
pub fn plan_ports(
    profile: &RawProfile,
    effective: &EffectiveMetadata,
) -> Result<PortPlan, PortPlanningError> {
    let mut publications = Vec::new();
    let mut warnings = Vec::new();
    let app_ports = &profile.common.app_ports;
    for (index, app_port) in app_ports.iter().enumerate() {
        let property = app_port_path(app_ports.len(), index);
        let publication = match app_port {
            AppPort::Number(port) => numeric_publication(*port, &property)?,
            AppPort::DockerArgument(argument) => parse_publication(argument, &property)?,
        };
        if publication.binding.may_expose_non_loopback() {
            warnings.push(PortPlanningWarning {
                property_path: property.clone(),
                kind: PortPlanningWarningKind::NonLoopbackPublication,
            });
        }
        if publications
            .iter()
            .any(|existing| publications_conflict(existing, &publication))
        {
            return Err(PortPlanningError::new(
                property,
                PortPlanningErrorKind::PublicationConflict,
                "publication conflicts with an earlier fixed host binding",
            ));
        }
        publications.push(publication);
    }

    for key in effective.ports_attributes.keys() {
        let property = format!(
            "$.portsAttributes[{}]",
            serde_json::Value::String(key.clone())
        );
        match attribute_key_kind(key) {
            AttributeKeyKind::Exact => {}
            AttributeKeyKind::Deferred => warnings.push(PortPlanningWarning {
                property_path: property,
                kind: PortPlanningWarningKind::DeferredDiscovery,
            }),
            AttributeKeyKind::InvalidPort => {
                return Err(PortPlanningError::new(
                    property,
                    PortPlanningErrorKind::InvalidPort,
                    "port attribute key contains a port outside 1 through 65535",
                ));
            }
        }
    }
    if effective.other_ports_attributes.is_some() {
        warnings.push(PortPlanningWarning {
            property_path: "$.otherPortsAttributes".to_owned(),
            kind: PortPlanningWarningKind::DeferredDiscovery,
        });
    }

    let compose = matches!(profile.scenario, RawScenario::Compose(_));
    let mut forwards = Vec::with_capacity(effective.forward_ports.len());
    for (index, forward) in effective.forward_ports.iter().enumerate() {
        let property = format!("$.forwardPorts[{index}]");
        let (target_host, port, service_key) = match forward {
            ForwardPort::Container(port) => (ForwardTargetHost::ContainerLoopback, *port, None),
            ForwardPort::Service { service, port } if compose => (
                ForwardTargetHost::ComposeService(service.clone()),
                *port,
                Some(format!("{service}:{port}")),
            ),
            ForwardPort::Service { .. } => {
                return Err(PortPlanningError::new(
                    property,
                    PortPlanningErrorKind::ServiceTargetOutsideCompose,
                    "service forwarding targets require a Compose scenario",
                ));
            }
        };
        let port = PortNumber::new(port).map_err(|_| {
            PortPlanningError::new(
                property.clone(),
                PortPlanningErrorKind::InvalidPort,
                "forward port must be between 1 and 65535",
            )
        })?;
        if forwards
            .iter()
            .any(|existing: &ForwardRequest| existing.requested_local == port)
        {
            return Err(PortPlanningError::new(
                property,
                PortPlanningErrorKind::ForwardConflict,
                "multiple forwards request the same local port",
            ));
        }
        let attributes = service_key
            .as_deref()
            .and_then(|key| effective.ports_attributes.get(key))
            .or_else(|| effective.ports_attributes.get(&port.get().to_string()))
            .map_or_else(
                EffectivePortAttributes::default,
                EffectivePortAttributes::from,
            );
        match attributes.on_auto_forward {
            AutoForwardAction::Ignore => warnings.push(PortPlanningWarning {
                property_path: property.clone(),
                kind: PortPlanningWarningKind::ExplicitForwardOverridesIgnore,
            }),
            AutoForwardAction::OpenBrowser | AutoForwardAction::OpenBrowserOnce => {
                warnings.push(PortPlanningWarning {
                    property_path: property.clone(),
                    kind: PortPlanningWarningKind::BrowserLaunchSuppressed,
                });
            }
            AutoForwardAction::OpenPreview => warnings.push(PortPlanningWarning {
                property_path: property.clone(),
                kind: PortPlanningWarningKind::EmbeddedPreviewUnsupported,
            }),
            AutoForwardAction::Notify | AutoForwardAction::Silent => {}
        }
        forwards.push(ForwardRequest {
            requested_local: port,
            assigned_local: None,
            target_host,
            target_port: port,
            attributes,
        });
    }
    Ok(PortPlan {
        publications,
        forwards,
        warnings,
    })
}

impl Default for EffectivePortAttributes {
    fn default() -> Self {
        Self {
            on_auto_forward: AutoForwardAction::Notify,
            elevate_if_needed: false,
            label: None,
            require_local_port: false,
            protocol: None,
        }
    }
}

impl From<&PortAttributes> for EffectivePortAttributes {
    fn from(value: &PortAttributes) -> Self {
        Self {
            on_auto_forward: value.on_auto_forward.unwrap_or(AutoForwardAction::Notify),
            elevate_if_needed: value.elevate_if_needed.unwrap_or(false),
            label: value.label.clone(),
            require_local_port: value.require_local_port.unwrap_or(false),
            protocol: value.protocol,
        }
    }
}

fn app_port_path(count: usize, index: usize) -> String {
    if count == 1 {
        "$.appPort".to_owned()
    } else {
        format!("$.appPort[{index}]")
    }
}

fn numeric_publication(port: u16, property: &str) -> Result<PublicationRequest, PortPlanningError> {
    let port = PortNumber::new(port).map_err(|_| {
        PortPlanningError::new(
            property,
            PortPlanningErrorKind::InvalidPort,
            "publication port must be between 1 and 65535",
        )
    })?;
    let range = PortRange {
        start: port,
        end: port,
    };
    Ok(PublicationRequest {
        argument: format!("127.0.0.1:{port}:{port}", port = port.get()),
        binding: PublicationBinding::Loopback(IpAddr::from([127, 0, 0, 1])),
        host_ports: Some(range),
        container_ports: range,
        protocol: PublicationProtocol::Tcp,
    })
}

fn parse_publication(
    argument: &str,
    property: &str,
) -> Result<PublicationRequest, PortPlanningError> {
    if argument.is_empty() || argument.chars().any(char::is_whitespace) {
        return Err(invalid_publication(property));
    }
    let (without_protocol, protocol) = match argument.rsplit_once('/') {
        Some((value, "tcp")) => (value, PublicationProtocol::Tcp),
        Some((value, "udp")) => (value, PublicationProtocol::Udp),
        Some((value, "sctp")) => (value, PublicationProtocol::Sctp),
        Some(_) => return Err(invalid_publication(property)),
        None => (argument, PublicationProtocol::Tcp),
    };
    let (binding, host, container) = if let Some(rest) = without_protocol.strip_prefix('[') {
        let (address, ports) = rest
            .split_once("]:")
            .ok_or_else(|| invalid_publication(property))?;
        let address = address
            .parse::<IpAddr>()
            .map_err(|_| invalid_publication(property))?;
        if !address.is_ipv6() {
            return Err(invalid_publication(property));
        }
        let (host, container) = ports.split_once(':').map_or(("", ports), |parts| parts);
        (binding(address), host, container)
    } else {
        let fields = without_protocol.split(':').collect::<Vec<_>>();
        match fields.as_slice() {
            [container] => (PublicationBinding::AllInterfaces, "", *container),
            [address_or_host, container] => {
                if let Ok(address) = address_or_host.parse::<IpAddr>() {
                    (binding(address), "", *container)
                } else {
                    (
                        PublicationBinding::AllInterfaces,
                        *address_or_host,
                        *container,
                    )
                }
            }
            ["", host, container] => (PublicationBinding::AllInterfaces, *host, *container),
            [address, host, container] => {
                let address = address
                    .parse::<IpAddr>()
                    .map_err(|_| invalid_publication(property))?;
                (binding(address), *host, *container)
            }
            _ => return Err(invalid_publication(property)),
        }
    };
    let container_ports = parse_range(container).ok_or_else(|| invalid_publication(property))?;
    let host_ports = if host.is_empty() {
        None
    } else {
        Some(parse_range(host).ok_or_else(|| invalid_publication(property))?)
    };
    if host_ports.is_some_and(|ports| ports.width() != container_ports.width()) {
        return Err(invalid_publication(property));
    }
    Ok(PublicationRequest {
        argument: argument.to_owned(),
        binding,
        host_ports,
        container_ports,
        protocol,
    })
}

fn parse_range(value: &str) -> Option<PortRange> {
    let (start, end) = value.split_once('-').map_or((value, value), |parts| parts);
    let start = start
        .parse::<u16>()
        .ok()
        .and_then(|value| PortNumber::new(value).ok())?;
    let end = end
        .parse::<u16>()
        .ok()
        .and_then(|value| PortNumber::new(value).ok())?;
    (start <= end).then_some(PortRange { start, end })
}

fn binding(address: IpAddr) -> PublicationBinding {
    if address.is_loopback() {
        PublicationBinding::Loopback(address)
    } else {
        PublicationBinding::NonLoopback(address)
    }
}

fn publications_conflict(left: &PublicationRequest, right: &PublicationRequest) -> bool {
    left.protocol == right.protocol
        && left
            .host_ports
            .zip(right.host_ports)
            .is_some_and(|(left_ports, right_ports)| {
                left_ports.overlaps(right_ports) && bindings_overlap(left.binding, right.binding)
            })
}

fn bindings_overlap(left: PublicationBinding, right: PublicationBinding) -> bool {
    match (left.address_family(), right.address_family()) {
        (Some(left_v6), Some(right_v6)) if left_v6 != right_v6 => false,
        _ => {
            matches!(left, PublicationBinding::AllInterfaces)
                || matches!(right, PublicationBinding::AllInterfaces)
                || left == right
        }
    }
}

#[derive(Clone, Copy)]
enum AttributeKeyKind {
    Exact,
    Deferred,
    InvalidPort,
}

fn attribute_key_kind(key: &str) -> AttributeKeyKind {
    if !key.is_empty() && key.bytes().all(|byte| byte.is_ascii_digit()) {
        return if key.parse::<u16>().is_ok_and(|port| port > 0) {
            AttributeKeyKind::Exact
        } else {
            AttributeKeyKind::InvalidPort
        };
    }
    if let Some((service, port)) = key.rsplit_once(':')
        && valid_service_name(service)
        && port.bytes().all(|byte| byte.is_ascii_digit())
    {
        return if port.parse::<u16>().is_ok_and(|port| port > 0) {
            AttributeKeyKind::Exact
        } else {
            AttributeKeyKind::InvalidPort
        };
    }
    AttributeKeyKind::Deferred
}

fn valid_service_name(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn invalid_publication(property: &str) -> PortPlanningError {
    PortPlanningError::new(
        property,
        PortPlanningErrorKind::InvalidPublication,
        "invalid Docker publication; use [host-address:]host-port:container-port[/protocol]",
    )
}

/// Stable category of port planning failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PortPlanningErrorKind {
    /// Port was zero or outside its supported range.
    InvalidPort,
    /// Explicit Docker publication syntax was malformed.
    InvalidPublication,
    /// Two fixed publications overlap.
    PublicationConflict,
    /// Two forward requests need the same local port.
    ForwardConflict,
    /// A service target was used without Compose.
    ServiceTargetOutsideCompose,
}

/// Exact property-located port planning error that omits rejected values.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{property_path}: {message}")]
pub struct PortPlanningError {
    /// Exact source property.
    pub property_path: String,
    /// Stable failure category.
    pub kind: PortPlanningErrorKind,
    message: &'static str,
}

impl PortPlanningError {
    fn new(
        property_path: impl Into<String>,
        kind: PortPlanningErrorKind,
        message: &'static str,
    ) -> Self {
        Self {
            property_path: property_path.into(),
            kind,
            message,
        }
    }
}
