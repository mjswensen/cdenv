//! Host coordination for container-only effective environment snapshots.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use cdenv_core::{ContainerId, GenerationId};
use cdenv_devcontainer::{DeferredSegment, EnvironmentPlan, UserEnvProbe};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent_provisioning::{
    AgentCommandOutput, AgentProvisionTransportError, AgentProvisioningEngine,
    AgentProvisioningFacts,
};
use crate::{CancellationToken, ExecCommand};

const MAXIMUM_CAPTURE_RESULT_BYTES: usize = 64 * 1024;

/// Inputs for one generation-scoped effective environment capture.
pub struct AgentEnvironmentRequest<'a> {
    /// Independently verified running container.
    pub container: &'a ContainerId,
    /// Effective Dev Container user used for the agent Exec.
    pub remote_user: &'a str,
    /// Verified final agent and selected-user identity.
    pub provisioned: &'a AgentProvisioningFacts,
    /// Active generation owning the container-side snapshot.
    pub generation: GenerationId,
    /// Container checkout folder, which state must not enter.
    pub checkout: &'a str,
    /// Planned remote environment templates.
    pub environment: &'a EnvironmentPlan,
    /// Configured selected-user shell probe.
    pub probe: UserEnvProbe,
}

/// Initial snapshot available to readiness lifecycle work.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadinessEnvironment {
    container: ContainerId,
    generation: GenerationId,
    snapshot_path: String,
    entries: usize,
}

impl ReadinessEnvironment {
    /// Borrows the container-side path passed to lifecycle agent processes.
    #[must_use]
    pub fn snapshot_path(&self) -> &str {
        &self.snapshot_path
    }

    /// Returns the number of captured entries without exposing them.
    #[must_use]
    pub const fn entries(&self) -> usize {
        self.entries
    }
}

/// Recaptured snapshot safe to use for newly established SSH transports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshEnvironment {
    snapshot_path: String,
    entries: usize,
}

impl SshEnvironment {
    /// Borrows the container-side path passed to the SSH agent process.
    #[must_use]
    pub fn snapshot_path(&self) -> &str {
        &self.snapshot_path
    }

    /// Returns the number of captured entries without exposing them.
    #[must_use]
    pub const fn entries(&self) -> usize {
        self.entries
    }
}

/// Effective environment transport or validation failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum AgentEnvironmentError {
    /// A user, checkout, home, agent, or generation input was unsafe.
    #[error("invalid effective environment capture {field}")]
    InvalidRequest {
        /// Safe field name.
        field: &'static str,
    },
    /// The transient in-memory request could not be encoded.
    #[error("cannot encode effective environment capture request")]
    EncodeRequest,
    /// Docker Exec transport failed. Agent diagnostics are value-free by contract.
    #[error("effective environment capture failed: {source}")]
    Transport {
        /// Preserved transport failure.
        #[source]
        source: AgentProvisionTransportError,
    },
    /// Successful output was malformed, oversized, or included stderr.
    #[error("effective environment capture returned invalid machine output")]
    InvalidOutput,
    /// SSH recapture did not follow the initial capture for this generation.
    #[error("SSH environment recapture does not match the readiness generation")]
    GenerationMismatch,
}

/// Coordinator that captures once for readiness and replaces it after readiness for SSH.
pub struct AgentEnvironmentCapturer<E> {
    engine: E,
}

impl<E> AgentEnvironmentCapturer<E> {
    /// Constructs a capturer from the same narrow Docker Exec seam used by provisioning.
    #[must_use]
    pub const fn new(engine: E) -> Self {
        Self { engine }
    }
}

