use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Component, Path};

use jsonc_parser::{ParseOptions, parse_to_serde_value};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Result, SpikeError, read};

const MAX_CONFIG_BYTES: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_ARRAY_ITEMS: usize = 4096;
const MAX_OBJECT_ITEMS: usize = 4096;
const MAX_STRING_BYTES: usize = 256 * 1024;

const SUPPORTED_TOP_LEVEL: &[&str] = &[
    "$schema",
    "name",
    "image",
    "build",
    "dockerComposeFile",
    "service",
    "runServices",
    "workspaceFolder",
    "workspaceMount",
    "features",
    "overrideFeatureInstallOrder",
    "containerEnv",
    "remoteEnv",
    "containerUser",
    "remoteUser",
    "updateRemoteUserUID",
    "userEnvProbe",
    "overrideCommand",
    "init",
    "privileged",
    "capAdd",
    "securityOpt",
    "mounts",
    "runArgs",
    "appPort",
    "forwardPorts",
    "portsAttributes",
    "otherPortsAttributes",
    "initializeCommand",
    "onCreateCommand",
    "updateContentCommand",
    "postCreateCommand",
    "postStartCommand",
    "postAttachCommand",
    "waitFor",
    "shutdownAction",
    "hostRequirements",
    "customizations",
    "secrets",
];

const RESERVED_BUILD_OPTIONS: &[&str] = &[
    "--file",
    "-f",
    "--tag",
    "-t",
    "--output",
    "-o",
    "--iidfile",
    "--metadata-file",
];

#[derive(Debug, Clone, Serialize)]
pub(crate) struct EffectivePlan {
    pub(crate) profile: &'static str,
    pub(crate) scenario: &'static str,
    #[serde(rename = "devcontainerId")]
    pub(crate) devcontainer_id: String,
    #[serde(rename = "workspaceFolder")]
    pub(crate) workspace_folder: String,
    #[serde(rename = "hostStage")]
    pub(crate) host_stage: Value,
    #[serde(rename = "runtimeStage")]
    pub(crate) runtime_stage: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct SubstitutionContext<'a> {
    pub(crate) local_workspace: &'a Path,
    pub(crate) container_workspace: &'a str,
    pub(crate) local_env: &'a BTreeMap<String, String>,
    pub(crate) container_env: Option<&'a BTreeMap<String, String>>,
    pub(crate) devcontainer_id: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
struct RawFeatureMetadata {
    id: String,
    version: String,
    #[serde(default, rename = "dependsOn")]
    depends_on: BTreeMap<String, Value>,
    #[serde(default, rename = "installsAfter")]
    installs_after: Vec<String>,
    #[serde(default)]
    options: BTreeMap<String, FeatureOption>,
}

#[derive(Debug, Clone, Deserialize)]
struct FeatureOption {
    #[serde(rename = "type")]
    kind: String,
    default: Option<Value>,
    #[serde(default)]
    proposals: Vec<String>,
    #[serde(default)]
    enum_values: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct OrderedFeature {
    pub(crate) id: String,
    pub(crate) source: String,
    pub(crate) version: String,
    pub(crate) options: BTreeMap<String, Value>,
}

#[derive(Debug, Clone)]
struct FeatureNode {
    metadata: RawFeatureMetadata,
    source: String,
    options: BTreeMap<String, Value>,
    dependencies: BTreeSet<String>,
    priority: usize,
}

pub(crate) fn parse_and_validate(path: &Path, schema_path: &Path) -> Result<Value> {
    let bytes = read(path)?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(SpikeError::ConfigSize {
            path: path.to_path_buf(),
            limit: MAX_CONFIG_BYTES,
        });
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| SpikeError::Jsonc {
        path: path.to_path_buf(),
        detail: format!("input is not UTF-8: {error}"),
    })?;
    check_lexical_depth(path, text)?;

    let options = ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
    };
    let value =
        parse_to_serde_value::<Value>(text, &options).map_err(|error| SpikeError::Jsonc {
            path: path.to_path_buf(),
            detail: error.to_string(),
        })?;
    check_value_bounds(path, &value, "$", 0)?;

