//! Pure Dev Container Feature references, metadata, dependency resolution, and lock values.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::net::IpAddr;

use serde_json::{Map, Value};
use thiserror::Error;
use url::Url;

use crate::{CommandValue, LifecycleCommands, RawCommand, RawMount};

const FEATURE_METADATA_PROPERTIES: &[&str] = &[
    "id",
    "version",
    "name",
    "description",
    "documentationURL",
    "licenseURL",
    "keywords",
    "legacyIds",
    "deprecated",
    "options",
    "dependsOn",
    "installsAfter",
    "containerEnv",
    "mounts",
    "capAdd",
    "securityOpt",
    "entrypoint",
    "customizations",
    "init",
    "privileged",
    "onCreateCommand",
    "updateContentCommand",
    "postCreateCommand",
    "postStartCommand",
    "postAttachCommand",
];

/// A normalized, supported Feature reference.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureReference {
    /// A fully qualified anonymous OCI reference.
    Oci(String),
    /// An unauthenticated HTTPS tarball URL.
    Https(String),
    /// A directory contained below the selected configuration directory.
    Local(String),
}

impl FeatureReference {
    /// Normalizes a supported Feature reference without performing I/O.
    ///
    /// # Errors
    ///
    /// Rejects credentials, insecure or private endpoints, traversal, legacy identifiers, and
    /// malformed OCI, HTTPS, or local forms.
    pub fn parse(input: &str) -> Result<Self, FeatureError> {
        if input.starts_with("./") {
            return normalize_local(input).map(Self::Local);
        }
        if input.starts_with("https://") {
            return normalize_https(input).map(Self::Https);
        }
        if input.contains("://") || input.starts_with("http:") {
            return Err(FeatureError::UnsupportedReference {
                reference: input.to_owned(),
                reason: "Feature transport must use verified HTTPS".to_owned(),
            });
        }
        normalize_oci(input).map(Self::Oci)
    }

    /// Returns the canonical reference used for lookup and lock keys.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Oci(value) | Self::Https(value) | Self::Local(value) => value,
        }
    }

    fn resource_name(&self) -> &str {
        match self {
            Self::Oci(value) => oci_resource_name(value),
            Self::Https(value) | Self::Local(value) => value,
        }
    }

    fn selector(&self) -> &str {
        match self {
            Self::Oci(value) => value
                .strip_prefix(oci_resource_name(value))
                .unwrap_or_default(),
            Self::Https(_) | Self::Local(_) => "",
        }
    }
}

impl fmt::Display for FeatureReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn normalize_local(input: &str) -> Result<String, FeatureError> {
    let mut parts = Vec::new();
    for part in input[2..].split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(unsupported(
                    input,
                    "local Feature escapes its configuration directory",
                ));
            }
            value if value.contains('\0') || value.contains('\\') => {
                return Err(unsupported(
                    input,
                    "local Feature path contains an unsupported component",
                ));
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err(unsupported(
            input,
            "local Feature path must name a directory",
        ));
    }
    Ok(format!("./{}", parts.join("/")))
}

fn normalize_https(input: &str) -> Result<String, FeatureError> {
    let mut url = Url::parse(input).map_err(|error| unsupported(input, error.to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(unsupported(
            input,
            "Feature URL credentials are unsupported",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| unsupported(input, "Feature URL requires a public host"))?;
    reject_private_host(input, host)?;
    if url.fragment().is_some() {
        return Err(unsupported(input, "Feature URL fragments are unsupported"));
    }
    url.set_host(Some(&host.to_ascii_lowercase()))
        .map_err(|_| unsupported(input, "Feature URL host is invalid"))?;
    Ok(url.to_string())
}

fn normalize_oci(input: &str) -> Result<String, FeatureError> {
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return Err(unsupported(
            input,
            "OCI Feature reference is empty or contains whitespace",
        ));
    }
    if input.contains('@') && !input.contains("@sha256:") {
        return Err(unsupported(
            input,
            "OCI credentials and non-SHA-256 digests are unsupported",
        ));
    }
    let (name_and_tag, digest) = input
        .split_once('@')
        .map_or((input, None), |(name, digest)| (name, Some(digest)));
    if let Some(digest) = digest {
        validate_sha256(digest).map_err(|reason| unsupported(input, reason))?;
    }
    let slash = name_and_tag
        .find('/')
        .ok_or_else(|| unsupported(input, "OCI Feature reference must be fully qualified"))?;
    let registry = &name_and_tag[..slash];
    reject_private_host(input, registry.split(':').next().unwrap_or(registry))?;
    if !(registry.contains('.') || registry.contains(':')) {
        return Err(unsupported(
            input,
            "OCI Feature reference must include a registry host",
        ));
    }
    let repository_and_tag = &name_and_tag[slash + 1..];
    let last = repository_and_tag.rsplit('/').next().unwrap_or_default();
    let has_tag = last.contains(':');
    let repository = if has_tag {
        repository_and_tag
            .rsplit_once(':')
            .map_or(repository_and_tag, |(name, _)| name)
    } else {
        repository_and_tag
    };
    if repository.is_empty()
        || repository.split('/').any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
    {
        return Err(unsupported(
            input,
            "OCI repository must be lowercase and fully qualified",
        ));
    }
    if has_tag && last.rsplit_once(':').is_some_and(|(_, tag)| tag.is_empty()) {
        return Err(unsupported(input, "OCI Feature tag cannot be empty"));
    }
    let mut normalized = format!("{}/{repository_and_tag}", registry.to_ascii_lowercase());
    if digest.is_none() && !has_tag {
        normalized.push_str(":latest");
    }
    if let Some(digest) = digest {
        normalized.push('@');
        normalized.push_str(digest);
    }
    Ok(normalized)
}

fn reject_private_host(reference: &str, host: &str) -> Result<(), FeatureError> {
    let lowercase = host.to_ascii_lowercase();
    let private_name = lowercase == "localhost"
        || lowercase.ends_with(".localhost")
        || lowercase.strip_suffix(".local").is_some()
        || !lowercase.contains('.');
    let private_ip = lowercase
        .parse::<IpAddr>()
        .is_ok_and(|address| match address {
            IpAddr::V4(value) => value.is_private() || value.is_loopback() || value.is_link_local(),
            IpAddr::V6(value) => {
                value.is_loopback() || value.is_unique_local() || value.is_unicast_link_local()
            }
        });
    if private_name || private_ip {
        return Err(unsupported(
            reference,
            "private or local Feature endpoints are unsupported",
        ));
    }
    Ok(())
}

fn unsupported(reference: &str, reason: impl Into<String>) -> FeatureError {
    FeatureError::UnsupportedReference {
        reference: reference.to_owned(),
        reason: reason.into(),
    }
}

fn validate_sha256(value: &str) -> Result<(), &'static str> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("digest must use SHA-256");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("digest must contain 64 lowercase hexadecimal characters");
    }
    Ok(())
}

