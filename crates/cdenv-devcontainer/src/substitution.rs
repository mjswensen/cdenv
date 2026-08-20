//! Property-aware staged Dev Container variable substitution.

use std::collections::BTreeMap;

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// Stable identity labels used to derive `${devcontainerId}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StableIdentityLabels(BTreeMap<String, String>);

impl StableIdentityLabels {
    /// Selects the stable installation/workspace labels. Generation labels are deliberately absent.
    #[must_use]
    pub fn new(installation_id: impl Into<String>, workspace_name: impl Into<String>) -> Self {
        Self(BTreeMap::from([
            ("cdenv.installation".to_owned(), installation_id.into()),
            ("cdenv.workspace".to_owned(), workspace_name.into()),
        ]))
    }

    /// Computes the pinned canonical sorted-label SHA-256 big-integer base-32 identifier.
    #[must_use]
    pub fn devcontainer_id(&self) -> String {
        let canonical = serde_json::to_vec(&self.0).unwrap_or_default();
        base32_big_integer(&Sha256::digest(canonical))
    }
}

/// A property whose string values support pinned substitutions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SubstitutionProperty {
    /// Display name.
    Name,
    /// Docker build argument. Runtime/container and dev-container ID variables are forbidden.
    BuildArgument,
    /// Docker run argument.
    RunArgument,
    /// Workspace folder.
    WorkspaceFolder,
    /// Workspace mount.
    WorkspaceMount,
    /// Additional mount string/source/target.
    Mount,
    /// Container environment value.
    ContainerEnv,
    /// Remote environment value.
    RemoteEnv,
    /// Container user.
    ContainerUser,
    /// Remote user.
    RemoteUser,
    /// Lifecycle command string or argument.
    LifecycleCommand,
    /// Tool-owned customization string.
    Customization,
    /// Feature metadata entrypoint.
    FeatureEntrypoint,
}

impl SubstitutionProperty {
    const fn allows_devcontainer_id(self) -> bool {
        !matches!(self, Self::BuildArgument)
    }

    const fn allows_container_env(self) -> bool {
        matches!(self, Self::RemoteEnv | Self::LifecycleCommand)
    }
}

/// Injected values available during host/workspace planning.
pub struct HostSubstitutionInputs<'a> {
    /// Canonical local workspace path represented as UTF-8 by the host adapter.
    pub local_workspace_folder: &'a str,
    /// Effective container workspace folder after scenario defaulting.
    pub container_workspace_folder: &'a str,
    /// Explicit host environment snapshot. This crate never reads process globals.
    pub local_env: &'a BTreeMap<String, String>,
    /// Stable identity labels, excluding generation identity.
    pub identity_labels: &'a StableIdentityLabels,
}

/// A host-resolved value that may still require actual container environment data.
#[derive(Clone, PartialEq, Eq)]
pub struct DeferredString {
    segments: Vec<Segment>,
}

#[derive(Clone, PartialEq, Eq)]
enum Segment {
    Literal(String),
    ContainerEnv { name: String, default: String },
}

impl std::fmt::Debug for DeferredString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeferredString")
            .field(
                "requires_container_environment",
                &self.requires_container_environment(),
            )
            .field("value", &"<redacted>")
            .finish()
    }
}

impl DeferredString {
    /// Reports whether runtime reconciliation must inject actual container data.
    #[must_use]
    pub fn requires_container_environment(&self) -> bool {
        self.segments
            .iter()
            .any(|segment| matches!(segment, Segment::ContainerEnv { .. }))
    }

    /// Resolves deferred expressions from an explicitly captured active-container environment.
    #[must_use]
    pub fn resolve(&self, container_env: &BTreeMap<String, String>) -> ResolvedString {
        let mut value = String::new();
        for segment in &self.segments {
            match segment {
                Segment::Literal(text) => value.push_str(text),
                Segment::ContainerEnv { name, default } => {
                    value.push_str(container_env.get(name).map_or(default, String::as_str));
                }
            }
        }
        ResolvedString(value)
    }

    /// Produces a safe persisted summary containing no resolved values.
    #[must_use]
    pub fn summary(&self, property: SubstitutionProperty) -> SubstitutionSummary {
        SubstitutionSummary {
            property,
            requires_container_environment: self.requires_container_environment(),
        }
    }

    pub(crate) fn fingerprint_value(&self) -> serde_json::Value {
        serde_json::Value::Array(
            self.segments
                .iter()
                .map(|segment| match segment {
                    Segment::Literal(value) => serde_json::json!({"literal": value}),
                    Segment::ContainerEnv { name, default } => {
                        serde_json::json!({"containerEnv": name, "default": default})
                    }
                })
                .collect(),
        )
    }
}

/// A fully resolved sensitive string. Debug output is always redacted and the type is not serializable.
#[derive(Clone, PartialEq, Eq)]
pub struct ResolvedString(String);

impl ResolvedString {
    /// Borrows the value for immediate plan execution.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for ResolvedString {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ResolvedString(<redacted>)")
    }
}

/// Serializable persisted summary that records stage needs but never values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubstitutionSummary {
    /// Property class.
    pub property: SubstitutionProperty,
    /// Whether actual container data is still needed.
    pub requires_container_environment: bool,
}