    let schema: Value = serde_json::from_slice(&read(schema_path)?).map_err(|error| {
        SpikeError::SchemaCompile(format!("{}: {error}", schema_path.display()))
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| SpikeError::SchemaCompile(error.to_string()))?;
    let diagnostics = validator
        .iter_errors(&value)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    if !diagnostics.is_empty() {
        return Err(SpikeError::Schema {
            path: path.to_path_buf(),
            diagnostics: diagnostics.join("\n"),
        });
    }

    validate_profile(path, &value)?;
    Ok(value)
}

fn check_lexical_depth(path: &Path, text: &str) -> Result<()> {
    #[derive(Clone, Copy)]
    enum State {
        Normal,
        String,
        LineComment,
        BlockComment,
    }

    let bytes = text.as_bytes();
    let mut state = State::Normal;
    let mut escaped = false;
    let mut depth = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        let current = bytes[index];
        let next = bytes.get(index + 1).copied();
        match state {
            State::Normal if current == b'"' => state = State::String,
            State::Normal if current == b'/' && next == Some(b'/') => {
                state = State::LineComment;
                index += 1;
            }
            State::Normal if current == b'/' && next == Some(b'*') => {
                state = State::BlockComment;
                index += 1;
            }
            State::Normal if current == b'{' || current == b'[' => {
                depth += 1;
                if depth > MAX_DEPTH {
                    return Err(SpikeError::ConfigDepth {
                        path: path.to_path_buf(),
                        limit: MAX_DEPTH,
                    });
                }
            }
            State::Normal if current == b'}' || current == b']' => {
                depth = depth.saturating_sub(1);
            }
            State::String if escaped => escaped = false,
            State::String if current == b'\\' => escaped = true,
            State::String if current == b'"' => state = State::Normal,
            State::LineComment if current == b'\n' => state = State::Normal,
            State::BlockComment if current == b'*' && next == Some(b'/') => {
                state = State::Normal;
                index += 1;
            }
            _ => {}
        }
        index += 1;
    }
    Ok(())
}