fn oci_resource_name(value: &str) -> &str {
    let without_digest = value.split('@').next().unwrap_or(value);
    let slash = without_digest.rfind('/').unwrap_or(0);
    without_digest[slash + 1..]
        .find(':')
        .map_or(without_digest, |offset| {
            &without_digest[..slash + 1 + offset]
        })
}

/// A value supplied to a Feature option.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum FeatureValue {
    /// A boolean option value.
    Boolean(bool),
    /// A string option value.
    String(String),
}

impl fmt::Display for FeatureValue {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Boolean(value) => write!(formatter, "{value}"),
            Self::String(value) => formatter.write_str(value),
        }
    }
}

/// The schema and default for one Feature option.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FeatureOption {
    /// A boolean option.
    Boolean {
        /// Value used when omitted.
        default: bool,
    },
    /// A string option.
    String {
        /// Value used when omitted.
        default: String,
        /// If present, the exhaustive allowed-value set.
        allowed: Option<BTreeSet<String>>,
        /// Suggested values which do not restrict custom inputs.
        proposals: BTreeSet<String>,
    },
}

/// Contributions made by one Feature. They are retained together to ensure one application.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FeatureContributions {
    /// Container environment entries.
    pub container_env: BTreeMap<String, String>,
    /// Runtime mounts.
    pub mounts: Vec<RawMount>,
    /// Linux capabilities.
    pub cap_add: Vec<String>,
    /// Container security options.
    pub security_opt: Vec<String>,
    /// Optional Feature entrypoint.
    pub entrypoint: Option<String>,
    /// Lifecycle hooks contributed by the Feature.
    pub lifecycle: LifecycleCommands,
    /// Tool-specific metadata.
    pub customizations: BTreeMap<String, Value>,
    /// Whether an init process is requested.
    pub init: Option<bool>,
    /// Whether privileged mode is requested.
    pub privileged: Option<bool>,
}

/// Validated metadata loaded by a future transport adapter.
#[derive(Clone, Debug, PartialEq)]
pub struct FeatureMetadata {
    /// Feature-local identifier.
    pub id: String,
    /// Feature version.
    pub version: String,
    /// Available options.
    pub options: BTreeMap<String, FeatureOption>,
    /// Recursive hard dependency requests.
    pub depends_on: BTreeMap<FeatureReference, BTreeMap<String, FeatureValue>>,
    /// Non-recursive soft order hints.
    pub installs_after: Vec<String>,
    /// Runtime and metadata contributions.
    pub contributions: FeatureContributions,
}

impl FeatureMetadata {
    /// Parses strict Feature metadata from an already loaded JSON value.
    ///
    /// # Errors
    ///
    /// Returns a property-located metadata or reference error.
    pub fn from_value(value: &Value) -> Result<Self, FeatureError> {
        let object = value
            .as_object()
            .ok_or_else(|| metadata("$", "Feature metadata must be an object"))?;
        if let Some(name) = object
            .keys()
            .find(|name| !FEATURE_METADATA_PROPERTIES.contains(&name.as_str()))
        {
            return Err(metadata(
                format!("$.{name}"),
                "unknown Feature metadata property",
            ));
        }
        let id = required_string(object, "id")?;
        if id.is_empty() || id.contains('/') || id.chars().any(char::is_whitespace) {
            return Err(metadata(
                "$.id",
                "Feature id must be a non-empty local identifier",
            ));
        }
        let version = required_string(object, "version")?;
        if version.is_empty() {
            return Err(metadata("$.version", "Feature version cannot be empty"));
        }
        let options = parse_option_definitions(object.get("options"))?;
        let depends_on = parse_dependencies(object.get("dependsOn"))?;
        let installs_after = string_array(object.get("installsAfter"), "$.installsAfter")?;
        let contributions = FeatureContributions {
            container_env: string_map(object.get("containerEnv"), "$.containerEnv")?,
            mounts: parse_mounts(object.get("mounts"))?,
            cap_add: string_array(object.get("capAdd"), "$.capAdd")?,
            security_opt: string_array(object.get("securityOpt"), "$.securityOpt")?,
            entrypoint: optional_string(object, "entrypoint")?,
            lifecycle: LifecycleCommands {
                initialize: None,
                on_create: parse_command(object.get("onCreateCommand"), "$.onCreateCommand")?,
                update_content: parse_command(
                    object.get("updateContentCommand"),
                    "$.updateContentCommand",
                )?,
                post_create: parse_command(object.get("postCreateCommand"), "$.postCreateCommand")?,
                post_start: parse_command(object.get("postStartCommand"), "$.postStartCommand")?,
                post_attach: parse_command(object.get("postAttachCommand"), "$.postAttachCommand")?,
            },
            customizations: object_map(object.get("customizations"), "$.customizations")?,
            init: optional_bool(object, "init")?,
            privileged: optional_bool(object, "privileged")?,
        };
        Ok(Self {
            id,
            version,
            options,
            depends_on,
            installs_after,
            contributions,
        })
    }
}