/// Performs only host/workspace-stage substitutions for one allowed property.
///
/// `$${...}` escapes an expression as literal `${...}`. Missing environment variables use their
/// optional default or the empty string. Container environment expressions remain typed/deferred.
///
/// # Errors
///
/// Returns an error naming only the property/expression, never input or substituted values.
pub fn substitute_host(
    property: SubstitutionProperty,
    input: &str,
    context: &HostSubstitutionInputs<'_>,
) -> Result<DeferredString, SubstitutionError> {
    let local_basename = basename(context.local_workspace_folder);
    let container_basename = basename(context.container_workspace_folder);
    let devcontainer_id = context.identity_labels.devcontainer_id();
    let mut segments = Vec::new();
    let mut literal = String::new();
    let mut index = 0;
    while index < input.len() {
        let remainder = &input[index..];
        if remainder.starts_with("$${") {
            literal.push_str("${");
            index += 3;
            continue;
        }
        if !remainder.starts_with("${") {
            let character = remainder
                .chars()
                .next()
                .ok_or(SubstitutionError::Malformed { property })?;
            literal.push(character);
            index += character.len_utf8();
            continue;
        }
        let expression_start = index + 2;
        let Some(relative_end) = input[expression_start..].find('}') else {
            return Err(SubstitutionError::Malformed { property });
        };
        let expression_end = expression_start + relative_end;
        let expression = &input[expression_start..expression_end];
        let replacement = match expression {
            "localWorkspaceFolder" => context.local_workspace_folder,
            "localWorkspaceFolderBasename" => local_basename,
            "containerWorkspaceFolder" => context.container_workspace_folder,
            "containerWorkspaceFolderBasename" => container_basename,
            "devcontainerId" if property.allows_devcontainer_id() => &devcontainer_id,
            "devcontainerId" => {
                return Err(SubstitutionError::Disallowed {
                    property,
                    variable: "devcontainerId".to_owned(),
                });
            }
            value if value.starts_with("localEnv:") => {
                let (name, default) = environment_expression(value, "localEnv:", property)?;
                context.local_env.get(name).map_or(default, String::as_str)
            }
            value if value.starts_with("containerEnv:") && property.allows_container_env() => {
                let (name, default) = environment_expression(value, "containerEnv:", property)?;
                if !literal.is_empty() {
                    segments.push(Segment::Literal(std::mem::take(&mut literal)));
                }
                segments.push(Segment::ContainerEnv {
                    name: name.to_owned(),
                    default: default.to_owned(),
                });
                index = expression_end + 1;
                continue;
            }
            value if value.starts_with("containerEnv:") => {
                return Err(SubstitutionError::Disallowed {
                    property,
                    variable: "containerEnv".to_owned(),
                });
            }
            _ => {
                return Err(SubstitutionError::Unsupported {
                    property,
                    variable: expression_name(expression),
                });
            }
        };
        literal.push_str(replacement);
        index = expression_end + 1;
    }
    if !literal.is_empty() || segments.is_empty() {
        segments.push(Segment::Literal(literal));
    }
    Ok(DeferredString { segments })
}

fn environment_expression<'a>(
    expression: &'a str,
    prefix: &str,
    property: SubstitutionProperty,
) -> Result<(&'a str, &'a str), SubstitutionError> {
    let mut fields = expression[prefix.len()..].splitn(2, ':');
    let name = fields.next().unwrap_or_default();
    if name.is_empty() {
        return Err(SubstitutionError::Malformed { property });
    }
    Ok((name, fields.next().unwrap_or_default()))
}

fn expression_name(expression: &str) -> String {
    expression.split(':').next().unwrap_or("unknown").to_owned()
}

fn basename(path: &str) -> &str {
    path.trim_end_matches('/').rsplit('/').next().unwrap_or("")
}

fn base32_big_integer(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 32] = b"0123456789abcdefghijklmnopqrstuv";
    let mut number = bytes.to_vec();
    let mut output = Vec::with_capacity(52);
    while number.iter().any(|byte| *byte != 0) {
        let mut carry = 0_u16;
        for byte in &mut number {
            let value = (carry << 8) | u16::from(*byte);
            *byte = u8::try_from(value / 32).unwrap_or_default();
            carry = value % 32;
        }
        output.push(DIGITS[usize::from(carry)]);
    }
    while output.len() < 52 {
        output.push(b'0');
    }
    output.reverse();
    String::from_utf8(output).unwrap_or_default()
}

/// A substitution failure safe for diagnostics.
#[expect(
    missing_docs,
    reason = "variant fields repeat the documented property/variable error contract"
)]
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum SubstitutionError {
    /// Syntax was unterminated or omitted an environment name.
    #[error("malformed substitution in {property:?}")]
    Malformed { property: SubstitutionProperty },
    /// A known variable is not supported by this property/stage.
    #[error("substitution `{variable}` is not allowed in {property:?}")]
    Disallowed {
        property: SubstitutionProperty,
        variable: String,
    },
    /// The pinned profile does not recognize this variable.
    #[error("unsupported substitution `{variable}` in {property:?}")]
    Unsupported {
        property: SubstitutionProperty,
        variable: String,
    },
}
