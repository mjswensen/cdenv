//! Typed, pure validation for the pinned V1 Dev Container profile.

use std::collections::BTreeMap;
use std::fmt;

use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Diagnostic, RawDocument, SourceSpan};

const PROPERTY_FAMILIES: &[CapabilityFamily] = &[
    CapabilityFamily {
        name: "authoring",
        properties: &["$schema", "name", "customizations", "secrets"],
    },
    CapabilityFamily {
        name: "scenarios",
        properties: &[
            "image",
            "build",
            "dockerComposeFile",
            "service",
            "runServices",
        ],
    },
    CapabilityFamily {
        name: "workspace",
        properties: &["workspaceFolder", "workspaceMount", "mounts"],
    },
    CapabilityFamily {
        name: "features",
        properties: &["features", "overrideFeatureInstallOrder"],
    },
    CapabilityFamily {
        name: "environment-and-users",
        properties: &[
            "containerEnv",
            "remoteEnv",
            "containerUser",
            "remoteUser",
            "updateRemoteUserUID",
            "userEnvProbe",
        ],
    },
    CapabilityFamily {
        name: "container-runtime",
        properties: &[
            "overrideCommand",
            "init",
            "privileged",
            "capAdd",
            "securityOpt",
            "runArgs",
        ],
    },
    CapabilityFamily {
        name: "ports",
        properties: &[
            "appPort",
            "forwardPorts",
            "portsAttributes",
            "otherPortsAttributes",
        ],
    },
    CapabilityFamily {
        name: "lifecycle",
        properties: &[
            "initializeCommand",
            "onCreateCommand",
            "updateContentCommand",
            "postCreateCommand",
            "postStartCommand",
            "postAttachCommand",
            "waitFor",
            "shutdownAction",
        ],
    },
    CapabilityFamily {
        name: "host-requirements",
        properties: &["hostRequirements"],
    },
];

/// A validated, unmerged and unsubstituted V1 profile.
#[derive(Clone, Debug, PartialEq)]
pub struct RawProfile {
    /// Authoring-only schema URI. It is never followed at runtime.
    pub schema_uri: Option<String>,
    /// Optional display name.
    pub name: Option<String>,
    /// Structurally valid scenario selection.
    pub scenario: RawScenario,
    /// Properties shared by all scenarios.
    pub common: RawCommon,
}

/// Exactly one supported container scenario.
#[derive(Clone, Debug, PartialEq)]
pub enum RawScenario {
    /// Use an existing Docker image.
    Image(Box<ImageScenario>),
    /// Build a Docker image with `BuildKit`.
    Dockerfile(Box<DockerfileScenario>),
    /// Use explicit Docker Compose V2 files and a primary service.
    Compose(Box<ComposeScenario>),
}

/// Existing-image scenario input.
#[derive(Clone, Debug, PartialEq)]
/// Typed value from the validated raw profile.
pub struct ImageScenario {
    /// Validated raw property value.
    pub image: String,
    /// Validated raw property value.
    pub options: NonComposeOptions,
}

/// Dockerfile scenario input.
#[derive(Clone, Debug, PartialEq)]
/// Typed value from the validated raw profile.
pub struct DockerfileScenario {
    /// Validated raw property value.
    pub build: RawBuild,
    /// Validated raw property value.
    pub options: NonComposeOptions,
}

/// Compose V2 scenario input.
#[derive(Clone, Debug, PartialEq)]
/// Typed value from the validated raw profile.
pub struct ComposeScenario {
    /// Validated raw property value.
    pub files: Vec<String>,
    /// Validated raw property value.
    pub service: String,
    /// Validated raw property value.
    pub run_services: Option<Vec<String>>,
    /// Validated raw property value.
    pub workspace_folder: String,
    /// Validated raw property value.
    pub shutdown_action: Option<ShutdownAction>,
    /// Validated raw property value.
    pub override_command: Option<bool>,
}

/// Supported Docker build properties before option-conflict planning.
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct RawBuild {
    /// Validated raw property value.
    pub dockerfile: String,
    /// Validated raw property value.
    pub context: Option<String>,
    /// Validated raw property value.
    pub target: Option<String>,
    /// Validated raw property value.
    pub args: BTreeMap<String, String>,
    /// Validated raw property value.
    pub cache_from: Vec<String>,
    /// Validated raw property value.
    pub options: Vec<String>,
}

/// Properties available only to image and Dockerfile scenarios.
#[derive(Clone, Debug, Default, PartialEq)]
/// Typed value from the validated raw profile.
pub struct NonComposeOptions {
    /// Validated raw property value.
    pub workspace_folder: Option<String>,
    /// Validated raw property value.
    pub workspace_mount: Option<String>,
    /// Validated raw property value.
    pub app_ports: Vec<AppPort>,
    /// Validated raw property value.
    pub run_args: Vec<String>,
    /// Validated raw property value.
    pub shutdown_action: Option<ShutdownAction>,
    /// Validated raw property value.
    pub override_command: Option<bool>,
}

/// Properties shared by all scenarios.
#[derive(Clone, Debug, Default, PartialEq)]
/// Typed value from the validated raw profile.
pub struct RawCommon {
    /// Validated raw property value.
    pub features: BTreeMap<String, RawFeature>,
    /// Validated raw property value.
    pub override_feature_install_order: Vec<String>,
    /// Validated raw property value.
    pub container_env: BTreeMap<String, String>,
    /// Validated raw property value.
    pub remote_env: BTreeMap<String, Option<String>>,
    /// Validated raw property value.
    pub container_user: Option<String>,
    /// Validated raw property value.
    pub remote_user: Option<String>,
    /// Validated raw property value.
    pub update_remote_user_uid: Option<bool>,
    /// Validated raw property value.
    pub user_env_probe: Option<UserEnvProbe>,
    /// Validated raw property value.
    pub init: Option<bool>,
    /// Validated raw property value.
    pub privileged: Option<bool>,
    /// Validated raw property value.
    pub cap_add: Vec<String>,
    /// Validated raw property value.
    pub security_opt: Vec<String>,
    /// Validated raw property value.
    pub mounts: Vec<RawMount>,
    /// Validated raw property value.
    pub forward_ports: Vec<ForwardPort>,
    /// Validated raw property value.
    pub ports_attributes: BTreeMap<String, PortAttributes>,
    /// Validated raw property value.
    pub other_ports_attributes: Option<PortAttributes>,
    /// Validated raw property value.
    pub lifecycle: LifecycleCommands,
    /// Validated raw property value.
    pub wait_for: Option<WaitFor>,
    /// Validated raw property value.
    pub host_requirements: Option<HostRequirements>,
    /// Validated raw property value.
    pub customizations: BTreeMap<String, Value>,
    /// Validated raw property value.
    pub secrets: BTreeMap<String, SecretMetadata>,
}