fn parse_option_definitions(
    value: Option<&Value>,
) -> Result<BTreeMap<String, FeatureOption>, FeatureError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| metadata("$.options", "options must be an object"))?;
    object
        .iter()
        .map(|(name, value)| {
            let path = format!("$.options[{name:?}]");
            let definition = value
                .as_object()
                .ok_or_else(|| metadata(&path, "option definition must be an object"))?;
            let kind = required_string(definition, "type")
                .map_err(|_| metadata(format!("{path}.type"), "option type is required"))?;
            let default = definition
                .get("default")
                .ok_or_else(|| metadata(format!("{path}.default"), "option default is required"))?;
            let option = match kind.as_str() {
                "boolean" => FeatureOption::Boolean {
                    default: default.as_bool().ok_or_else(|| {
                        metadata(
                            format!("{path}.default"),
                            "boolean option requires a boolean default",
                        )
                    })?,
                },
                "string" => {
                    let default = default
                        .as_str()
                        .ok_or_else(|| {
                            metadata(
                                format!("{path}.default"),
                                "string option requires a string default",
                            )
                        })?
                        .to_owned();
                    let allowed_values =
                        string_array(definition.get("enum"), &format!("{path}.enum"))?;
                    let proposals =
                        string_array(definition.get("proposals"), &format!("{path}.proposals"))?
                            .into_iter()
                            .collect();
                    let allowed = (!allowed_values.is_empty())
                        .then(|| allowed_values.into_iter().collect::<BTreeSet<_>>());
                    if allowed
                        .as_ref()
                        .is_some_and(|values| !values.contains(&default))
                    {
                        return Err(metadata(
                            format!("{path}.default"),
                            "default is not in the option enum",
                        ));
                    }
                    FeatureOption::String {
                        default,
                        allowed,
                        proposals,
                    }
                }
                _ => {
                    return Err(metadata(
                        format!("{path}.type"),
                        "option type must be boolean or string",
                    ));
                }
            };
            Ok((name.clone(), option))
        })
        .collect()
}

fn parse_dependencies(
    value: Option<&Value>,
) -> Result<BTreeMap<FeatureReference, BTreeMap<String, FeatureValue>>, FeatureError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value
        .as_object()
        .ok_or_else(|| metadata("$.dependsOn", "dependsOn must be an object"))?;
    object
        .iter()
        .map(|(reference, options)| {
            let reference_value = FeatureReference::parse(reference)?;
            let options = options.as_object().ok_or_else(|| {
                metadata(
                    format!("$.dependsOn[{reference:?}]"),
                    "dependency options must be an object",
                )
            })?;
            let values = options
                .iter()
                .map(|(name, value)| {
                    parse_feature_value(value)
                        .map(|value| (name.clone(), value))
                        .ok_or_else(|| {
                            metadata(
                                format!("$.dependsOn[{reference:?}][{name:?}]"),
                                "option must be boolean or string",
                            )
                        })
                })
                .collect::<Result<_, _>>()?;
            Ok((reference_value, values))
        })
        .collect()
}

fn parse_mounts(value: Option<&Value>) -> Result<Vec<RawMount>, FeatureError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let array = value
        .as_array()
        .ok_or_else(|| metadata("$.mounts", "mounts must be an array"))?;
    array
        .iter()
        .enumerate()
        .map(|(index, value)| {
            if let Some(value) = value.as_str() {
                return Ok(RawMount::String(value.to_owned()));
            }
            let object = value.as_object().ok_or_else(|| {
                metadata(
                    format!("$.mounts[{index}]"),
                    "mount must be a string or object",
                )
            })?;
            let kind = required_string(object, "type")?;
            let target = required_string(object, "target")?;
            let source = optional_string(object, "source")?;
            let kind = match kind.as_str() {
                "bind" => crate::MountKind::Bind,
                "volume" => crate::MountKind::Volume,
                _ => {
                    return Err(metadata(
                        format!("$.mounts[{index}].type"),
                        "mount type must be bind or volume",
                    ));
                }
            };
            Ok(RawMount::Object {
                kind,
                source,
                target,
            })
        })
        .collect()
}

fn parse_command(value: Option<&Value>, path: &str) -> Result<Option<RawCommand>, FeatureError> {
    let Some(value) = value else { return Ok(None) };
    if let Some(command) = value.as_str() {
        return Ok(Some(RawCommand::Shell(command.to_owned())));
    }
    if let Some(items) = value.as_array() {
        return Ok(Some(RawCommand::Exec(
            items
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    item.as_str().map(str::to_owned).ok_or_else(|| {
                        metadata(
                            format!("{path}[{index}]"),
                            "command argument must be a string",
                        )
                    })
                })
                .collect::<Result<_, _>>()?,
        )));
    }
    let object = value
        .as_object()
        .ok_or_else(|| metadata(path, "command must be a string, array, or object"))?;
    let commands = object
        .iter()
        .map(|(name, value)| {
            let command = if let Some(value) = value.as_str() {
                CommandValue::Shell(value.to_owned())
            } else {
                CommandValue::Exec(
                    value
                        .as_array()
                        .ok_or_else(|| {
                            metadata(
                                format!("{path}[{name:?}]"),
                                "parallel command must be a string or array",
                            )
                        })?
                        .iter()
                        .enumerate()
                        .map(|(index, item)| {
                            item.as_str().map(str::to_owned).ok_or_else(|| {
                                metadata(
                                    format!("{path}[{name:?}][{index}]"),
                                    "command argument must be a string",
                                )
                            })
                        })
                        .collect::<Result<_, _>>()?,
                )
            };
            Ok((name.clone(), command))
        })
        .collect::<Result<_, FeatureError>>()?;
    Ok(Some(RawCommand::Parallel(commands)))
}