fn check_value_bounds(path: &Path, value: &Value, location: &str, depth: usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(SpikeError::ConfigDepth {
            path: path.to_path_buf(),
            limit: MAX_DEPTH,
        });
    }
    match value {
        Value::String(string) if string.len() > MAX_STRING_BYTES => Err(SpikeError::ConfigBound {
            path: path.to_path_buf(),
            location: location.to_string(),
            detail: format!("string exceeds {MAX_STRING_BYTES} bytes"),
        }),
        Value::Array(values) if values.len() > MAX_ARRAY_ITEMS => Err(SpikeError::ConfigBound {
            path: path.to_path_buf(),
            location: location.to_string(),
            detail: format!("array exceeds {MAX_ARRAY_ITEMS} items"),
        }),
        Value::Object(values) if values.len() > MAX_OBJECT_ITEMS => Err(SpikeError::ConfigBound {
            path: path.to_path_buf(),
            location: location.to_string(),
            detail: format!("object exceeds {MAX_OBJECT_ITEMS} entries"),
        }),
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                check_value_bounds(path, child, &format!("{location}[{index}]"), depth + 1)?;
            }
            Ok(())
        }
        Value::Object(values) => {
            for (key, child) in values {
                if key.len() > MAX_STRING_BYTES {
                    return Err(SpikeError::ConfigBound {
                        path: path.to_path_buf(),
                        location: location.to_string(),
                        detail: format!("object key exceeds {MAX_STRING_BYTES} bytes"),
                    });
                }
                check_value_bounds(path, child, &format!("{location}.{key}"), depth + 1)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn validate_profile(path: &Path, value: &Value) -> Result<()> {
    let object = value.as_object().ok_or_else(|| SpikeError::Profile {
        path: "$".to_string(),
        detail: "configuration must be an object".to_string(),
    })?;
    for key in object.keys() {
        if !SUPPORTED_TOP_LEVEL.contains(&key.as_str()) {
            return Err(SpikeError::Profile {
                path: format!("$.{key}"),
                detail: "unknown behavioral property for profile cdenv-devcontainer-v1 at specification c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421".to_string(),
            });
        }
    }

    let scenarios = [
        object.contains_key("image"),
        object.contains_key("build"),
        object.contains_key("dockerComposeFile"),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if scenarios != 1 {
        return Err(SpikeError::Profile {
            path: path.display().to_string(),
            detail: "exactly one of `image`, `build`, or `dockerComposeFile` is required"
                .to_string(),
        });
    }
    if object.contains_key("dockerComposeFile") && !object.contains_key("service") {
        return Err(SpikeError::Profile {
            path: "$.service".to_string(),
            detail: "Compose scenarios require a primary `service`".to_string(),
        });
    }
    if let Some(options) = object
        .get("build")
        .and_then(Value::as_object)
        .and_then(|build| build.get("options"))
        .and_then(Value::as_array)
    {
        validate_build_options(options)?;
    }
    if let Some(features) = object.get("features").and_then(Value::as_object) {
        for source in features.keys() {
            validate_feature_source(source)?;
        }
    }
    Ok(())
}

pub(crate) fn validate_build_options(options: &[Value]) -> Result<()> {
    let mut index = 0_usize;
    while index < options.len() {
        let argument = options[index].as_str().ok_or_else(|| SpikeError::Profile {
            path: format!("$.build.options[{index}]"),
            detail: "Docker build options must be strings".to_string(),
        })?;
        let name = argument.split('=').next().unwrap_or(argument);
        if RESERVED_BUILD_OPTIONS.contains(&name)
            || name == "--label" && argument.contains("cdenv.")
            || name.starts_with("--label=cdenv.")
        {
            return Err(SpikeError::Profile {
                path: format!("$.build.options[{index}]"),
                detail: format!(
                    "reserved Docker build option `{argument}` conflicts with cdenv-owned output"
                ),
            });
        }
        if name == "--label" {
            let next = options.get(index + 1).and_then(Value::as_str).unwrap_or("");
            if next.starts_with("cdenv.") {
                return Err(SpikeError::Profile {
                    path: format!("$.build.options[{index}]"),
                    detail: format!(
                        "reserved Docker build option `--label {next}` conflicts with cdenv identity"
                    ),
                });
            }
            index += 1;
        }
        index += 1;
    }
    Ok(())
}

pub(crate) fn validate_feature_source(source: &str) -> Result<()> {
    if source.starts_with("./") {
        let path = Path::new(source);
        if path.is_absolute()
            || path
                .components()
                .any(|component| matches!(component, Component::ParentDir | Component::RootDir))
        {
            return Err(SpikeError::Feature(format!(
                "local Feature `{source}` escapes the .devcontainer directory"
            )));
        }
        return Ok(());
    }
    if source.starts_with("https://") {
        if source.contains('@')
            && source.split("//").nth(1).is_some_and(|rest| {
                rest.split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            })
        {
            return Err(SpikeError::Feature(format!(
                "credentials in Feature URL `{source}` are forbidden"
            )));
        }
        return Ok(());
    }
    if source.starts_with("http://") {
        return Err(SpikeError::Feature(format!(
            "insecure HTTP Feature source `{source}` is unsupported"
        )));
    }
    if source.contains('/') && !source.contains("://") {
        return Ok(());
    }
    Err(SpikeError::Feature(format!(
        "unsupported Feature source `{source}`; expected public OCI, HTTPS, or contained local path"
    )))
}

pub(crate) fn scenario(value: &Value) -> &'static str {
    if value.get("image").is_some() {
        "image"
    } else if value.get("build").is_some() {
        "dockerfile"
    } else {
        "compose"
    }
}

pub(crate) fn effective_plan(
    raw: &Value,
    local_workspace: &Path,
    container_env: &BTreeMap<String, String>,
) -> Result<EffectivePlan> {
    let labels = BTreeMap::from([
        (
            "cdenv.installation".to_string(),
            "spike-installation".to_string(),
        ),
        (
            "cdenv.workspace".to_string(),
            local_workspace
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("workspace")
                .to_string(),
        ),
    ]);
    let devcontainer_id = devcontainer_id(&labels);
    let local_env = BTreeMap::from([
        ("CDENV_SPIKE_SUFFIX".to_string(), "fallback".to_string()),
        (
            "CDENV_SPIKE_BUILD_ARG".to_string(),
            "bounded-default".to_string(),
        ),
    ]);
    let container_workspace = format!(
        "/workspaces/{}",
        local_workspace
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("workspace")
    );
    let host_context = SubstitutionContext {
        local_workspace,
        container_workspace: &container_workspace,
        local_env: &local_env,
        container_env: None,
        devcontainer_id: &devcontainer_id,
    };
    let host_stage = substitute_value(raw, &host_context)?;
    let runtime_context = SubstitutionContext {
        container_env: Some(container_env),
        ..host_context
    };
    let runtime_stage = substitute_value(&host_stage, &runtime_context)?;
    let workspace_folder = runtime_stage
        .get("workspaceFolder")
        .and_then(Value::as_str)
        .unwrap_or(&container_workspace)
        .to_string();

    Ok(EffectivePlan {
        profile: "cdenv-devcontainer-v1",
        scenario: scenario(raw),
        devcontainer_id,
        workspace_folder,
        host_stage,
        runtime_stage,
    })
}

fn substitute_value(value: &Value, context: &SubstitutionContext<'_>) -> Result<Value> {
    match value {
        Value::String(string) => Ok(Value::String(substitute_string(string, context)?)),
        Value::Array(values) => values
            .iter()
            .map(|value| substitute_value(value, context))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(values) => values
            .iter()
            .map(|(key, value)| Ok((key.clone(), substitute_value(value, context)?)))
            .collect::<Result<Map<_, _>>>()
            .map(Value::Object),
        _ => Ok(value.clone()),
    }
}

fn substitute_string(input: &str, context: &SubstitutionContext<'_>) -> Result<String> {
    let mut result = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(start) = rest.find("${") {
        result.push_str(&rest[..start]);
        let expression = &rest[start + 2..];
        let end = expression.find('}').ok_or_else(|| SpikeError::Profile {
            path: "$".to_string(),
            detail: format!("unterminated substitution in `{input}`"),
        })?;
        let variable = &expression[..end];
        let replacement = match variable {
            "localWorkspaceFolder" => context.local_workspace.to_string_lossy().to_string(),
            "localWorkspaceFolderBasename" => context
                .local_workspace
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("")
                .to_string(),
            "containerWorkspaceFolder" => context.container_workspace.to_string(),
            "containerWorkspaceFolderBasename" => Path::new(context.container_workspace)
                .file_name()
                .and_then(std::ffi::OsStr::to_str)
                .unwrap_or("")
                .to_string(),
            "devcontainerId" => context.devcontainer_id.to_string(),
            _ if variable.starts_with("localEnv:") => {
                let mut fields = variable["localEnv:".len()..].splitn(2, ':');
                let name = fields.next().unwrap_or_default();
                let default = fields.next().unwrap_or_default();
                context
                    .local_env
                    .get(name)
                    .map_or_else(|| default.to_string(), Clone::clone)
            }
            _ if variable.starts_with("containerEnv:") => {
                let Some(container_env) = context.container_env else {
                    result.push_str(&rest[start..start + end + 3]);
                    rest = &expression[end + 1..];
                    continue;
                };
                let mut fields = variable["containerEnv:".len()..].splitn(2, ':');
                let name = fields.next().unwrap_or_default();
                let default = fields.next().unwrap_or_default();
                container_env
                    .get(name)
                    .map_or_else(|| default.to_string(), Clone::clone)
            }
            _ => {
                return Err(SpikeError::Profile {
                    path: "$".to_string(),
                    detail: format!("unsupported substitution `${{{variable}}}`"),
                });
            }
        };
        result.push_str(&replacement);
        rest = &expression[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}

pub(crate) fn devcontainer_id(labels: &BTreeMap<String, String>) -> String {
    let input = serde_json::to_vec(labels).unwrap_or_default();
    let digest = Sha256::digest(input);
    base32_big_integer(&digest)
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

pub(crate) fn canonical_json<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let value = serde_json::to_value(value).map_err(|error| SpikeError::Profile {
        path: "$".to_string(),
        detail: format!("could not normalize plan: {error}"),
    })?;
    let sorted = sort_json(value);
    let mut output = serde_json::to_vec_pretty(&sorted).map_err(|error| SpikeError::Profile {
        path: "$".to_string(),
        detail: format!("could not serialize normalized plan: {error}"),
    })?;
    output.push(b'\n');
    Ok(output)
}

fn sort_json(value: Value) -> Value {
    match value {
        Value::Object(values) => Value::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, sort_json(value)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect(),
        ),
        Value::Array(values) => Value::Array(values.into_iter().map(sort_json).collect()),
        other => other,
    }
}

pub(crate) fn merge_metadata(metadata: &[Value], repository: &Value) -> Result<Value> {
    let mut sources = metadata.to_vec();
    sources.push(repository.clone());
    let mut output = Map::new();

    for property in ["init", "privileged"] {
        let merged = sources
            .iter()
            .filter_map(|source| source.get(property).and_then(Value::as_bool))
            .any(|value| value);
        if sources.iter().any(|source| source.get(property).is_some()) {
            output.insert(property.to_string(), Value::Bool(merged));
        }
    }
    for property in ["capAdd", "securityOpt"] {
        let values = stable_union(&sources, property);
        if !values.is_empty() {
            output.insert(property.to_string(), Value::Array(values));
        }
    }
    let entrypoints = sources
        .iter()
        .filter_map(|source| source.get("entrypoint").cloned())
        .collect::<Vec<_>>();
    if !entrypoints.is_empty() {
        output.insert("entrypoints".to_string(), Value::Array(entrypoints));
    }
    let mounts = merge_mounts(&sources)?;
    if !mounts.is_empty() {
        output.insert("mounts".to_string(), Value::Array(mounts));
    }
    for property in [
        "onCreateCommand",
        "updateContentCommand",
        "postCreateCommand",
        "postStartCommand",
        "postAttachCommand",
    ] {
        let commands = sources
            .iter()
            .filter_map(|source| source.get(property).cloned())
            .collect::<Vec<_>>();
        if !commands.is_empty() {
            output.insert(property.to_string(), Value::Array(commands));
        }
    }
    for property in [
        "waitFor",
        "containerUser",
        "remoteUser",
        "userEnvProbe",
        "overrideCommand",
        "otherPortsAttributes",
        "shutdownAction",
        "updateRemoteUserUID",
    ] {
        if let Some(value) = sources
            .iter()
            .rev()
            .find_map(|source| source.get(property).cloned())
        {
            output.insert(property.to_string(), value);
        }
    }
    for property in ["containerEnv", "remoteEnv", "portsAttributes"] {
        let mut merged = Map::new();
        for source in &sources {
            if let Some(values) = source.get(property).and_then(Value::as_object) {
                for (key, value) in values {
                    merged.insert(key.clone(), value.clone());
                }
            }
        }
        if !merged.is_empty() {
            output.insert(property.to_string(), Value::Object(merged));
        }
    }
    let forward_ports = merge_forward_ports(&sources);
    if !forward_ports.is_empty() {
        output.insert("forwardPorts".to_string(), Value::Array(forward_ports));
    }
    if let Some(requirements) = merge_host_requirements(&sources)? {
        output.insert("hostRequirements".to_string(), requirements);
    }
    Ok(Value::Object(output))
}

fn stable_union(sources: &[Value], property: &str) -> Vec<Value> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for value in sources
        .iter()
        .filter_map(|source| source.get(property).and_then(Value::as_array))
        .flatten()
    {
        let key = value.to_string();
        if seen.insert(key) {
            output.push(value.clone());
        }
    }
    output
}

fn merge_mounts(sources: &[Value]) -> Result<Vec<Value>> {
    let mut order = Vec::<String>::new();
    let mut mounts = BTreeMap::<String, Value>::new();
    for mount in sources
        .iter()
        .filter_map(|source| source.get("mounts").and_then(Value::as_array))
        .flatten()
    {
        let target = mount_target(mount)?;
        if !mounts.contains_key(&target) {
            order.push(target.clone());
        }
        mounts.insert(target, mount.clone());
    }
    Ok(order
        .iter()
        .filter_map(|target| mounts.get(target).cloned())
        .collect())
}

fn mount_target(mount: &Value) -> Result<String> {
    if let Some(object) = mount.as_object() {
        return ["target", "dst", "destination"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))
            .map(ToString::to_string)
            .ok_or_else(|| SpikeError::Profile {
                path: "$.mounts".to_string(),
                detail: "mount object has no target".to_string(),
            });
    }
    let string = mount.as_str().ok_or_else(|| SpikeError::Profile {
        path: "$.mounts".to_string(),
        detail: "mount must be a string or object".to_string(),
    })?;
    string
        .split(',')
        .find_map(|field| {
            let (key, value) = field.split_once('=')?;
            matches!(key, "target" | "dst" | "destination").then(|| value.to_string())
        })
        .ok_or_else(|| SpikeError::Profile {
            path: "$.mounts".to_string(),
            detail: format!("mount `{string}` has no target"),
        })
}

fn merge_forward_ports(sources: &[Value]) -> Vec<Value> {
    let mut order = Vec::<String>::new();
    let mut values = BTreeMap::<String, Value>::new();
    for value in sources
        .iter()
        .filter_map(|source| source.get("forwardPorts").and_then(Value::as_array))
        .flatten()
    {
        let key = match value {
            Value::Number(number) => number.to_string(),
            Value::String(string) => string
                .rsplit_once(':')
                .map_or_else(|| string.clone(), |(_, port)| port.to_string()),
            _ => value.to_string(),
        };
        if !values.contains_key(&key) {
            order.push(key.clone());
        }
        values.insert(key, value.clone());
    }
    order
        .iter()
        .filter_map(|key| values.get(key).cloned())
        .collect()
}

fn merge_host_requirements(sources: &[Value]) -> Result<Option<Value>> {
    let mut cpus = None::<u64>;
    let mut memory = None::<u64>;
    let mut storage = None::<u64>;
    let mut gpu = None::<Value>;
    for requirements in sources
        .iter()
        .filter_map(|source| source.get("hostRequirements").and_then(Value::as_object))
    {
        cpus = cpus.max(requirements.get("cpus").and_then(Value::as_u64));
        memory = memory.max(
            requirements
                .get("memory")
                .and_then(Value::as_str)
                .map(parse_size)
                .transpose()?,
        );
        storage = storage.max(
            requirements
                .get("storage")
                .and_then(Value::as_str)
                .map(parse_size)
                .transpose()?,
        );
        if let Some(value) = requirements.get("gpu") {
            gpu = Some(value.clone());
        }
    }
    if cpus.is_none() && memory.is_none() && storage.is_none() && gpu.is_none() {
        return Ok(None);
    }
    let mut output = Map::new();
    if let Some(value) = cpus {
        output.insert("cpus".to_string(), Value::from(value));
    }
    if let Some(value) = memory {
        output.insert("memoryBytes".to_string(), Value::from(value));
    }
    if let Some(value) = storage {
        output.insert("storageBytes".to_string(), Value::from(value));
    }
    if let Some(value) = gpu {
        output.insert("gpu".to_string(), value);
    }
    Ok(Some(Value::Object(output)))
}

fn parse_size(value: &str) -> Result<u64> {
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let number = value[..split]
        .parse::<u64>()
        .map_err(|error| SpikeError::Profile {
            path: "$.hostRequirements".to_string(),
            detail: format!("invalid size `{value}`: {error}"),
        })?;
    let multiplier = match &value[split..].to_ascii_lowercase()[..] {
        "" => 1,
        "kb" => 1024,
        "mb" => 1024_u64.pow(2),
        "gb" => 1024_u64.pow(3),
        "tb" => 1024_u64.pow(4),
        unit => {
            return Err(SpikeError::Profile {
                path: "$.hostRequirements".to_string(),
                detail: format!("unsupported size unit `{unit}`"),
            });
        }
    };
    number
        .checked_mul(multiplier)
        .ok_or_else(|| SpikeError::Profile {
            path: "$.hostRequirements".to_string(),
            detail: format!("size `{value}` overflows"),
        })
}

#[expect(
    clippy::too_many_lines,
    reason = "the disposable ordering spike keeps graph loading, validation, and ordering visible together"
)]
pub(crate) fn resolve_local_feature_order(
    config_path: &Path,
    feature_values: &Map<String, Value>,
    override_order: &[String],
) -> Result<Vec<OrderedFeature>> {
    let config_dir = config_path.parent().ok_or_else(|| {
        SpikeError::Feature(format!(
            "configuration `{}` has no parent",
            config_path.display()
        ))
    })?;
    let mut nodes = BTreeMap::<String, FeatureNode>::new();
    let mut worklist = feature_values
        .iter()
        .filter(|(source, _)| source.starts_with("./"))
        .map(|(source, options)| (source.clone(), options.clone()))
        .collect::<Vec<_>>();

    while let Some((source, provided_options)) = worklist.pop() {
        let normalized = normalize_local_source(&source)?;
        let provided_options = provided_options.as_object().cloned().unwrap_or_default();
        let metadata_path = config_dir
            .join(&normalized)
            .join("devcontainer-feature.json");
        let metadata: RawFeatureMetadata =
            serde_json::from_slice(&read(&metadata_path)?).map_err(|error| {
                SpikeError::Feature(format!("{}: {error}", metadata_path.display()))
            })?;
        let options = validate_feature_options(&metadata, &provided_options)?;
        if let Some(existing) = nodes.get(&normalized) {
            if existing.options != options {
                return Err(SpikeError::Feature(format!(
                    "conflicting options for dependency `{normalized}`: {} versus {}",
                    serde_json::to_string(&existing.options).unwrap_or_default(),
                    serde_json::to_string(&options).unwrap_or_default()
                )));
            }
            continue;
        }
        let folder = Path::new(&normalized)
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default();
        if folder != metadata.id {
            return Err(SpikeError::Feature(format!(
                "local Feature folder `{folder}` does not match metadata id `{}`",
                metadata.id
            )));
        }
        let dependencies = metadata
            .depends_on
            .keys()
            .map(|dependency| normalize_local_source(dependency))
            .collect::<Result<BTreeSet<_>>>()?;
        for (dependency, dependency_options) in &metadata.depends_on {
            worklist.push((dependency.clone(), dependency_options.clone()));
        }
        nodes.insert(
            normalized.clone(),
            FeatureNode {
                metadata,
                source: normalized,
                options,
                dependencies,
                priority: 0,
            },
        );
    }

    let id_to_source = nodes
        .iter()
        .map(|(source, node)| (node.metadata.id.clone(), source.clone()))
        .collect::<HashMap<_, _>>();
    for node in nodes.values_mut() {
        for soft in &node.metadata.installs_after {
            if let Some(source) = id_to_source.get(soft) {
                node.dependencies.insert(source.clone());
            }
        }
        if let Some(index) = override_order
            .iter()
            .position(|candidate| candidate == &node.metadata.id || candidate == &node.source)
        {
            node.priority = override_order.len() - index;
        }
    }

    let mut installed = BTreeSet::new();
    let mut output = Vec::new();
    while installed.len() < nodes.len() {
        let mut round = nodes
            .iter()
            .filter(|(source, node)| {
                !installed.contains(*source)
                    && node
                        .dependencies
                        .iter()
                        .all(|dependency| installed.contains(dependency))
            })
            .collect::<Vec<_>>();
        if round.is_empty() {
            let unresolved = nodes
                .iter()
                .filter(|(source, _)| !installed.contains(*source))
                .map(|(source, node)| {
                    format!(
                        "{source} -> {}",
                        node.dependencies
                            .iter()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })
                .collect::<Vec<_>>()
                .join("; ");
            return Err(SpikeError::Feature(format!(
                "dependency cycle or inconsistent graph: {unresolved}"
            )));
        }
        let max_priority = round
            .iter()
            .map(|(_, node)| node.priority)
            .max()
            .unwrap_or(0);
        round.retain(|(_, node)| node.priority == max_priority);
        round.sort_by(|(left_source, left), (right_source, right)| {
            left.metadata
                .id
                .cmp(&right.metadata.id)
                .then_with(|| left.metadata.version.cmp(&right.metadata.version))
                .then_with(|| right.options.len().cmp(&left.options.len()))
                .then_with(|| left_source.cmp(right_source))
        });
        for (source, node) in round {
            installed.insert(source.clone());
            output.push(OrderedFeature {
                id: node.metadata.id.clone(),
                source: node.source.clone(),
                version: node.metadata.version.clone(),
                options: node.options.clone(),
            });
        }
    }
    Ok(output)
}

fn normalize_local_source(source: &str) -> Result<String> {
    validate_feature_source(source)?;
    let without_prefix = source.strip_prefix("./").ok_or_else(|| {
        SpikeError::Feature(format!(
            "dependency `{source}` is not a local Feature in this fixture"
        ))
    })?;
    Ok(format!("./{without_prefix}"))
}

fn validate_feature_options(
    metadata: &RawFeatureMetadata,
    provided: &Map<String, Value>,
) -> Result<BTreeMap<String, Value>> {
    for key in provided.keys() {
        if !metadata.options.contains_key(key) {
            return Err(SpikeError::Feature(format!(
                "Feature `{}` has no option `{key}`",
                metadata.id
            )));
        }
    }
    let mut output = BTreeMap::new();
    for (name, definition) in &metadata.options {
        let value = provided
            .get(name)
            .cloned()
            .or_else(|| definition.default.clone())
            .ok_or_else(|| {
                SpikeError::Feature(format!(
                    "Feature `{}` requires option `{name}`",
                    metadata.id
                ))
            })?;
        let valid_type = matches!(
            (definition.kind.as_str(), &value),
            ("boolean", Value::Bool(_)) | ("string", Value::String(_))
        );
        if !valid_type {
            return Err(SpikeError::Feature(format!(
                "Feature `{}` option `{name}` must be {}",
                metadata.id, definition.kind
            )));
        }
        let allowed = if definition.enum_values.is_empty() {
            &definition.proposals
        } else {
            &definition.enum_values
        };
        if !allowed.is_empty()
            && value
                .as_str()
                .is_some_and(|candidate| !allowed.iter().any(|item| item == candidate))
        {
            return Err(SpikeError::Feature(format!(
                "Feature `{}` option `{name}` has unsupported value `{value}`",
                metadata.id
            )));
        }
        output.insert(name.clone(), value);
    }
    Ok(output)
}

pub(crate) fn feature_order_digest(order: &[OrderedFeature]) -> Result<String> {
    let bytes = canonical_json(&json!({ "features": order }))?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> std::path::PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("vendor/devcontainers-spec/schemas/devContainer.base.schema.json")
    }

    fn validate_text(text: &str) -> Result<Value> {
        let directory = tempfile::tempdir().expect("temporary fixture should be created");
        let path = directory.path().join("devcontainer.json");
        std::fs::write(&path, text).expect("temporary fixture should be written");
        parse_and_validate(&path, &schema())
    }

    fn substitutions<'a>(
        local_env: &'a BTreeMap<String, String>,
        container_env: Option<&'a BTreeMap<String, String>>,
    ) -> SubstitutionContext<'a> {
        SubstitutionContext {
            local_workspace: Path::new("/checkout/example"),
            container_workspace: "/workspaces/example",
            local_env,
            container_env,
            devcontainer_id: "stableid",
        }
    }

    #[test]
    fn devcontainer_id_matches_the_pinned_bigint_base32_algorithm() {
        let labels = BTreeMap::from([
            ("a".to_string(), "1".to_string()),
            ("b".to_string(), "2".to_string()),
        ]);

        let result = devcontainer_id(&labels);

        assert_eq!(
            result,
            "08fndntvsrfu47rm420eui212b7iikkn9jnj0t0vq69hs73di88i"
        );
    }

    #[test]
    fn host_substitution_leaves_container_environment_for_runtime() {
        let local_env = BTreeMap::new();
        let context = substitutions(&local_env, None);

        let result = substitute_string("${containerEnv:PATH}:/bin", &context);

        assert_eq!(
            result.expect("substitution should succeed"),
            "${containerEnv:PATH}:/bin"
        );
    }

    #[test]
    fn runtime_substitution_uses_container_environment() {
        let local_env = BTreeMap::new();
        let container_env = BTreeMap::from([("PATH".to_string(), "/usr/bin".to_string())]);
        let context = substitutions(&local_env, Some(&container_env));

        let result = substitute_string("${containerEnv:PATH}:/bin", &context);

        assert_eq!(
            result.expect("substitution should succeed"),
            "/usr/bin:/bin"
        );
    }

    #[test]
    fn reserved_output_option_fails_with_the_exact_argument() {
        let options = vec![Value::String(
            "--output=type=local,dest=checkout".to_string(),
        )];

        let error = validate_build_options(&options).expect_err("reserved option should fail");

        assert!(
            error
                .to_string()
                .contains("--output=type=local,dest=checkout")
        );
    }

    #[test]
    fn insecure_feature_source_fails_closed() {
        let error = validate_feature_source("http://example.test/feature.tgz")
            .expect_err("HTTP should fail");

        assert!(error.to_string().contains("insecure HTTP"));
    }

    #[test]
    fn metadata_merge_uses_repository_last_for_mount_conflicts() {
        let image = json!({
            "mounts": [{"source": "image", "target": "/same", "type": "volume"}]
        });
        let repository = json!({
            "mounts": [{"source": "repository", "target": "/same", "type": "volume"}]
        });

        let result = merge_metadata(&[image], &repository).expect("merge should succeed");

        assert_eq!(result["mounts"][0]["source"], "repository");
    }

    #[test]
    fn metadata_merge_uses_maximum_host_requirement() {
        let image = json!({"hostRequirements": {"memory": "2gb"}});
        let repository = json!({"hostRequirements": {"memory": "1gb"}});

        let result = merge_metadata(&[image], &repository).expect("merge should succeed");

        assert_eq!(result["hostRequirements"]["memoryBytes"], 2_147_483_648_u64);
    }

    #[test]
    fn comments_are_accepted_but_trailing_commas_are_rejected() {
        let value =
            validate_text("{ // supported JSONC comment\n \"image\": \"debian:13-slim\"\n}")
                .expect("commented configuration should validate");
        assert_eq!(value["image"], "debian:13-slim");

        let error = validate_text("{\"image\":\"debian:13-slim\",}")
            .expect_err("trailing comma should fail closed");
        assert!(error.to_string().contains("JSONC"));
    }

    #[test]
    fn unknown_and_oversized_configuration_fail_closed() {
        let unknown =
            validate_text("{\"image\":\"debian:13-slim\",\"futureSemanticProperty\":true}")
                .expect_err("unknown property should fail");
        assert!(
            unknown.to_string().contains("futureSemanticProperty")
                || unknown.to_string().contains("unknown behavioral property")
        );

        let oversized = format!(
            "{{\"image\":\"debian:13-slim\",\"name\":\"{}\"}}",
            "x".repeat(MAX_CONFIG_BYTES)
        );
        let error = validate_text(&oversized).expect_err("oversized input should fail");
        assert!(error.to_string().contains("1048576"));
    }

    #[test]
    fn feature_dependency_cycle_fails_closed() {
        let directory = tempfile::tempdir().expect("temporary fixture should be created");
        let config_dir = directory.path().join(".devcontainer");
        for (id, dependency) in [("a", "b"), ("b", "a")] {
            let feature_dir = config_dir.join(format!("features/{id}"));
            std::fs::create_dir_all(&feature_dir).expect("Feature directory should be created");
            std::fs::write(
                feature_dir.join("devcontainer-feature.json"),
                serde_json::to_vec(&json!({
                    "id": id,
                    "version": "1.0.0",
                    "dependsOn": {format!("./features/{dependency}"): {}}
                }))
                .expect("metadata should serialize"),
            )
            .expect("metadata should be written");
        }
        let features = json!({"./features/a": {}})
            .as_object()
            .cloned()
            .expect("fixture should be an object");

        let error =
            resolve_local_feature_order(&config_dir.join("devcontainer.json"), &features, &[])
                .expect_err("dependency cycle should fail");

        assert!(error.to_string().contains("cycle"));
    }

    #[test]
    fn conflicting_recursive_feature_options_fail_closed() {
        let directory = tempfile::tempdir().expect("temporary fixture should be created");
        let config_dir = directory.path().join(".devcontainer");
        for id in ["base", "top"] {
            let feature_dir = config_dir.join(format!("features/{id}"));
            std::fs::create_dir_all(&feature_dir).expect("Feature directory should be created");
            let depends_on = if id == "top" {
                json!({"./features/base": {"message": "dependency"}})
            } else {
                json!({})
            };
            std::fs::write(
                feature_dir.join("devcontainer-feature.json"),
                serde_json::to_vec(&json!({
                    "id": id,
                    "version": "1.0.0",
                    "dependsOn": depends_on,
                    "options": {"message": {"type": "string", "default": "default"}}
                }))
                .expect("metadata should serialize"),
            )
            .expect("metadata should be written");
        }
        let features = json!({
            "./features/base": {"message": "direct"},
            "./features/top": {}
        })
        .as_object()
        .cloned()
        .expect("fixture should be an object");

        let error =
            resolve_local_feature_order(&config_dir.join("devcontainer.json"), &features, &[])
                .expect_err("conflicting options should fail");

        assert!(error.to_string().contains("conflicting options"));
    }

    #[test]
    fn canonical_json_sorts_object_keys() {
        let value = json!({"z": 1, "a": {"d": 2, "b": 1}});

        let result = canonical_json(&value).expect("normalization should succeed");

        assert_eq!(
            String::from_utf8(result).expect("JSON is UTF-8"),
            "{\n  \"a\": {\n    \"b\": 1,\n    \"d\": 2\n  },\n  \"z\": 1\n}\n"
        );
    }
}
