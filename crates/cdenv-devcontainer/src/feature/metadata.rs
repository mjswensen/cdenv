//! Strict metadata parsing and effective option validation, independent of graph traversal.

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{FeatureError, FeatureReference, render_path};
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

/// A value supplied to a Feature option.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(untagged)]
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

pub(super) fn validate_options(
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