fn required_string(object: &Map<String, Value>, name: &str) -> Result<String, FeatureError> {
    object
        .get(name)
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| metadata(format!("$.{name}"), format!("{name} must be a string")))
}
fn optional_string(
    object: &Map<String, Value>,
    name: &str,
) -> Result<Option<String>, FeatureError> {
    object
        .get(name)
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| metadata(format!("$.{name}"), format!("{name} must be a string")))
        })
        .transpose()
}
fn optional_bool(object: &Map<String, Value>, name: &str) -> Result<Option<bool>, FeatureError> {
    object
        .get(name)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| metadata(format!("$.{name}"), format!("{name} must be a boolean")))
        })
        .transpose()
}
fn string_array(value: Option<&Value>, path: &str) -> Result<Vec<String>, FeatureError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    value
        .as_array()
        .ok_or_else(|| metadata(path, "value must be an array"))?
        .iter()
        .enumerate()
        .map(|(index, item)| {
            item.as_str()
                .map(str::to_owned)
                .ok_or_else(|| metadata(format!("{path}[{index}]"), "array item must be a string"))
        })
        .collect()
}
fn string_map(value: Option<&Value>, path: &str) -> Result<BTreeMap<String, String>, FeatureError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .ok_or_else(|| metadata(path, "value must be an object"))?
        .iter()
        .map(|(name, value)| {
            value
                .as_str()
                .map(|value| (name.clone(), value.to_owned()))
                .ok_or_else(|| metadata(format!("{path}[{name:?}]"), "value must be a string"))
        })
        .collect()
}
fn object_map(value: Option<&Value>, path: &str) -> Result<BTreeMap<String, Value>, FeatureError> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    value
        .as_object()
        .map(|object| {
            object
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .ok_or_else(|| metadata(path, "value must be an object"))
}
fn parse_feature_value(value: &Value) -> Option<FeatureValue> {
    value.as_bool().map(FeatureValue::Boolean).or_else(|| {
        value
            .as_str()
            .map(|value| FeatureValue::String(value.to_owned()))
    })
}
fn metadata(path: impl Into<String>, message: impl Into<String>) -> FeatureError {
    FeatureError::InvalidMetadata {
        path: path.into(),
        message: message.into(),
    }
}

/// Content identity established by a future transport adapter.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureInstallIdentity {
    /// OCI manifest digest.
    OciDigest(String),
    /// SHA-256 integrity of an HTTPS tarball.
    HttpsIntegrity(String),
    /// Canonical checkout-contained local source.
    Local(String),
}

impl FeatureInstallIdentity {
    /// Creates an OCI identity after validating the digest.
    ///
    /// # Errors
    ///
    /// Returns [`FeatureError::InvalidIdentity`] for a malformed digest.
    pub fn oci_digest(digest: impl Into<String>) -> Result<Self, FeatureError> {
        let digest = digest.into();
        validate_sha256(&digest).map_err(|message| FeatureError::InvalidIdentity {
            value: digest.clone(),
            message,
        })?;
        Ok(Self::OciDigest(digest))
    }
    /// Creates an HTTPS identity after validating artifact integrity.
    ///
    /// # Errors
    ///
    /// Returns [`FeatureError::InvalidIdentity`] for malformed integrity.
    pub fn https_integrity(integrity: impl Into<String>) -> Result<Self, FeatureError> {
        let integrity = integrity.into();
        validate_sha256(&integrity).map_err(|message| FeatureError::InvalidIdentity {
            value: integrity.clone(),
            message,
        })?;
        Ok(Self::HttpsIntegrity(integrity))
    }
    /// Creates a source-unique local identity.
    #[must_use]
    pub fn local(reference: &FeatureReference) -> Self {
        Self::Local(reference.as_str().to_owned())
    }
}

/// An already fetched Feature supplied to the pure resolver.
#[derive(Clone, Debug, PartialEq)]
pub struct FeaturePackage {
    /// The reference by which this package can be requested.
    pub reference: FeatureReference,
    /// Exact install-content identity.
    pub identity: FeatureInstallIdentity,
    /// Validated Feature metadata.
    pub metadata: FeatureMetadata,
    /// Resolved OCI manifest digest, when applicable.
    pub digest: Option<String>,
    /// Downloaded artifact integrity, when applicable.
    pub integrity: Option<String>,
}

/// One root Feature request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureRequest {
    /// Requested reference.
    pub reference: FeatureReference,
    /// Explicit options (defaults are applied by the resolver).
    pub options: BTreeMap<String, FeatureValue>,
}

impl FeatureRequest {
    /// Creates a request with no explicit options.
    #[must_use]
    pub fn new(reference: FeatureReference) -> Self {
        Self {
            reference,
            options: BTreeMap::new(),
        }
    }
}

/// A resolved Feature in deterministic installation order.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFeature {
    /// Canonical source used for installation.
    pub reference: FeatureReference,
    /// Exact install-content identity.
    pub identity: FeatureInstallIdentity,
    /// Metadata and contributions, represented exactly once.
    pub metadata: FeatureMetadata,
    /// Effective options including defaults.
    pub options: BTreeMap<String, FeatureValue>,
}