/// A supported Feature source and its typed option values.
#[derive(Clone, Debug, PartialEq)]
/// Typed value from the validated raw profile.
pub struct RawFeature {
    /// Validated raw property value.
    pub source: FeatureSource,
    /// Validated raw property value.
    pub options: BTreeMap<String, FeatureOptionValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum FeatureSource {
    /// Validated profile choice.
    Oci(String),
    /// Validated profile choice.
    Https(String),
    /// Validated profile choice.
    Local(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum FeatureOptionValue {
    /// Validated profile choice.
    Boolean(bool),
    /// Validated profile choice.
    String(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum RawMount {
    /// Validated profile choice.
    String(String),
    /// Validated profile choice.
    Object {
        /// Validated variant value.
        kind: MountKind,
        /// Validated variant value.
        source: Option<String>,
        /// Validated variant value.
        target: String,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum MountKind {
    /// Validated profile choice.
    Bind,
    /// Validated profile choice.
    Volume,
}
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum AppPort {
    /// Validated profile choice.
    Number(u16),
    /// Validated profile choice.
    DockerArgument(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum ForwardPort {
    /// Validated profile choice.
    Container(u16),
    /// Validated profile choice.
    Service {
        /// Compose service name.
        service: String,
        /// Container port.
        port: u16,
    },
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum ShutdownAction {
    /// Validated profile choice.
    None,
    /// Validated profile choice.
    StopContainer,
    /// Validated profile choice.
    StopCompose,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Typed value from the validated raw profile.
pub enum UserEnvProbe {
    /// Validated profile choice.
    None,
    /// Validated profile choice.
    LoginShell,
    /// Validated profile choice.
    LoginInteractiveShell,
    /// Validated profile choice.
    InteractiveShell,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum WaitFor {
    /// Validated profile choice.
    Initialize,
    /// Validated profile choice.
    OnCreate,
    /// Validated profile choice.
    UpdateContent,
    /// Validated profile choice.
    PostCreate,
    /// Validated profile choice.
    PostStart,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
/// Typed value from the validated raw profile.
pub enum AutoForwardAction {
    /// Validated profile choice.
    Notify,
    /// Validated profile choice.
    OpenBrowser,
    /// Validated profile choice.
    OpenBrowserOnce,
    /// Validated profile choice.
    OpenPreview,
    /// Validated profile choice.
    Silent,
    /// Validated profile choice.
    Ignore,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
/// Typed value from the validated raw profile.
pub enum PortProtocol {
    /// Validated profile choice.
    Http,
    /// Validated profile choice.
    Https,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct PortAttributes {
    /// Validated raw property value.
    pub on_auto_forward: Option<AutoForwardAction>,
    /// Validated raw property value.
    pub elevate_if_needed: Option<bool>,
    /// Validated raw property value.
    pub label: Option<String>,
    /// Validated raw property value.
    pub require_local_port: Option<bool>,
    /// Validated raw property value.
    pub protocol: Option<PortProtocol>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum RawCommand {
    /// Validated profile choice.
    Shell(String),
    /// Validated profile choice.
    Exec(Vec<String>),
    /// Validated profile choice.
    Parallel(BTreeMap<String, CommandValue>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum CommandValue {
    /// Validated profile choice.
    Shell(String),
    /// Validated profile choice.
    Exec(Vec<String>),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct LifecycleCommands {
    /// Validated raw property value.
    pub initialize: Option<RawCommand>,
    /// Validated raw property value.
    pub on_create: Option<RawCommand>,
    /// Validated raw property value.
    pub update_content: Option<RawCommand>,
    /// Validated raw property value.
    pub post_create: Option<RawCommand>,
    /// Validated raw property value.
    pub post_start: Option<RawCommand>,
    /// Validated raw property value.
    pub post_attach: Option<RawCommand>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct HostRequirements {
    /// Validated raw property value.
    pub cpus: Option<u64>,
    /// Validated raw property value.
    pub memory: Option<String>,
    /// Validated raw property value.
    pub storage: Option<String>,
    /// Validated raw property value.
    pub gpu: Option<GpuRequirement>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum GpuRequirement {
    /// Validated profile choice.
    Required,
    /// Validated profile choice.
    NotRequired,
    /// Validated profile choice.
    Optional,
    /// Validated profile choice.
    Detailed {
        /// Validated variant value.
        cores: Option<u64>,
        /// Validated variant value.
        memory: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct SecretMetadata {
    /// Validated raw property value.
    pub description: Option<String>,
    /// Validated raw property value.
    pub documentation_url: Option<String>,
}

/// Stable class of profile validation failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub enum ProfileErrorKind {
    /// Validated profile choice.
    InvalidType,
    /// Validated profile choice.
    MissingProperty,
    /// Validated profile choice.
    UnknownProperty,
    /// Validated profile choice.
    UnsupportedValue,
    /// Validated profile choice.
    InvalidValue,
    /// Validated profile choice.
    ScenarioConflict,
}

/// A source-located profile validation failure.
#[derive(Clone, Debug, PartialEq)]
/// Typed value from the validated raw profile.
pub struct ProfileError {
    /// Validated raw property value.
    pub kind: ProfileErrorKind,
    /// Validated raw property value.
    pub diagnostic: Box<Diagnostic>,
    /// Rejected JSON value when one was present.
    /// Validated raw property value.
    pub value: Option<Value>,
}

impl std::error::Error for ProfileError {}
impl fmt::Display for ProfileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.diagnostic.fmt(formatter)
    }
}

/// One deterministic group in the profile support report.
#[derive(Clone, Debug, PartialEq, Eq)]
/// Typed value from the validated raw profile.
pub struct Capability {
    /// Validated raw property value.
    pub family: &'static str,
    /// Validated raw property value.
    pub properties: &'static [&'static str],
}
#[derive(Clone, Copy)]
struct CapabilityFamily {
    name: &'static str,
    properties: &'static [&'static str],
}

/// Returns supported top-level behavioral properties grouped in reviewed order.
#[must_use]
pub fn capability_report() -> Vec<Capability> {
    PROPERTY_FAMILIES
        .iter()
        .map(|family| Capability {
            family: family.name,
            properties: family.properties,
        })
        .collect()
}

/// Renders the deterministic machine-readable support report.
///
/// # Errors
///
/// Returns an error only if serialization of the internally defined report
/// fails.
pub fn capability_report_json() -> Result<String, serde_json::Error> {
    let families = PROPERTY_FAMILIES.iter().map(|family| {
        serde_json::json!({
            "family": family.name,
            "properties": family.properties,
        })
    });
    serde_json::to_string_pretty(&serde_json::json!({
        "profileRevision": crate::PROFILE_REVISION,
        "families": families.collect::<Vec<_>>(),
    }))
}

/// Validates a bounded JSONC document against `cdenv-devcontainer-v1`.
///
/// This function performs no I/O, metadata merge, substitution, download, or
/// host capability check.
///
/// # Errors
///
/// Returns a source-located [`ProfileError`] when the document does not conform
/// to the pinned profile.
pub fn validate_profile(document: &RawDocument) -> Result<RawProfile, ProfileError> {
    Validator { document }.validate()
}

struct Validator<'a> {
    document: &'a RawDocument,
}

impl Validator<'_> {
    fn validate(&self) -> Result<RawProfile, ProfileError> {
        let root = self.object(self.document.value(), "$")?;
        for (key, value) in root {
            if !PROPERTY_FAMILIES
                .iter()
                .any(|family| family.properties.contains(&key.as_str()))
            {
                let kind = if matches!(key.as_str(), "dockerFile" | "context") {
                    ProfileErrorKind::UnsupportedValue
                } else {
                    ProfileErrorKind::UnknownProperty
                };
                return Err(self.error(kind, &path("$", key), Some(value), if kind == ProfileErrorKind::UnsupportedValue { "deprecated top-level Dockerfile form is unsupported; use `build.dockerfile`" } else { "unknown top-level behavioral property" }));
            }
        }

        let image = root.get("image");
        let build = root.get("build");
        let compose = root.get("dockerComposeFile");
        let selected = usize::from(image.is_some())
            + usize::from(build.is_some())
            + usize::from(compose.is_some());
        if selected != 1 {
            return Err(self.error(
                ProfileErrorKind::ScenarioConflict,
                "$",
                Some(self.document.value()),
                "exactly one of `image`, `build`, or `dockerComposeFile` is required",
            ));
        }

        let schema_uri = self.optional_string(root, "$schema")?;
        let name = self.optional_string(root, "name")?;
        let common = self.common(root)?;
        let scenario = if image.is_some() {
            self.reject_compose_only(root)?;
            RawScenario::Image(Box::new(ImageScenario {
                image: self.required_string(root, "$", "image")?,
                options: self.non_compose(root)?,
            }))
        } else if let Some(value) = build {
            self.reject_compose_only(root)?;
            RawScenario::Dockerfile(Box::new(DockerfileScenario {
                build: self.build(value)?,
                options: self.non_compose(root)?,
            }))
        } else {
            self.reject_non_compose_only(root)?;
            let compose = compose.ok_or_else(|| {
                self.error(
                    ProfileErrorKind::ScenarioConflict,
                    "$.dockerComposeFile",
                    None,
                    "Compose scenario selection is missing",
                )
            })?;
            RawScenario::Compose(Box::new(self.compose(root, compose)?))
        };
        Ok(RawProfile {
            schema_uri,
            name,
            scenario,
            common,
        })
    }

    fn common(&self, root: &Map<String, Value>) -> Result<RawCommon, ProfileError> {
        Ok(RawCommon {
            features: self.features(root.get("features"))?,
            override_feature_install_order: self
                .string_array(
                    root.get("overrideFeatureInstallOrder"),
                    "$.overrideFeatureInstallOrder",
                )?
                .unwrap_or_default(),
            container_env: self.string_map(root.get("containerEnv"), "$.containerEnv", false)?,
            remote_env: self.nullable_string_map(root.get("remoteEnv"), "$.remoteEnv")?,
            container_user: self.optional_string(root, "containerUser")?,
            remote_user: self.optional_string(root, "remoteUser")?,
            update_remote_user_uid: self.optional_bool(root, "updateRemoteUserUID")?,
            user_env_probe: self.enum_optional(
                root,
                "userEnvProbe",
                &[
                    "none",
                    "loginShell",
                    "loginInteractiveShell",
                    "interactiveShell",
                ],
                |value| match value {
                    "none" => UserEnvProbe::None,
                    "loginShell" => UserEnvProbe::LoginShell,
                    "loginInteractiveShell" => UserEnvProbe::LoginInteractiveShell,
                    _ => UserEnvProbe::InteractiveShell,
                },
            )?,
            init: self.optional_bool(root, "init")?,
            privileged: self.optional_bool(root, "privileged")?,
            cap_add: self
                .string_array(root.get("capAdd"), "$.capAdd")?
                .unwrap_or_default(),
            security_opt: self
                .string_array(root.get("securityOpt"), "$.securityOpt")?
                .unwrap_or_default(),
            mounts: self.mounts(root.get("mounts"))?,
            forward_ports: self.forward_ports(root.get("forwardPorts"))?,
            ports_attributes: self
                .port_attributes_map(root.get("portsAttributes"), "$.portsAttributes")?,
            other_ports_attributes: root
                .get("otherPortsAttributes")
                .map(|value| self.port_attributes(value, "$.otherPortsAttributes", false))
                .transpose()?,
            lifecycle: LifecycleCommands {
                initialize: self.command(root.get("initializeCommand"), "$.initializeCommand")?,
                on_create: self.command(root.get("onCreateCommand"), "$.onCreateCommand")?,
                update_content: self
                    .command(root.get("updateContentCommand"), "$.updateContentCommand")?,
                post_create: self.command(root.get("postCreateCommand"), "$.postCreateCommand")?,
                post_start: self.command(root.get("postStartCommand"), "$.postStartCommand")?,
                post_attach: self.command(root.get("postAttachCommand"), "$.postAttachCommand")?,
            },
            wait_for: self.enum_optional(
                root,
                "waitFor",
                &[
                    "initializeCommand",
                    "onCreateCommand",
                    "updateContentCommand",
                    "postCreateCommand",
                    "postStartCommand",
                ],
                |value| match value {
                    "initializeCommand" => WaitFor::Initialize,
                    "onCreateCommand" => WaitFor::OnCreate,
                    "updateContentCommand" => WaitFor::UpdateContent,
                    "postCreateCommand" => WaitFor::PostCreate,
                    _ => WaitFor::PostStart,
                },
            )?,
            host_requirements: root
                .get("hostRequirements")
                .map(|value| self.host_requirements(value))
                .transpose()?,
            customizations: self.customizations(root.get("customizations"))?,
            secrets: self.secrets(root.get("secrets"))?,
        })
    }

    fn build(&self, value: &Value) -> Result<RawBuild, ProfileError> {
        let object = self.object(value, "$.build")?;
        self.reject_unknown(
            object,
            "$.build",
            &[
                "dockerfile",
                "context",
                "target",
                "args",
                "cacheFrom",
                "options",
            ],
        )?;
        let dockerfile = self.required_string(object, "$.build", "dockerfile")?;
        Ok(RawBuild {
            dockerfile,
            context: self.optional_string_at(object, "$.build", "context")?,
            target: self.optional_string_at(object, "$.build", "target")?,
            args: self.string_map(object.get("args"), "$.build.args", false)?,
            cache_from: self.string_or_array(object.get("cacheFrom"), "$.build.cacheFrom")?,
            options: self
                .string_array(object.get("options"), "$.build.options")?
                .unwrap_or_default(),
        })
    }

    fn non_compose(&self, root: &Map<String, Value>) -> Result<NonComposeOptions, ProfileError> {
        let shutdown_action = self.enum_optional(
            root,
            "shutdownAction",
            &["none", "stopContainer"],
            |value| {
                if value == "none" {
                    ShutdownAction::None
                } else {
                    ShutdownAction::StopContainer
                }
            },
        )?;
        Ok(NonComposeOptions {
            workspace_folder: self.optional_string(root, "workspaceFolder")?,
            workspace_mount: self.optional_string(root, "workspaceMount")?,
            app_ports: self.app_ports(root.get("appPort"))?,
            run_args: self
                .string_array(root.get("runArgs"), "$.runArgs")?
                .unwrap_or_default(),
            shutdown_action,
            override_command: self.optional_bool(root, "overrideCommand")?,
        })
    }

    fn compose(
        &self,
        root: &Map<String, Value>,
        files: &Value,
    ) -> Result<ComposeScenario, ProfileError> {
        let files = self.one_or_many_strings(files, "$.dockerComposeFile")?;
        if files.is_empty() {
            return Err(self.invalid(
                "$.dockerComposeFile",
                &Value::Array(Vec::new()),
                "at least one Compose file is required",
            ));
        }
        if let Some((index, _)) = files.iter().enumerate().find(|(_, file)| file.is_empty()) {
            return Err(self.invalid(
                &index_path("$.dockerComposeFile", index),
                &Value::String(String::new()),
                "Compose file must not be empty",
            ));
        }
        let service = self.required_string(root, "$", "service")?;
        let workspace_folder = self.required_string(root, "$", "workspaceFolder")?;
        let run_services = self.string_array(root.get("runServices"), "$.runServices")?;
        if let Some(services) = &run_services
            && services.is_empty()
        {
            return Err(self.invalid(
                "$.runServices",
                &Value::Array(Vec::new()),
                "at least one run service is required when present",
            ));
        }
        let shutdown_action =
            self.enum_optional(root, "shutdownAction", &["none", "stopCompose"], |value| {
                if value == "none" {
                    ShutdownAction::None
                } else {
                    ShutdownAction::StopCompose
                }
            })?;
        Ok(ComposeScenario {
            files,
            service,
            run_services,
            workspace_folder,
            shutdown_action,
            override_command: self.optional_bool(root, "overrideCommand")?,
        })
    }

    fn reject_compose_only(&self, root: &Map<String, Value>) -> Result<(), ProfileError> {
        self.reject_present(
            root,
            &["service", "runServices"],
            "property is only valid for a Compose scenario",
        )
    }
    fn reject_non_compose_only(&self, root: &Map<String, Value>) -> Result<(), ProfileError> {
        self.reject_present(
            root,
            &["workspaceMount", "appPort", "runArgs"],
            "property is not supported for a Compose scenario",
        )
    }

    fn features(
        &self,
        value: Option<&Value>,
    ) -> Result<BTreeMap<String, RawFeature>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        let object = self.object(value, "$.features")?;
        let mut result = BTreeMap::new();
        for (reference, options) in object {
            let feature_path = keyed("$.features", reference);
            let source = self.feature_source(reference, &feature_path, options)?;
            let options_object = self.object(options, &feature_path)?;
            let mut typed = BTreeMap::new();
            for (name, value) in options_object {
                let option_path = path(&feature_path, name);
                let option = match value {
                    Value::Bool(value) => FeatureOptionValue::Boolean(*value),
                    Value::String(value) => FeatureOptionValue::String(value.clone()),
                    _ => {
                        return Err(self.type_error(
                            &option_path,
                            value,
                            "Feature option must be a boolean or string",
                        ));
                    }
                };
                typed.insert(name.clone(), option);
            }
            result.insert(
                reference.clone(),
                RawFeature {
                    source,
                    options: typed,
                },
            );
        }
        Ok(result)
    }

    fn feature_source(
        &self,
        reference: &str,
        property_path: &str,
        value: &Value,
    ) -> Result<FeatureSource, ProfileError> {
        if ["fish", "maven", "gradle", "homebrew", "jupyterlab"].contains(&reference) {
            return Err(self.unsupported(
                property_path,
                value,
                "legacy Feature identifiers are unsupported",
            ));
        }
        if reference.starts_with("http://") {
            return Err(self.unsupported(
                property_path,
                value,
                "insecure HTTP Feature sources are unsupported",
            ));
        }
        if let Some(authority) = reference.strip_prefix("https://") {
            let host = authority.split('/').next().unwrap_or_default();
            if host.is_empty() || host.contains('@') {
                return Err(self.unsupported(
                    property_path,
                    value,
                    "Feature URL credentials and empty hosts are unsupported",
                ));
            }
            return Ok(FeatureSource::Https(reference.to_owned()));
        }
        if reference.starts_with("./") {
            if reference.split('/').any(|part| part == "..") {
                return Err(self.unsupported(
                    property_path,
                    value,
                    "local Feature traversal is unsupported",
                ));
            }
            return Ok(FeatureSource::Local(reference.to_owned()));
        }
        let registry = reference.split('/').next().unwrap_or_default();
        let digest_is_valid = reference.rsplit_once('@').is_none_or(|(_, digest)| {
            digest.strip_prefix("sha256:").is_some_and(|hex| {
                hex.len() == 64
                    && hex
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
        });
        if reference.contains('/')
            && (registry.contains('.') || registry.contains(':') || registry == "localhost")
            && !reference.contains("://")
            && !registry.contains('@')
            && !reference.chars().any(char::is_whitespace)
            && digest_is_valid
        {
            return Ok(FeatureSource::Oci(reference.to_owned()));
        }
        Err(self.unsupported(property_path, value, "Feature source must be fully qualified OCI, unauthenticated HTTPS, or contained `./` local input"))
    }

    fn mounts(&self, value: Option<&Value>) -> Result<Vec<RawMount>, ProfileError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let array = self.array(value, "$.mounts")?;
        let mut result = Vec::new();
        for (index, item) in array.iter().enumerate() {
            let p = index_path("$.mounts", index);
            match item {
                Value::String(value) => result.push(RawMount::String(value.clone())),
                Value::Object(object) => {
                    self.reject_unknown(object, &p, &["type", "source", "target"])?;
                    let kind_value = object.get("type").ok_or_else(|| {
                        self.error(
                            ProfileErrorKind::MissingProperty,
                            &path(&p, "type"),
                            None,
                            "required property is missing",
                        )
                    })?;
                    let kind = match self.string(kind_value, &path(&p, "type"))?.as_str() {
                        "bind" => MountKind::Bind,
                        "volume" => MountKind::Volume,
                        _ => {
                            return Err(self.unsupported(
                                &path(&p, "type"),
                                kind_value,
                                "supported mount types are `bind` and `volume`",
                            ));
                        }
                    };
                    let source = self.optional_string_at(object, &p, "source")?;
                    let target = self.required_string(object, &p, "target")?;
                    result.push(RawMount::Object {
                        kind,
                        source,
                        target,
                    });
                }
                _ => return Err(self.type_error(&p, item, "mount must be a string or object")),
            }
        }
        Ok(result)
    }

    fn app_ports(&self, value: Option<&Value>) -> Result<Vec<AppPort>, ProfileError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let values: Vec<&Value> = match value {
            Value::Array(values) => values.iter().collect(),
            _ => vec![value],
        };
        values
            .into_iter()
            .enumerate()
            .map(|(i, value)| {
                let p = if matches!(self.document.value().get("appPort"), Some(Value::Array(_))) {
                    index_path("$.appPort", i)
                } else {
                    "$.appPort".to_owned()
                };
                match value {
                    Value::String(value) => Ok(AppPort::DockerArgument(value.clone())),
                    _ => self.port_number(value, &p).map(AppPort::Number),
                }
            })
            .collect()
    }

    fn forward_ports(&self, value: Option<&Value>) -> Result<Vec<ForwardPort>, ProfileError> {
        let Some(value) = value else {
            return Ok(Vec::new());
        };
        let mut result = Vec::new();
        let mut requested_ports = std::collections::BTreeSet::new();
        for (i, value) in self.array(value, "$.forwardPorts")?.iter().enumerate() {
            let p = index_path("$.forwardPorts", i);
            let forward = if let Value::String(text) = value {
                let (service, port) = text.rsplit_once(':').ok_or_else(|| {
                    self.invalid(&p, value, "forward port must use `service:port`")
                })?;
                if service.is_empty()
                    || !service
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
                {
                    return Err(self.invalid(&p, value, "invalid Compose service port reference"));
                }
                let port = port
                    .parse::<u16>()
                    .ok()
                    .filter(|port| *port > 0)
                    .ok_or_else(|| self.invalid(&p, value, "port must be between 1 and 65535"))?;
                ForwardPort::Service {
                    service: service.to_owned(),
                    port,
                }
            } else {
                ForwardPort::Container(self.port_number(value, &p)?)
            };
            let port = match &forward {
                ForwardPort::Container(port) | ForwardPort::Service { port, .. } => *port,
            };
            if !requested_ports.insert(port) {
                return Err(self.invalid(
                    &p,
                    value,
                    "multiple forwards cannot request the same local port",
                ));
            }
            result.push(forward);
        }
        Ok(result)
    }

    fn port_attributes_map(
        &self,
        value: Option<&Value>,
        p: &str,
    ) -> Result<BTreeMap<String, PortAttributes>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        self.object(value, p)?
            .iter()
            .map(|(key, value)| {
                Ok((
                    key.clone(),
                    self.port_attributes(value, &keyed(p, key), true)?,
                ))
            })
            .collect()
    }
    fn port_attributes(
        &self,
        value: &Value,
        p: &str,
        allow_once: bool,
    ) -> Result<PortAttributes, ProfileError> {
        let object = self.object(value, p)?;
        self.reject_unknown(
            object,
            p,
            &[
                "onAutoForward",
                "elevateIfNeeded",
                "label",
                "requireLocalPort",
                "protocol",
            ],
        )?;
        let action_values = if allow_once {
            &[
                "notify",
                "openBrowser",
                "openBrowserOnce",
                "openPreview",
                "silent",
                "ignore",
            ][..]
        } else {
            &["notify", "openBrowser", "openPreview", "silent", "ignore"][..]
        };
        Ok(PortAttributes {
            on_auto_forward: self.enum_optional_at(
                object,
                p,
                "onAutoForward",
                action_values,
                |value| match value {
                    "notify" => AutoForwardAction::Notify,
                    "openBrowser" => AutoForwardAction::OpenBrowser,
                    "openBrowserOnce" => AutoForwardAction::OpenBrowserOnce,
                    "openPreview" => AutoForwardAction::OpenPreview,
                    "silent" => AutoForwardAction::Silent,
                    _ => AutoForwardAction::Ignore,
                },
            )?,
            elevate_if_needed: self.optional_bool_at(object, p, "elevateIfNeeded")?,
            label: self.optional_string_at(object, p, "label")?,
            require_local_port: self.optional_bool_at(object, p, "requireLocalPort")?,
            protocol: self.enum_optional_at(object, p, "protocol", &["http", "https"], |v| {
                if v == "http" {
                    PortProtocol::Http
                } else {
                    PortProtocol::Https
                }
            })?,
        })
    }

    fn command(&self, value: Option<&Value>, p: &str) -> Result<Option<RawCommand>, ProfileError> {
        value
            .map(|value| match value {
                Value::String(value) => Ok(RawCommand::Shell(value.clone())),
                Value::Array(_) => self.command_array(value, p).map(RawCommand::Exec),
                Value::Object(object) => {
                    let mut commands = BTreeMap::new();
                    for (key, value) in object {
                        let child = keyed(p, key);
                        let command = match value {
                            Value::String(value) => CommandValue::Shell(value.clone()),
                            Value::Array(_) => {
                                CommandValue::Exec(self.command_array(value, &child)?)
                            }
                            _ => {
                                return Err(self.type_error(
                                    &child,
                                    value,
                                    "parallel command must be a string or string array",
                                ));
                            }
                        };
                        commands.insert(key.clone(), command);
                    }
                    Ok(RawCommand::Parallel(commands))
                }
                _ => Err(self.type_error(
                    p,
                    value,
                    "command must be a string, string array, or object",
                )),
            })
            .transpose()
    }
    fn command_array(&self, value: &Value, p: &str) -> Result<Vec<String>, ProfileError> {
        let values = self.array(value, p)?;
        if values.is_empty() {
            return Err(self.invalid(p, value, "direct command must not be empty"));
        }
        values
            .iter()
            .enumerate()
            .map(|(i, value)| self.string(value, &index_path(p, i)))
            .collect()
    }

    fn host_requirements(&self, value: &Value) -> Result<HostRequirements, ProfileError> {
        let p = "$.hostRequirements";
        let object = self.object(value, p)?;
        self.reject_unknown(object, p, &["cpus", "memory", "storage", "gpu"])?;
        let cpus = self.positive_integer(object.get("cpus"), "$.hostRequirements.cpus")?;
        let memory = self.byte_requirement(object.get("memory"), "$.hostRequirements.memory")?;
        let storage = self.byte_requirement(object.get("storage"), "$.hostRequirements.storage")?;
        let gpu = object.get("gpu").map(|value| self.gpu(value)).transpose()?;
        Ok(HostRequirements {
            cpus,
            memory,
            storage,
            gpu,
        })
    }
    fn gpu(&self, value: &Value) -> Result<GpuRequirement, ProfileError> {
        match value {
            Value::Bool(true) => Ok(GpuRequirement::Required),
            Value::Bool(false) => Ok(GpuRequirement::NotRequired),
            Value::String(v) if v == "optional" => Ok(GpuRequirement::Optional),
            Value::Object(object) => {
                self.reject_unknown(object, "$.hostRequirements.gpu", &["cores", "memory"])?;
                Ok(GpuRequirement::Detailed {
                    cores: self
                        .positive_integer(object.get("cores"), "$.hostRequirements.gpu.cores")?,
                    memory: self
                        .byte_requirement(object.get("memory"), "$.hostRequirements.gpu.memory")?,
                })
            }
            _ => Err(self.unsupported(
                "$.hostRequirements.gpu",
                value,
                "GPU must be true, false, `optional`, or a detailed requirement",
            )),
        }
    }

    fn customizations(
        &self,
        value: Option<&Value>,
    ) -> Result<BTreeMap<String, Value>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        self.object(value, "$.customizations")?
            .iter()
            .map(|(tool, value)| {
                if value.is_object() {
                    Ok((tool.clone(), value.clone()))
                } else {
                    Err(self.type_error(
                        &keyed("$.customizations", tool),
                        value,
                        "tool customization must be an object",
                    ))
                }
            })
            .collect()
    }
    fn secrets(
        &self,
        value: Option<&Value>,
    ) -> Result<BTreeMap<String, SecretMetadata>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        let object = self.object(value, "$.secrets")?;
        let mut result = BTreeMap::new();
        for (name, value) in object {
            let p = keyed("$.secrets", name);
            if !valid_env_name(name) {
                return Err(self.invalid(
                    &p,
                    &Value::String(name.clone()),
                    "secret name must be an environment variable name",
                ));
            }
            let metadata = self.object(value, &p)?;
            self.reject_unknown(metadata, &p, &["description", "documentationUrl"])?;
            result.insert(
                name.clone(),
                SecretMetadata {
                    description: self.optional_string_at(metadata, &p, "description")?,
                    documentation_url: self.optional_string_at(metadata, &p, "documentationUrl")?,
                },
            );
        }
        Ok(result)
    }

    fn reject_unknown(
        &self,
        object: &Map<String, Value>,
        parent: &str,
        allowed: &[&str],
    ) -> Result<(), ProfileError> {
        for (key, value) in object {
            if !allowed.contains(&key.as_str()) {
                return Err(self.error(
                    ProfileErrorKind::UnknownProperty,
                    &path(parent, key),
                    Some(value),
                    "unknown behavioral property",
                ));
            }
        }
        Ok(())
    }
    fn reject_present(
        &self,
        root: &Map<String, Value>,
        names: &[&str],
        message: &str,
    ) -> Result<(), ProfileError> {
        for name in names {
            if let Some(value) = root.get(*name) {
                return Err(self.error(
                    ProfileErrorKind::UnsupportedValue,
                    &path("$", name),
                    Some(value),
                    message,
                ));
            }
        }
        Ok(())
    }
    fn object<'a>(
        &self,
        value: &'a Value,
        p: &str,
    ) -> Result<&'a Map<String, Value>, ProfileError> {
        value
            .as_object()
            .ok_or_else(|| self.type_error(p, value, "expected an object"))
    }
    fn array<'a>(&self, value: &'a Value, p: &str) -> Result<&'a Vec<Value>, ProfileError> {
        value
            .as_array()
            .ok_or_else(|| self.type_error(p, value, "expected an array"))
    }
    fn string(&self, value: &Value, p: &str) -> Result<String, ProfileError> {
        value
            .as_str()
            .map(ToOwned::to_owned)
            .ok_or_else(|| self.type_error(p, value, "expected a string"))
    }
    fn optional_string(
        &self,
        object: &Map<String, Value>,
        key: &str,
    ) -> Result<Option<String>, ProfileError> {
        self.optional_string_at(object, "$", key)
    }
    fn optional_string_at(
        &self,
        object: &Map<String, Value>,
        parent: &str,
        key: &str,
    ) -> Result<Option<String>, ProfileError> {
        object
            .get(key)
            .map(|value| self.string(value, &path(parent, key)))
            .transpose()
    }
    fn required_string(
        &self,
        object: &Map<String, Value>,
        parent: &str,
        key: &str,
    ) -> Result<String, ProfileError> {
        object
            .get(key)
            .ok_or_else(|| {
                self.error(
                    ProfileErrorKind::MissingProperty,
                    &path(parent, key),
                    None,
                    "required property is missing",
                )
            })
            .and_then(|value| self.string(value, &path(parent, key)))
            .and_then(|value| {
                if value.is_empty() {
                    Err(self.invalid(
                        &path(parent, key),
                        &Value::String(value),
                        "value must not be empty",
                    ))
                } else {
                    Ok(value)
                }
            })
    }
    fn optional_bool(
        &self,
        object: &Map<String, Value>,
        key: &str,
    ) -> Result<Option<bool>, ProfileError> {
        self.optional_bool_at(object, "$", key)
    }
    fn optional_bool_at(
        &self,
        object: &Map<String, Value>,
        parent: &str,
        key: &str,
    ) -> Result<Option<bool>, ProfileError> {
        object
            .get(key)
            .map(|value| {
                value
                    .as_bool()
                    .ok_or_else(|| self.type_error(&path(parent, key), value, "expected a boolean"))
            })
            .transpose()
    }
    fn string_array(
        &self,
        value: Option<&Value>,
        p: &str,
    ) -> Result<Option<Vec<String>>, ProfileError> {
        value
            .map(|value| {
                self.array(value, p)?
                    .iter()
                    .enumerate()
                    .map(|(i, value)| self.string(value, &index_path(p, i)))
                    .collect()
            })
            .transpose()
    }
    fn one_or_many_strings(&self, value: &Value, p: &str) -> Result<Vec<String>, ProfileError> {
        match value {
            Value::String(value) => Ok(vec![value.clone()]),
            Value::Array(values) => values
                .iter()
                .enumerate()
                .map(|(index, value)| self.string(value, &index_path(p, index)))
                .collect(),
            _ => Err(self.type_error(p, value, "expected a string or string array")),
        }
    }
    fn string_or_array(&self, value: Option<&Value>, p: &str) -> Result<Vec<String>, ProfileError> {
        value
            .map(|value| self.one_or_many_strings(value, p))
            .transpose()
            .map(Option::unwrap_or_default)
    }
    fn string_map(
        &self,
        value: Option<&Value>,
        p: &str,
        _nullable: bool,
    ) -> Result<BTreeMap<String, String>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        self.object(value, p)?
            .iter()
            .map(|(key, value)| Ok((key.clone(), self.string(value, &keyed(p, key))?)))
            .collect()
    }
    fn nullable_string_map(
        &self,
        value: Option<&Value>,
        p: &str,
    ) -> Result<BTreeMap<String, Option<String>>, ProfileError> {
        let Some(value) = value else {
            return Ok(BTreeMap::new());
        };
        self.object(value, p)?
            .iter()
            .map(|(key, value)| {
                if value.is_null() {
                    Ok((key.clone(), None))
                } else {
                    Ok((key.clone(), Some(self.string(value, &keyed(p, key))?)))
                }
            })
            .collect()
    }
    fn enum_optional<T>(
        &self,
        object: &Map<String, Value>,
        key: &str,
        allowed: &[&str],
        map: impl FnOnce(&str) -> T + Copy,
    ) -> Result<Option<T>, ProfileError> {
        self.enum_optional_at(object, "$", key, allowed, map)
    }
    fn enum_optional_at<T>(
        &self,
        object: &Map<String, Value>,
        parent: &str,
        key: &str,
        allowed: &[&str],
        map: impl FnOnce(&str) -> T + Copy,
    ) -> Result<Option<T>, ProfileError> {
        let Some(value) = object.get(key) else {
            return Ok(None);
        };
        let p = path(parent, key);
        let text = value
            .as_str()
            .ok_or_else(|| self.type_error(&p, value, "expected a string"))?;
        if !allowed.contains(&text) {
            return Err(self.unsupported(
                &p,
                value,
                &format!("supported values are {}", allowed.join(", ")),
            ));
        }
        Ok(Some(map(text)))
    }
    fn port_number(&self, value: &Value, p: &str) -> Result<u16, ProfileError> {
        value
            .as_u64()
            .and_then(|v| u16::try_from(v).ok())
            .filter(|port| *port > 0)
            .ok_or_else(|| self.invalid(p, value, "port must be an integer between 1 and 65535"))
    }
    fn positive_integer(
        &self,
        value: Option<&Value>,
        p: &str,
    ) -> Result<Option<u64>, ProfileError> {
        value
            .map(|value| {
                value
                    .as_u64()
                    .filter(|v| *v > 0)
                    .ok_or_else(|| self.invalid(p, value, "value must be a positive integer"))
            })
            .transpose()
    }
    fn byte_requirement(
        &self,
        value: Option<&Value>,
        p: &str,
    ) -> Result<Option<String>, ProfileError> {
        value
            .map(|value| {
                let text = value
                    .as_str()
                    .ok_or_else(|| self.type_error(p, value, "expected a byte-size string"))?;
                let digits = text.bytes().take_while(u8::is_ascii_digit).count();
                let suffix = &text[digits..];
                if digits == 0 || !matches!(suffix, "" | "kb" | "mb" | "gb" | "tb") {
                    return Err(self.invalid(
                        p,
                        value,
                        "byte size must match digits followed by optional kb, mb, gb, or tb",
                    ));
                }
                Ok(text.to_owned())
            })
            .transpose()
    }
    fn type_error(&self, p: &str, value: &Value, message: &str) -> ProfileError {
        self.error(ProfileErrorKind::InvalidType, p, Some(value), message)
    }
    fn invalid(&self, p: &str, value: &Value, message: &str) -> ProfileError {
        self.error(ProfileErrorKind::InvalidValue, p, Some(value), message)
    }
    fn unsupported(&self, p: &str, value: &Value, message: &str) -> ProfileError {
        self.error(ProfileErrorKind::UnsupportedValue, p, Some(value), message)
    }
    fn error(
        &self,
        kind: ProfileErrorKind,
        p: &str,
        value: Option<&Value>,
        message: &str,
    ) -> ProfileError {
        ProfileError {
            kind,
            diagnostic: Box::new(Diagnostic {
                file: self.document.path().clone(),
                property_path: p.to_owned(),
                span: self.document.property_span(p).unwrap_or_else(|| {
                    self.document.property_span("$").unwrap_or(SourceSpan {
                        start: 0,
                        end: 0,
                        line: 1,
                        column: 1,
                    })
                }),
                message: message.to_owned(),
            }),
            value: value.cloned(),
        }
    }
}

fn path(parent: &str, key: &str) -> String {
    if key.chars().all(|c| c == '_' || c.is_ascii_alphanumeric()) && !key.is_empty() {
        format!("{parent}.{key}")
    } else {
        keyed(parent, key)
    }
}
fn keyed(parent: &str, key: &str) -> String {
    format!("{parent}[{}]", Value::String(key.to_owned()))
}
fn index_path(parent: &str, index: usize) -> String {
    format!("{parent}[{index}]")
}
fn valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ConfigPath, PROFILE_REVISION, ParseLimits, parse_jsonc};

    fn validate(source: &str) -> Result<RawProfile, ProfileError> {
        let path =
            ConfigPath::parse(".devcontainer/devcontainer.json").expect("valid fixture path");
        let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default())
            .expect("valid JSONC fixture");
        validate_profile(&document)
    }

    #[test]
    fn all_scenario_forms_produce_structurally_valid_models() {
        let cases = [
            r#"{"image":"debian:13"}"#,
            r#"{"build":{"dockerfile":"Dockerfile"}}"#,
            r#"{"dockerComposeFile":["compose.yml"],"service":"app","workspaceFolder":"/work"}"#,
        ];
        for case in cases {
            validate(case).expect("supported scenario should validate");
        }
    }

    #[test]
    fn supported_property_families_validate_together() {
        let profile = validate(r#"{
          "$schema":"https://example.invalid/schema.json", "name":"demo", "image":"debian:13",
          "features":{"ghcr.io/devcontainers/features/git:1":{"version":"latest","strict":true}},
          "overrideFeatureInstallOrder":["ghcr.io/devcontainers/features/git"],
          "workspaceFolder":"/workspaces/demo", "workspaceMount":"source=x,target=/workspaces/demo,type=bind",
          "containerEnv":{"A":"b"}, "remoteEnv":{"A":null}, "containerUser":"vscode", "remoteUser":"vscode",
          "updateRemoteUserUID":true, "userEnvProbe":"loginInteractiveShell", "overrideCommand":true,
          "init":true, "privileged":false, "capAdd":["SYS_PTRACE"], "securityOpt":["seccomp=unconfined"],
          "mounts":[{"type":"volume","source":"cache","target":"/cache"}], "runArgs":["--read-only"],
          "appPort":[3000,"127.0.0.1:8000:8000"], "forwardPorts":[3000,"db:5432"],
          "portsAttributes":{"3000":{"onAutoForward":"openBrowserOnce","protocol":"http"}},
          "otherPortsAttributes":{"onAutoForward":"ignore"},
          "initializeCommand":"echo init", "onCreateCommand":["echo","create"],
          "postCreateCommand":{"a":"one","b":["two"]}, "waitFor":"postCreateCommand",
          "shutdownAction":"stopContainer", "hostRequirements":{"cpus":2,"memory":"4gb","gpu":"optional"},
          "customizations":{"unknown.tool":{"anything":[1,2]}},
          "secrets":{"TOKEN":{"description":"token","documentationUrl":"https://example.invalid"}}
        }"#).expect("all supported families should validate");
        assert!(matches!(profile.scenario, RawScenario::Image(_)));
        assert_eq!(profile.common.features.len(), 1);
    }

    #[test]
    fn deliberate_unsupported_values_report_exact_path_value_and_revision() {
        let cases = [
            (
                r#"{"image":"x","shutdownAction":"stopCompose"}"#,
                "$.shutdownAction",
                Value::String("stopCompose".into()),
            ),
            (
                r#"{"image":"x","features":{"http://example.test/f.tgz":{}}}"#,
                "$.features[\"http://example.test/f.tgz\"]",
                Value::Object(Map::new()),
            ),
            (
                r#"{"dockerFile":"Dockerfile"}"#,
                "$.dockerFile",
                Value::String("Dockerfile".into()),
            ),
        ];
        for (source, expected_path, expected_value) in cases {
            let error = validate(source).expect_err("unsupported input should fail");
            assert_eq!(error.diagnostic.property_path, expected_path);
            assert_eq!(error.value, Some(expected_value));
            assert!(error.to_string().contains(PROFILE_REVISION));
        }
    }

    #[test]
    fn scenario_exclusivity_and_required_compose_fields_are_enforced() {
        let conflict = validate(r#"{"image":"x","build":{"dockerfile":"Dockerfile"}}"#)
            .expect_err("scenario conflict");
        assert_eq!(conflict.kind, ProfileErrorKind::ScenarioConflict);
        let missing = validate(r#"{"dockerComposeFile":"compose.yml","service":"app"}"#)
            .expect_err("missing workspace folder");
        assert_eq!(missing.diagnostic.property_path, "$.workspaceFolder");
    }

    #[test]
    fn capability_report_is_deterministic_and_covers_the_allowlist() {
        let first = capability_report();
        let second = capability_report();
        assert_eq!(first, second);
        assert_eq!(
            capability_report_json().expect("report should serialize"),
            include_str!("../tests/snapshots/capability-report.json").trim_end()
        );
        let listed: Vec<_> = first
            .iter()
            .flat_map(|family| family.properties.iter().copied())
            .collect();
        assert_eq!(listed.len(), 39);
        let mut unique = listed.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(listed.len(), unique.len());
    }
}