impl<E: AgentProvisioningEngine> AgentEnvironmentCapturer<E> {
    /// Captures the selected-user environment before readiness lifecycle work.
    ///
    /// # Errors
    ///
    /// Returns request, encoding, Docker transport, or machine-output errors.
    pub async fn capture_for_readiness(
        &self,
        request: &AgentEnvironmentRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ReadinessEnvironment, AgentEnvironmentError> {
        let result = self.capture(request, cancellation).await?;
        Ok(ReadinessEnvironment {
            container: request.container.clone(),
            generation: request.generation,
            snapshot_path: result.snapshot_path,
            entries: result.entries,
        })
    }

    /// Atomically recaptures after the selected readiness stage for new SSH transports.
    ///
    /// The readiness token prevents accidentally publishing a first/pre-readiness snapshot as the
    /// SSH environment.
    ///
    /// # Errors
    ///
    /// Returns a generation mismatch or request, encoding, Docker transport, or output error.
    pub async fn recapture_for_ssh(
        &self,
        request: &AgentEnvironmentRequest<'_>,
        readiness: &ReadinessEnvironment,
        cancellation: &CancellationToken,
    ) -> Result<SshEnvironment, AgentEnvironmentError> {
        if readiness.container != *request.container || readiness.generation != request.generation {
            return Err(AgentEnvironmentError::GenerationMismatch);
        }
        let result = self.capture(request, cancellation).await?;
        if result.snapshot_path != readiness.snapshot_path {
            return Err(AgentEnvironmentError::GenerationMismatch);
        }
        Ok(SshEnvironment {
            snapshot_path: result.snapshot_path,
            entries: result.entries,
        })
    }

    async fn capture(
        &self,
        request: &AgentEnvironmentRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<CaptureOutput, AgentEnvironmentError> {
        let state_directory = validate_request(request)?;
        let remote_environment = request
            .environment
            .remote()
            .iter()
            .map(|(name, value)| {
                let value = value.as_ref().map(|value| {
                    value
                        .segments()
                        .map(|segment| match segment {
                            DeferredSegment::Literal(value) => WireSegment::Literal { value },
                            DeferredSegment::ContainerEnvironment { name, default } => {
                                WireSegment::ContainerEnvironment { name, default }
                            }
                        })
                        .collect()
                });
                (name.as_str(), value)
            })
            .collect();
        let wire = CaptureRequest {
            generation: request.generation.to_string(),
            state_directory: &state_directory,
            probe: request.probe,
            remote_environment,
        };
        let input = serde_json::to_vec(&wire).map_err(|_| AgentEnvironmentError::EncodeRequest)?;
        let command = [
            request.provisioned.agent_path.clone(),
            "capture-environment".to_owned(),
        ];
        let output = self
            .engine
            .execute(
                ExecCommand {
                    container: request.container,
                    command: &command,
                    user: Some(request.remote_user),
                    working_directory: None,
                    environment: &[],
                },
                &input,
                cancellation,
            )
            .await
            .map_err(|source| AgentEnvironmentError::Transport { source })?;
        parse_output(&output)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CaptureRequest<'a> {
    generation: String,
    state_directory: &'a str,
    probe: UserEnvProbe,
    remote_environment: BTreeMap<&'a str, Option<Vec<WireSegment<'a>>>>,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum WireSegment<'a> {
    Literal { value: &'a str },
    ContainerEnvironment { name: &'a str, default: &'a str },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaptureOutput {
    snapshot_path: String,
    entries: usize,
}

fn validate_request(
    request: &AgentEnvironmentRequest<'_>,
) -> Result<String, AgentEnvironmentError> {
    if request.remote_user.is_empty() || request.remote_user.contains('\0') {
        return Err(AgentEnvironmentError::InvalidRequest {
            field: "remote user",
        });
    }
    let home = safe_absolute(&request.provisioned.identity.home).ok_or(
        AgentEnvironmentError::InvalidRequest {
            field: "selected-user home",
        },
    )?;
    let checkout = safe_absolute(request.checkout)
        .ok_or(AgentEnvironmentError::InvalidRequest { field: "checkout" })?;
    let agent = safe_absolute(&request.provisioned.agent_path).ok_or(
        AgentEnvironmentError::InvalidRequest {
            field: "agent path",
        },
    )?;
    let state = home.join(".cdenv/environment");
    if state.starts_with(checkout) || agent.starts_with(checkout) {
        return Err(AgentEnvironmentError::InvalidRequest {
            field: "container state location",
        });
    }
    Ok(state.display().to_string())
}

fn safe_absolute(value: &str) -> Option<&Path> {
    let path = Path::new(value);
    (path.is_absolute()
        && path != Path::new("/")
        && !value.contains('\0')
        && !path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        }))
    .then_some(path)
}

fn parse_output(output: &AgentCommandOutput) -> Result<CaptureOutput, AgentEnvironmentError> {
    if !output.stderr.is_empty()
        || output.stdout.is_empty()
        || output.stdout.len() > MAXIMUM_CAPTURE_RESULT_BYTES
    {
        return Err(AgentEnvironmentError::InvalidOutput);
    }
    serde_json::from_slice(&output.stdout).map_err(|_| AgentEnvironmentError::InvalidOutput)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use cdenv_core::{AgentBuildId, ProtocolVersion};
    use cdenv_devcontainer::{
        ConfigPath, ParseLimits, RuntimePlanningInputs, ScenarioMetadata, StableIdentityLabels,
        merge_image_metadata, parse_jsonc, plan_runtime, validate_profile,
    };

    use super::*;
    use crate::{AgentProvisionTransportError, RemoteAgentIdentity};

    const CONTAINER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[derive(Clone)]
    struct MemoryEngine {
        inputs: Arc<Mutex<Vec<Vec<u8>>>>,
        outputs: Arc<Mutex<VecDeque<Vec<u8>>>>,
    }

    impl AgentProvisioningEngine for MemoryEngine {
        async fn upload(
            &self,
            _: &ContainerId,
            _: &str,
            _: &[u8],
        ) -> Result<(), AgentProvisionTransportError> {
            panic!("environment capture does not upload or persist a host request")
        }

        async fn execute(
            &self,
            _: ExecCommand<'_>,
            input: &[u8],
            _: &CancellationToken,
        ) -> Result<AgentCommandOutput, AgentProvisionTransportError> {
            self.inputs.lock().expect("inputs").push(input.to_vec());
            Ok(AgentCommandOutput {
                stdout: self
                    .outputs
                    .lock()
                    .expect("outputs")
                    .pop_front()
                    .expect("output"),
                stderr: Vec::new(),
            })
        }
    }

    fn environment_plan() -> cdenv_devcontainer::RuntimePlan {
        let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
        let source = br#"{"image":"example.invalid/base","remoteEnv":{"A":"prefix-${containerEnv:BASE:fallback}","REMOVE":null},"userEnvProbe":"interactiveShell"}"#;
        let document = parse_jsonc(&path, source, ParseLimits::default()).expect("parse");
        let profile = validate_profile(&document).expect("profile");
        let effective = merge_image_metadata(&[], &profile).expect("metadata");
        let local_env = BTreeMap::new();
        let labels = StableIdentityLabels::new("installation", "workspace");
        plan_runtime(
            &profile,
            &effective,
            &RuntimePlanningInputs {
                local_workspace_folder: "/checkout/project",
                local_env: &local_env,
                identity_labels: &labels,
                scenario_metadata: ScenarioMetadata::default(),
                cdenv_owned_targets: &[],
                host_user: None,
            },
        )
        .expect("runtime")
    }

    fn provisioned() -> AgentProvisioningFacts {
        AgentProvisioningFacts {
            identity: RemoteAgentIdentity {
                uid: 1000,
                gid: 1000,
                home: "/home/dev".to_owned(),
                shell: "/bin/sh".to_owned(),
            },
            agent_path: "/usr/local/libexec/cdenv/cdenv-agent".to_owned(),
            build_id: AgentBuildId::parse("build").expect("build"),
            protocol_version: ProtocolVersion::new(1).expect("protocol"),
        }
    }

    #[tokio::test]
    async fn readiness_capture_and_ssh_recapture_use_the_same_generation_snapshot() {
        let inputs = Arc::new(Mutex::new(Vec::new()));
        let output = br#"{"snapshotPath":"/home/dev/.cdenv/environment/.cdenv-environment-7.bin","entries":3}"#.to_vec();
        let engine = MemoryEngine {
            inputs: inputs.clone(),
            outputs: Arc::new(Mutex::new(VecDeque::from([output.clone(), output]))),
        };
        let runtime = environment_plan();
        let provisioned = provisioned();
        let container = ContainerId::parse(CONTAINER_ID).expect("container");
        let request = AgentEnvironmentRequest {
            container: &container,
            remote_user: "dev",
            provisioned: &provisioned,
            generation: GenerationId::new(7).expect("generation"),
            checkout: "/workspaces/project",
            environment: &runtime.environment,
            probe: runtime.user_env_probe,
        };
        let capturer = AgentEnvironmentCapturer::new(engine);

        let readiness = capturer
            .capture_for_readiness(&request, &CancellationToken::default())
            .await
            .expect("readiness capture");
        let ssh = capturer
            .recapture_for_ssh(&request, &readiness, &CancellationToken::default())
            .await
            .expect("SSH recapture");

        assert_eq!(ssh.snapshot_path(), readiness.snapshot_path());
        let requests = inputs.lock().expect("inputs");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|request| {
            let value: serde_json::Value = serde_json::from_slice(request).expect("wire JSON");
            value["probe"] == "interactiveShell"
                && value["remoteEnvironment"]["A"][1]["kind"] == "containerEnvironment"
        }));
    }

    #[test]
    fn capture_types_do_not_expose_effective_values_in_debug_output() {
        let marker = "secret-marker";
        let snapshot = ReadinessEnvironment {
            container: ContainerId::parse(CONTAINER_ID).expect("container"),
            generation: GenerationId::new(1).expect("generation"),
            snapshot_path: "/home/dev/.cdenv/environment/.cdenv-environment-1.bin".to_owned(),
            entries: 1,
        };
        assert!(!format!("{snapshot:?}").contains(marker));
    }
}