/// Complete deterministic resolution result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedFeatures {
    /// Features in installation order. Every identity occurs exactly once.
    pub installation_order: Vec<ResolvedFeature>,
}

#[derive(Clone)]
struct Node {
    package: FeaturePackage,
    options: BTreeMap<String, FeatureValue>,
    explicit_options: BTreeMap<String, FeatureValue>,
    dependencies: BTreeSet<FeatureInstallIdentity>,
    first_path: Vec<FeatureReference>,
    priority: usize,
}

struct Pending {
    reference: FeatureReference,
    options: BTreeMap<String, FeatureValue>,
    parent: Option<FeatureInstallIdentity>,
    path: Vec<FeatureReference>,
}

/// Recursively resolves requests and applies the specification's deterministic round algorithm.
///
/// `packages` is an injected, already-fetched catalog keyed by normalized reference. This function
/// performs no source or filesystem I/O.
///
/// # Errors
///
/// Returns typed errors for missing packages, invalid options, conflicting duplicate requests,
/// invalid override hints, and hard/soft dependency cycles.
pub fn resolve_features(
    requests: &[FeatureRequest],
    packages: &BTreeMap<FeatureReference, FeaturePackage>,
    override_feature_install_order: &[String],
) -> Result<ResolvedFeatures, FeatureError> {
    let mut pending = requests
        .iter()
        .map(|request| Pending {
            reference: request.reference.clone(),
            options: request.options.clone(),
            parent: None,
            path: vec![request.reference.clone()],
        })
        .collect::<VecDeque<_>>();
    let mut nodes: BTreeMap<FeatureInstallIdentity, Node> = BTreeMap::new();

    while let Some(item) = pending.pop_front() {
        let package =
            packages
                .get(&item.reference)
                .ok_or_else(|| FeatureError::MissingPackage {
                    reference: item.reference.clone(),
                    path: render_path(&item.path),
                })?;
        if package.reference != item.reference {
            return Err(FeatureError::CatalogMismatch {
                key: item.reference,
                package: package.reference.clone(),
            });
        }
        let effective = validate_options(&package.metadata, &item.options, &item.path)?;
        if let Some(existing) = nodes.get_mut(&package.identity) {
            if existing.options != effective {
                let option = differing_option(&existing.options, &effective);
                return Err(FeatureError::ConflictingOptions {
                    identity: package.identity.clone(),
                    option,
                    first_path: render_path(&existing.first_path),
                    second_path: render_path(&item.path),
                });
            }
            if let Some(parent) = item.parent
                && let Some(parent_node) = nodes.get_mut(&parent)
            {
                parent_node.dependencies.insert(package.identity.clone());
            }
            continue;
        }
        nodes.insert(
            package.identity.clone(),
            Node {
                package: package.clone(),
                options: effective,
                explicit_options: item.options,
                dependencies: BTreeSet::new(),
                first_path: item.path.clone(),
                priority: 0,
            },
        );
        if let Some(parent) = item.parent
            && let Some(parent_node) = nodes.get_mut(&parent)
        {
            parent_node.dependencies.insert(package.identity.clone());
        }
        for (reference, options) in &package.metadata.depends_on {
            let mut path = item.path.clone();
            path.push(reference.clone());
            pending.push_back(Pending {
                reference: reference.clone(),
                options: options.clone(),
                parent: Some(package.identity.clone()),
                path,
            });
        }
    }

    apply_soft_edges(&mut nodes);
    apply_priorities(&mut nodes, override_feature_install_order)?;
    round_sort(nodes)
}

fn validate_options(
    metadata: &FeatureMetadata,
    provided: &BTreeMap<String, FeatureValue>,
    path: &[FeatureReference],
) -> Result<BTreeMap<String, FeatureValue>, FeatureError> {
    if let Some(name) = provided
        .keys()
        .find(|name| !metadata.options.contains_key(*name))
    {
        return Err(FeatureError::UnknownOption {
            feature: metadata.id.clone(),
            option: name.clone(),
            path: render_path(path),
        });
    }
    metadata
        .options
        .iter()
        .map(|(name, definition)| {
            let value = provided
                .get(name)
                .cloned()
                .unwrap_or_else(|| match definition {
                    FeatureOption::Boolean { default } => FeatureValue::Boolean(*default),
                    FeatureOption::String { default, .. } => FeatureValue::String(default.clone()),
                });
            let valid = match (definition, &value) {
                (FeatureOption::Boolean { .. }, FeatureValue::Boolean(_)) => true,
                (FeatureOption::String { allowed, .. }, FeatureValue::String(value)) => allowed
                    .as_ref()
                    .is_none_or(|allowed| allowed.contains(value)),
                _ => false,
            };
            if !valid {
                return Err(FeatureError::InvalidOption {
                    feature: metadata.id.clone(),
                    option: name.clone(),
                    value,
                    path: render_path(path),
                });
            }
            Ok((name.clone(), value))
        })
        .collect()
}

fn apply_soft_edges(nodes: &mut BTreeMap<FeatureInstallIdentity, Node>) {
    let matches = nodes
        .iter()
        .map(|(identity, node)| {
            (
                node.package.reference.resource_name().to_owned(),
                identity.clone(),
            )
        })
        .collect::<Vec<_>>();
    for node in nodes.values_mut() {
        for hint in &node.package.metadata.installs_after {
            let hint_name = FeatureReference::parse(hint)
                .map_or_else(
                    |_| hint.to_owned(),
                    |reference| reference.resource_name().to_owned(),
                )
                .to_ascii_lowercase();
            node.dependencies.extend(
                matches
                    .iter()
                    .filter(|(name, identity)| {
                        name.eq_ignore_ascii_case(&hint_name) && *identity != node.package.identity
                    })
                    .map(|(_, identity)| identity.clone()),
            );
        }
    }
}

fn apply_priorities(
    nodes: &mut BTreeMap<FeatureInstallIdentity, Node>,
    overrides: &[String],
) -> Result<(), FeatureError> {
    let count = overrides.len();
    for (index, value) in overrides.iter().enumerate() {
        if value.contains('@')
            || value
                .rsplit('/')
                .next()
                .is_some_and(|last| last.contains(':'))
        {
            return Err(FeatureError::InvalidOverride {
                reference: value.clone(),
                message: "override must omit tags, digests, and options",
            });
        }
        let matches = nodes
            .values_mut()
            .filter(|node| {
                node.package
                    .reference
                    .resource_name()
                    .eq_ignore_ascii_case(value)
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(FeatureError::InvalidOverride {
                reference: value.clone(),
                message: "override does not match a resolved Feature",
            });
        }
        for node in matches {
            node.priority = count - index;
        }
    }
    Ok(())
}

fn round_sort(
    mut nodes: BTreeMap<FeatureInstallIdentity, Node>,
) -> Result<ResolvedFeatures, FeatureError> {
    let mut installed = BTreeSet::new();
    let mut output = Vec::with_capacity(nodes.len());
    while !nodes.is_empty() {
        let mut ready = nodes
            .iter()
            .filter(|(_, node)| node.dependencies.is_subset(&installed))
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            let cycle = find_cycle(&nodes).unwrap_or_else(|| nodes.keys().cloned().collect());
            return Err(FeatureError::DependencyCycle {
                cycle: cycle
                    .iter()
                    .map(identity_label)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            });
        }
        let max_priority = ready
            .iter()
            .map(|identity| nodes[identity].priority)
            .max()
            .unwrap_or(0);
        ready.retain(|identity| nodes[identity].priority == max_priority);
        ready.sort_by(|left, right| compare_nodes(&nodes[left], &nodes[right]));
        for identity in ready {
            if let Some(node) = nodes.remove(&identity) {
                installed.insert(identity.clone());
                output.push(ResolvedFeature {
                    reference: node.package.reference,
                    identity,
                    metadata: node.package.metadata,
                    options: node.options,
                });
            }
        }
    }
    Ok(ResolvedFeatures {
        installation_order: output,
    })
}

fn compare_nodes(left: &Node, right: &Node) -> Ordering {
    left.package
        .reference
        .resource_name()
        .cmp(right.package.reference.resource_name())
        .then_with(|| {
            left.package
                .reference
                .selector()
                .cmp(right.package.reference.selector())
        })
        .then_with(|| {
            right
                .explicit_options
                .len()
                .cmp(&left.explicit_options.len())
        })
        .then_with(|| left.explicit_options.cmp(&right.explicit_options))
        .then_with(|| left.package.identity.cmp(&right.package.identity))
}

fn find_cycle(
    nodes: &BTreeMap<FeatureInstallIdentity, Node>,
) -> Option<Vec<FeatureInstallIdentity>> {
    fn visit(
        identity: &FeatureInstallIdentity,
        nodes: &BTreeMap<FeatureInstallIdentity, Node>,
        visiting: &mut Vec<FeatureInstallIdentity>,
        done: &mut BTreeSet<FeatureInstallIdentity>,
    ) -> Option<Vec<FeatureInstallIdentity>> {
        if let Some(index) = visiting.iter().position(|item| item == identity) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(identity.clone());
            return Some(cycle);
        }
        if done.contains(identity) {
            return None;
        }
        visiting.push(identity.clone());
        for dependency in nodes[identity]
            .dependencies
            .iter()
            .filter(|dependency| nodes.contains_key(*dependency))
        {
            if let Some(cycle) = visit(dependency, nodes, visiting, done) {
                return Some(cycle);
            }
        }
        visiting.pop();
        done.insert(identity.clone());
        None
    }
    let mut done = BTreeSet::new();
    for identity in nodes.keys() {
        if let Some(cycle) = visit(identity, nodes, &mut Vec::new(), &mut done) {
            return Some(cycle);
        }
    }
    None
}

fn identity_label(identity: &FeatureInstallIdentity) -> String {
    match identity {
        FeatureInstallIdentity::OciDigest(value)
        | FeatureInstallIdentity::HttpsIntegrity(value)
        | FeatureInstallIdentity::Local(value) => value.clone(),
    }
}
fn differing_option(
    first: &BTreeMap<String, FeatureValue>,
    second: &BTreeMap<String, FeatureValue>,
) -> String {
    first
        .keys()
        .chain(second.keys())
        .find(|key| first.get(*key) != second.get(*key))
        .cloned()
        .unwrap_or_else(|| "<options>".to_owned())
}
fn render_path(path: &[FeatureReference]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ")
}

/// Lockfile domain value. Parsing and writing belong to later adapters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FeatureLock {
    /// Non-local Feature records keyed by normalized request reference.
    pub features: BTreeMap<String, LockedFeature>,
}

/// One exact non-local Feature lock record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LockedFeature {
    /// Exact metadata version.
    pub version: String,
    /// Digest-qualified OCI reference or original HTTPS URL.
    pub resolved: String,
    /// SHA-256 of the downloaded Feature archive.
    pub integrity: String,
    /// Normalized hard dependency references.
    pub depends_on: Vec<String>,
}

impl LockedFeature {
    /// Validates and creates a lock record without reading or writing a lockfile.
    ///
    /// # Errors
    ///
    /// Returns [`FeatureError::InvalidLock`] when the version or integrity is invalid.
    pub fn new(
        version: String,
        resolved: String,
        integrity: String,
        depends_on: Vec<String>,
    ) -> Result<Self, FeatureError> {
        if version.is_empty() {
            return Err(FeatureError::InvalidLock {
                message: "lock version cannot be empty",
            });
        }
        validate_sha256(&integrity).map_err(|message| FeatureError::InvalidLock { message })?;
        Ok(Self {
            version,
            resolved,
            integrity,
            depends_on,
        })
    }
}

/// Pure Feature model or dependency resolution failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FeatureError {
    /// The reference is unsupported at the model boundary.
    #[error("unsupported Feature reference `{reference}`: {reason}")]
    UnsupportedReference {
        /// Rejected input.
        reference: String,
        /// Rejection reason.
        reason: String,
    },
    /// Feature metadata is invalid.
    #[error("invalid Feature metadata at {path}: {message}")]
    InvalidMetadata {
        /// JSON property path.
        path: String,
        /// Validation detail.
        message: String,
    },
    /// A content identity is malformed.
    #[error("invalid Feature identity `{value}`: {message}")]
    InvalidIdentity {
        /// Rejected identity.
        value: String,
        /// Validation detail.
        message: &'static str,
    },
    /// A package required by the graph was not injected.
    #[error("Feature package `{reference}` is missing while resolving {path}")]
    MissingPackage {
        /// Missing reference.
        reference: FeatureReference,
        /// Dependency path which requested it.
        path: String,
    },
    /// The package catalog key and package reference disagree.
    #[error("Feature catalog key `{key}` contains package `{package}`")]
    CatalogMismatch {
        /// Catalog lookup key.
        key: FeatureReference,
        /// Reference declared by the package.
        package: FeatureReference,
    },
    /// An unknown option was supplied.
    #[error("Feature `{feature}` has no option `{option}` while resolving {path}")]
    UnknownOption {
        /// Feature identifier.
        feature: String,
        /// Unknown option name.
        option: String,
        /// Dependency path.
        path: String,
    },
    /// An option has the wrong type or is outside its enum.
    #[error(
        "Feature `{feature}` option `{option}` has invalid value `{value}` while resolving {path}"
    )]
    InvalidOption {
        /// Feature identifier.
        feature: String,
        /// Invalid option name.
        option: String,
        /// Rejected value.
        value: FeatureValue,
        /// Dependency path.
        path: String,
    },
    /// Equal content was requested with conflicting effective options.
    #[error(
        "conflicting option `{option}` for {identity:?}: first requested by {first_path}; conflicting request at {second_path}"
    )]
    ConflictingOptions {
        /// Shared install identity.
        identity: FeatureInstallIdentity,
        /// First differing option.
        option: String,
        /// Path of the first request.
        first_path: String,
        /// Path of the conflicting request.
        second_path: String,
    },
    /// An override is malformed or does not identify an installed Feature.
    #[error("invalid overrideFeatureInstallOrder entry `{reference}`: {message}")]
    InvalidOverride {
        /// Rejected override entry.
        reference: String,
        /// Validation detail.
        message: &'static str,
    },
    /// Hard or retained soft edges contain a cycle.
    #[error(
        "Feature dependency cycle: {cycle}; remove a dependsOn/installsAfter edge or change the requested Features"
    )]
    DependencyCycle {
        /// Closed identity path demonstrating the cycle.
        cycle: String,
    },
    /// A lock domain value is malformed.
    #[error("invalid Feature lock value: {message}")]
    InvalidLock {
        /// Validation detail.
        message: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reference(id: &str) -> FeatureReference {
        FeatureReference::parse(&format!("./features/{id}")).expect("valid local reference")
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "keeps JSON fixture calls concise"
    )]
    fn package(id: &str, metadata: Value) -> FeaturePackage {
        let reference = reference(id);
        FeaturePackage {
            identity: FeatureInstallIdentity::local(&reference),
            reference,
            metadata: FeatureMetadata::from_value(&metadata).expect("valid fixture metadata"),
            digest: None,
            integrity: None,
        }
    }

    fn catalog(packages: Vec<FeaturePackage>) -> BTreeMap<FeatureReference, FeaturePackage> {
        packages
            .into_iter()
            .map(|package| (package.reference.clone(), package))
            .collect()
    }

    fn ids(result: &ResolvedFeatures) -> Vec<&str> {
        result
            .installation_order
            .iter()
            .map(|feature| feature.metadata.id.as_str())
            .collect()
    }

    #[test]
    fn references_normalize_and_reject_unsupported_transport_forms() {
        assert_eq!(
            FeatureReference::parse("GHCR.IO/devcontainers/features/git:1")
                .expect("public OCI reference")
                .as_str(),
            "ghcr.io/devcontainers/features/git:1"
        );
        assert_eq!(
            FeatureReference::parse("./features//git/./")
                .expect("contained local reference")
                .as_str(),
            "./features/git"
        );
        for invalid in [
            "http://example.com/feature.tgz",
            "https://user:secret@example.com/feature.tgz",
            "https://localhost/feature.tgz",
            "./features/../secret",
            "git",
        ] {
            assert!(matches!(
                FeatureReference::parse(invalid),
                Err(FeatureError::UnsupportedReference { .. })
            ));
        }
    }

    #[test]
    fn metadata_options_apply_defaults_and_validate_enums() {
        let package = package(
            "tool",
            json!({
                "id": "tool", "version": "1.0.0",
                "options": {
                    "enabled": {"type": "boolean", "default": true},
                    "channel": {"type": "string", "default": "stable", "enum": ["stable", "beta"]}
                }
            }),
        );
        let packages = catalog(vec![package]);
        let result = resolve_features(&[FeatureRequest::new(reference("tool"))], &packages, &[])
            .expect("defaults resolve");
        assert_eq!(
            result.installation_order[0].options,
            BTreeMap::from([
                (
                    "channel".to_owned(),
                    FeatureValue::String("stable".to_owned())
                ),
                ("enabled".to_owned(), FeatureValue::Boolean(true)),
            ])
        );

        let request = FeatureRequest {
            reference: reference("tool"),
            options: BTreeMap::from([(
                "channel".to_owned(),
                FeatureValue::String("nightly".to_owned()),
            )]),
        };
        assert!(matches!(
            resolve_features(&[request], &packages, &[]),
            Err(FeatureError::InvalidOption { option, .. }) if option == "channel"
        ));
    }

    #[test]
    fn recursive_equal_requests_merge_and_conflicts_include_both_paths() {
        let base = package(
            "base",
            json!({
                "id": "base", "version": "1.0.0",
                "options": {"mode": {"type": "string", "default": "same"}}
            }),
        );
        let left = package(
            "left",
            json!({"id": "left", "version": "1", "dependsOn": {"./features/base": {"mode": "same"}}}),
        );
        let right = package(
            "right",
            json!({"id": "right", "version": "1", "dependsOn": {"./features/base": {"mode": "same"}}}),
        );
        let packages = catalog(vec![base, left, right]);
        let requests = [
            FeatureRequest::new(reference("right")),
            FeatureRequest::new(reference("left")),
        ];
        let result = resolve_features(&requests, &packages, &[]).expect("equal requests merge");
        assert_eq!(ids(&result), vec!["base", "left", "right"]);

        let mut conflicting = packages;
        conflicting
            .get_mut(&reference("right"))
            .expect("fixture package")
            .metadata
            .depends_on
            .get_mut(&reference("base"))
            .expect("fixture dependency")
            .insert(
                "mode".to_owned(),
                FeatureValue::String("different".to_owned()),
            );
        let error = resolve_features(&requests, &conflicting, &[]).expect_err("conflict fails");
        assert!(matches!(
            error,
            FeatureError::ConflictingOptions { option, first_path, second_path, .. }
                if option == "mode" && first_path.contains("base") && second_path.contains("base")
        ));
    }

    #[test]
    fn soft_hints_overrides_and_disconnected_nodes_use_deterministic_rounds() {
        let base = package("base", json!({"id": "base", "version": "1"}));
        let after = package(
            "after",
            json!({"id": "after", "version": "1", "installsAfter": ["./features/base"]}),
        );
        let top = package(
            "top",
            json!({"id": "top", "version": "1", "dependsOn": {"./features/base": {}}}),
        );
        let free = package("free", json!({"id": "free", "version": "1"}));
        let packages = catalog(vec![top, free, after, base]);
        let requests = [
            FeatureRequest::new(reference("top")),
            FeatureRequest::new(reference("free")),
            FeatureRequest::new(reference("after")),
        ];
        let result = resolve_features(
            &requests,
            &packages,
            &["./features/free".to_owned(), "./features/top".to_owned()],
        )
        .expect("graph resolves");
        assert_eq!(ids(&result), vec!["free", "base", "top", "after"]);
    }

    #[test]
    fn order_is_independent_of_request_and_catalog_insertion_order() {
        let packages = vec![
            package(
                "c",
                json!({"id": "c", "version": "1", "dependsOn": {"./features/a": {}}}),
            ),
            package("a", json!({"id": "a", "version": "1"})),
            package("b", json!({"id": "b", "version": "1"})),
        ];
        let first_catalog = catalog(packages.clone());
        let second_catalog = catalog(packages.into_iter().rev().collect());
        let first = resolve_features(
            &[
                FeatureRequest::new(reference("c")),
                FeatureRequest::new(reference("b")),
            ],
            &first_catalog,
            &[],
        )
        .expect("first order resolves");
        let second = resolve_features(
            &[
                FeatureRequest::new(reference("b")),
                FeatureRequest::new(reference("c")),
            ],
            &second_catalog,
            &[],
        )
        .expect("second order resolves");
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn cycle_error_is_actionable_and_each_contribution_occurs_once() {
        let a = package(
            "a",
            json!({"id": "a", "version": "1", "dependsOn": {"./features/b": {}}, "containerEnv": {"A": "a"}}),
        );
        let b = package(
            "b",
            json!({"id": "b", "version": "1", "dependsOn": {"./features/a": {}}, "containerEnv": {"B": "b"}}),
        );
        let packages = catalog(vec![a, b]);
        let error = resolve_features(&[FeatureRequest::new(reference("a"))], &packages, &[])
            .expect_err("cycle fails");
        assert!(
            matches!(error, FeatureError::DependencyCycle { cycle } if cycle.contains('a') && cycle.contains('b'))
        );

        let leaf = package(
            "leaf",
            json!({"id": "leaf", "version": "1", "containerEnv": {"ONLY": "once"}}),
        );
        let one = package(
            "one",
            json!({"id": "one", "version": "1", "dependsOn": {"./features/leaf": {}}}),
        );
        let two = package(
            "two",
            json!({"id": "two", "version": "1", "dependsOn": {"./features/leaf": {}}}),
        );
        let packages = catalog(vec![leaf, one, two]);
        let result = resolve_features(
            &[
                FeatureRequest::new(reference("one")),
                FeatureRequest::new(reference("two")),
            ],
            &packages,
            &[],
        )
        .expect("diamond resolves");
        assert_eq!(
            result
                .installation_order
                .iter()
                .filter(|feature| feature
                    .metadata
                    .contributions
                    .container_env
                    .contains_key("ONLY"))
                .count(),
            1
        );
    }
}
