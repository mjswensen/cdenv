//! Production composition for environment lifecycle command paths.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use cdenv_core::{
    ContainerId, ForegroundOperation, ForwardingEndpoint, ForwardingEndpointAssignment,
    GenerationId, ProfileId, ProtocolVersion, TcpPort, WorkspaceName,
};
use cdenv_devcontainer::{
    BuildPlan, ComposeIdentity, ComposePlan, ComposePlanningInputs, ComposePrimaryOverride,
    ContainerPath, DeferredSegment, DockerOptionPlanningInputs, HostCapabilities,
    HostSubstitutionInputs, ImmutablePlanInputs, LifecycleCommand, LifecycleProcess,
    LifecycleStage as DevLifecycleStage, Measured, ParseLimits, RawScenario, RuntimePlanningInputs,
    ScenarioMetadata, StableIdentityLabels, UidUpdateIntent, UnknownMeasurement,
    compose_project_name, merge_image_metadata, plan_compose, plan_docker_options, plan_immutable,
    plan_lifecycle, plan_ports, plan_runtime, resolve_features, validate_profile,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::agent_provisioning::AgentProvisioningEngine;
use crate::bollard::{COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL};
use crate::image_orchestration::ImageContainerMatchState;
use crate::lifecycle_orchestration::HostLifecycle;
use crate::{
    ActiveForwarding, ActiveGeneration, ActiveScenario, AgentArtifactIdentity,
    AgentArtifactProvider, AgentEnvironmentCapturer, AgentEnvironmentRequest,
    AgentProvisionRequest, AgentProvisioner, BOLLARD_CONTROL_TIMEOUT, BollardAdapter,
    CancellationToken, CdenvRoot, ComposeAdapter, ComposeBaseRequest, ComposeCleanupKind,
    ComposeLifecycleFacts, ComposeLifecycleOrchestrator, ComposeProject, ComposeRebuildError,
    ComposeRebuildOrchestrator, ComposeRebuildRequest, ComposeRebuildRuntime, ComposeServiceHealth,
    ComposeUpRequest, CreateComposeRequest, DeclaredForward, DesiredForwardingPlan,
    DockerBuildContext, DockerBuildRequest, DockerCommandProbe, DockerEndpoint,
    DockerResourceIdentity, DockerfileInput, DownRequest, EnvironmentReconciler,
    EnvironmentReconciliationRequest, EnvironmentStopOutcome, EnvironmentTransition,
    FeatureSourcePolicy, FeatureSourceResolver, FingerprintKeyState, ForwardingSupervisorStop,
    GeneratedFeature, GeneratedImagePlan, GeneratedUidGidUpdate, GitAdapter, HostLifecycleExecutor,
    ImageContainerBuildRequest, ImageContainerCreateRequest, ImageContainerError,
    ImageContainerOrchestrator, ImageId, Installation, LifecycleCheckpoint, LifecycleInput,
    LifecycleRunnerStop, LifecycleStage, LifecycleStopOutcome, ManagedEnvironmentStop,
    ObservedComposeService, OperationState, PlanFingerprintCategory, PlanFingerprints,
    PreparedDesiredPlan, PreparedImage, PreparedRebuildPlan, ProcessDockerEnvironment,
    ProcessRunner, ProvisionedState, ReadyEnvironment, RebuildCleanupRequest, RebuildEnvironment,
    RebuildPlanner, RebuildRequest, RebuildRollbackRequest, ReconciliationPlanner,
    ReconciliationRequest, RecordedComposeRequest, RepoRelativeConfigPath, RuntimeReconciliation,
    ScopedForwardingSupervisor, SupervisorClaim, SupervisorForward, SupervisorManifest,
    UidGidMutation, WorkspaceState, WorkspaceStateError, discover_and_read_config, down_workspace,
    ensure_workspace_ssh_identity, load_supervisor_state, load_workspace_ssh_assets,
    load_workspace_state, rebuild_workspace, reconcile_workspace, start_detached_supervisor,
    stop_supervisor, supervisor_control_token, supervisor_status,
};

const PROFILE: &str = "cdenv-devcontainer-v1";

#[derive(Debug)]
struct WorkflowMessage(String);

impl fmt::Display for WorkflowMessage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for WorkflowMessage {}

fn message(error: impl fmt::Display) -> WorkflowMessage {
    WorkflowMessage(error.to_string())
}

/// Failure returned by the production lifecycle entry point.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProductionWorkflowError {
    /// The requested workspace has not been created.
    #[error("workspace `{0}` does not exist; run `cdenv create` first")]
    MissingWorkspace(WorkspaceName),
    /// Existing state could not be loaded.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// Production setup or reconciliation failed.
    #[error("{0}")]
    Workflow(String),
}

struct ProductionPlan {
    checkout: PathBuf,
    profile: ProfileId,
    runtime: cdenv_devcontainer::RuntimePlan,
    docker: cdenv_devcontainer::DockerOptionsPlan,
    ports: cdenv_devcontainer::PortPlan,
    lifecycle: cdenv_devcontainer::LifecyclePlan,
    scenario: ActiveScenario,
    compose: Option<ProductionComposePlan>,
    generated: Option<GeneratedImagePlan>,
    base_build: Option<cdenv_devcontainer::DockerfileBuildPlan>,
    feature_digests: BTreeMap<String, String>,
    host_requirements: Option<cdenv_devcontainer::HostRequirements>,
    capabilities: HostCapabilities,
    owned_targets: Vec<ContainerPath>,
    generation: GenerationId,
}

struct ProductionComposePlan {
    files: Vec<PathBuf>,
    plan: ComposePlan,
    primary_service: String,
    primary_has_build: bool,
    base_tag: String,
}

struct ProductionPlanner<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    cancellation: &'a CancellationToken,
    replacement: bool,
}

impl ReconciliationPlanner for ProductionPlanner<'_> {
    type Plan = ProductionPlan;
    type PlanError = WorkflowMessage;
    type PreflightError = WorkflowMessage;

    #[expect(
        clippy::too_many_lines,
        reason = "production planning keeps the security-sensitive adapter and fingerprint order visible"
    )]
    fn prepare(
        &self,
        checkout: &Path,
        explicit: Option<&Path>,
        current: &WorkspaceState,
        _feature_policy: FeatureSourcePolicy,
    ) -> Result<PreparedDesiredPlan<Self::Plan>, Self::PlanError> {
        let selected = explicit.map(Path::to_path_buf).or_else(|| {
            current
                .active()
                .is_some()
                .then(|| PathBuf::from(current.desired_devcontainer_config().as_str()))
        });
        let source = discover_and_read_config(checkout, selected.as_deref())
            .map_err(message)?
            .ok_or_else(|| message("no Dev Container configuration found"))?;
        let document =
            cdenv_devcontainer::parse_jsonc(source.path(), source.bytes(), ParseLimits::default())
                .map_err(message)?;
        let profile = validate_profile(&document).map_err(message)?;
        let configuration_directory = checkout
            .join(source.path().as_str())
            .parent()
            .ok_or_else(|| message("configuration has no parent directory"))?
            .to_path_buf();
        let (resolved_features, generated_features, feature_digests) =
            resolve_feature_plan(self.root, &profile, &configuration_directory)?;
        let feature_metadata = resolved_features
            .installation_order
            .iter()
            .map(cdenv_devcontainer::ImageMetadata::from_feature)
            .collect::<Vec<_>>();
        let effective = merge_image_metadata(&feature_metadata, &profile).map_err(message)?;
        let checkout_text = checkout
            .to_str()
            .ok_or_else(|| message("workspace checkout path is not UTF-8"))?;
        let local_env = std::env::vars().collect::<BTreeMap<_, _>>();
        let labels =
            StableIdentityLabels::new(current.installation_id().as_str(), current.name().as_str());
        let profile_id = ProfileId::parse(PROFILE).map_err(message)?;
        let generation = if self.replacement {
            let next = current
                .active()
                .map_or(1, |active| active.generation().get().saturating_add(1));
            GenerationId::new(next).map_err(message)?
        } else {
            current
                .active()
                .map_or_else(|| GenerationId::new(1), |active| Ok(active.generation()))
                .map_err(message)?
        };
        let config_directory_path = configuration_directory;
        let mut compose_input = None;
        let scenario_user = if let RawScenario::Compose(scenario) = &profile.scenario {
            let files = scenario
                .files
                .iter()
                .map(|file| {
                    config_directory_path
                        .join(file)
                        .canonicalize()
                        .map_err(message)
                })
                .collect::<Result<Vec<_>, _>>()?;
            let project_name =
                compose_project_name(current.installation_id().as_str(), current.name().as_str());
            let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
            let adapter = ComposeAdapter::system(
                endpoint,
                ProcessRunner::new(self.root.logs_dir()),
                self.root.temp_dir(),
            );
            let model = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(adapter.resolve_model(
                    ComposeProject {
                        files: &files,
                        project_name: &project_name,
                        working_directory: checkout,
                    },
                    self.cancellation,
                ))
            })
            .map_err(message)?;
            let user = model
                .services
                .get(&scenario.service)
                .and_then(|service| service.user.clone());
            compose_input = Some((files, model, project_name));
            user
        } else {
            None
        };
        let owned_targets = [
            ContainerPath::parse("/usr/local/libexec/cdenv").map_err(message)?,
            ContainerPath::parse("/usr/libexec/cdenv").map_err(message)?,
            ContainerPath::parse("/opt/cdenv").map_err(message)?,
            ContainerPath::parse("/var/lib/cdenv").map_err(message)?,
        ]
        .into_iter()
        .collect::<Vec<_>>();
        let runtime = plan_runtime(
            &profile,
            &effective,
            &RuntimePlanningInputs {
                local_workspace_folder: checkout_text,
                local_env: &local_env,
                identity_labels: &labels,
                scenario_metadata: ScenarioMetadata {
                    container_user: scenario_user.as_deref(),
                },
                cdenv_owned_targets: &owned_targets,
                host_user: Some(cdenv_devcontainer::HostUserIdentity::new(
                    nix::unistd::geteuid().as_raw(),
                    nix::unistd::getegid().as_raw(),
                )),
            },
        )
        .map_err(message)?;
        let substitutions = HostSubstitutionInputs {
            local_workspace_folder: checkout_text,
            container_workspace_folder: runtime.workspace.folder.as_str(),
            local_env: &local_env,
            identity_labels: &labels,
        };
        let config_directory = Path::new(source.path().as_str())
            .parent()
            .and_then(Path::to_str)
            .unwrap_or("");
        let mut docker = plan_docker_options(
            &profile,
            &runtime,
            &DockerOptionPlanningInputs {
                config_directory,
                substitutions: &substitutions,
                cdenv_owned_targets: &owned_targets,
            },
        )
        .map_err(message)?;
        let ports = plan_ports(&profile, &effective).map_err(message)?;
        let lifecycle = plan_lifecycle(&effective, &substitutions).map_err(message)?;
        let generated_tag = format!("cdenv/{}:{generation}", self.workspace);
        let compose_base_tag = format!("cdenv/{}-compose-base:{generation}", self.workspace);
        let uid_update = match &runtime.uid_update {
            UidUpdateIntent::None(_) => None,
            UidUpdateIntent::UpdateNamedUser { user, host } => Some(
                GeneratedUidGidUpdate::from_provider(
                    AgentArtifactProvider::embedded(),
                    host_architecture(),
                    &UidGidMutation {
                        account: user.as_str().to_owned(),
                        current_uid: 1,
                        current_gid: 1,
                        target_uid: host.uid(),
                        target_gid: host.gid(),
                    },
                )
                .map_err(message)?,
            ),
        };
        let mut base_build = None;
        let generated = if generated_features.is_empty() && uid_update.is_none() {
            None
        } else {
            let base = match &profile.scenario {
                RawScenario::Image(image) => image.image.clone(),
                RawScenario::Dockerfile(_) => {
                    let original = std::mem::replace(&mut docker.build, BuildPlan::Compose);
                    let BuildPlan::Dockerfile(original) = original else {
                        return Err(message("internal Dockerfile base planning mismatch"));
                    };
                    base_build = Some(original);
                    format!("cdenv/{}-feature-base:{generation}", self.workspace)
                }
                RawScenario::Compose(_) => {
                    format!("cdenv/{}-compose-base:{generation}", self.workspace)
                }
            };
            let metadata = serde_json::Value::Array(
                resolved_features
                    .installation_order
                    .iter()
                    .map(|feature| serde_json::json!({ "id": feature.metadata.id }))
                    .collect(),
            );
            let generated = GeneratedImagePlan::new_with_uid_update(
                &base,
                &generated_features,
                &metadata,
                &BTreeMap::new(),
                uid_update.as_ref(),
            )
            .map_err(message)?;
            docker.build = BuildPlan::Dockerfile(generated.build_plan().map_err(message)?);
            Some(generated)
        };
        let immutable = plan_immutable(&ImmutablePlanInputs {
            build: &docker.build,
            create_options: &docker.create,
            runtime: &runtime,
            ports: &ports,
            features: &resolved_features.installation_order,
            lifecycle: &lifecycle,
            entrypoints: &effective.entrypoints,
        });
        let installation = Installation::open_or_create(self.root).map_err(message)?;
        let key = match installation.fingerprint_key() {
            FingerprintKeyState::Available(key) => key,
            FingerprintKeyState::Unknown(reason) => {
                return Err(message(format!(
                    "installation fingerprint key is unavailable: {reason:?}"
                )));
            }
        };
        let fingerprints = immutable.fingerprint_with(|category, bytes| {
            let category = match category {
                cdenv_devcontainer::PlanCategory::Build => PlanFingerprintCategory::Build,
                cdenv_devcontainer::PlanCategory::Create => PlanFingerprintCategory::Create,
                cdenv_devcontainer::PlanCategory::Runtime => PlanFingerprintCategory::Runtime,
                cdenv_devcontainer::PlanCategory::Lifecycle => PlanFingerprintCategory::Lifecycle,
            };
            key.digest_plan(category, [bytes])
        });
        let (scenario, compose) = match (&profile.scenario, compose_input) {
            (RawScenario::Image(_), None) => (
                if generated.is_some() {
                    ActiveScenario::Dockerfile
                } else {
                    ActiveScenario::Image
                },
                None,
            ),
            (RawScenario::Dockerfile(_), None) => (ActiveScenario::Dockerfile, None),
            (RawScenario::Compose(compose_scenario), Some((files, model, _))) => {
                let base_tag = compose_base_tag;
                let final_tag = generated
                    .as_ref()
                    .map_or(base_tag.as_str(), |_| generated_tag.as_str());
                let compose_plan = plan_compose(
                    compose_scenario,
                    &model,
                    &ComposePlanningInputs {
                        identity: ComposeIdentity {
                            installation: current.installation_id().as_str(),
                            workspace: current.name().as_str(),
                            generation: &generation.to_string(),
                            profile: profile_id.as_str(),
                        },
                        runtime: &runtime,
                        ports: &ports,
                        primary_override: ComposePrimaryOverride {
                            image: Some(final_tag),
                            entrypoint: None,
                            command: None,
                        },
                    },
                )
                .map_err(message)?;
                let primary_has_build = model.services[&compose_scenario.service].has_build;
                let active_scenario = ActiveScenario::Compose {
                    project: compose_plan.project_name().to_owned(),
                    managed_services: compose_plan.managed_services().to_vec(),
                };
                (
                    active_scenario,
                    Some(ProductionComposePlan {
                        files,
                        plan: compose_plan,
                        primary_service: compose_scenario.service.clone(),
                        primary_has_build,
                        base_tag,
                    }),
                )
            }
            _ => return Err(message("internal scenario planning mismatch")),
        };
        let desired_config =
            crate::DesiredConfigPath::parse(source.path().as_str()).map_err(message)?;
        Ok(PreparedDesiredPlan {
            profile: profile_id.clone(),
            config: desired_config,
            fingerprints: PlanFingerprints::new(
                fingerprints.build,
                fingerprints.create,
                fingerprints.runtime,
            ),
            plan: ProductionPlan {
                checkout: checkout.to_path_buf(),
                profile: profile_id,
                runtime,
                docker,
                ports,
                lifecycle,
                scenario,
                compose,
                generated,
                base_build,
                feature_digests,
                host_requirements: effective.host_requirements,
                capabilities: unknown_host_capabilities(),
                owned_targets,
                generation,
            },
        })
    }

    async fn preflight(
        &self,
        desired: &PreparedDesiredPlan<Self::Plan>,
        _feature_policy: FeatureSourcePolicy,
    ) -> Result<(), Self::PreflightError> {
        if self.cancellation.is_cancelled() {
            return Err(message("environment reconciliation was cancelled"));
        }
        let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
        let runner = ProcessRunner::new(self.root.logs_dir());
        let probe = DockerCommandProbe::system(endpoint, runner);
        if desired.plan.compose.is_some() {
            probe
                .probe_compose(&desired.plan.checkout, self.cancellation)
                .await
                .map_err(message)?;
        } else {
            probe
                .probe_docker(&desired.plan.checkout, self.cancellation)
                .await
                .map_err(message)?;
        }
        Ok(())
    }
}

impl RebuildPlanner for ProductionPlanner<'_> {
    type Plan = ProductionPlan;
    type PlanError = WorkflowMessage;
    type PreflightError = WorkflowMessage;

    fn prepare(
        &self,
        checkout: &Path,
        explicit: Option<&Path>,
        current: &WorkspaceState,
    ) -> Result<PreparedRebuildPlan<Self::Plan>, Self::PlanError> {
        let desired = ReconciliationPlanner::prepare(
            self,
            checkout,
            explicit,
            current,
            FeatureSourcePolicy::FrozenOffline,
        )?;
        Ok(PreparedRebuildPlan {
            profile: desired.profile,
            config: desired.config,
            fingerprints: desired.fingerprints,
            plan: desired.plan,
        })
    }

    async fn validate_frozen(
        &self,
        desired: &PreparedRebuildPlan<Self::Plan>,
    ) -> Result<(), Self::PreflightError> {
        if self.cancellation.is_cancelled() {
            return Err(message("environment reconciliation was cancelled"));
        }
        let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
        let probe = DockerCommandProbe::system(endpoint, ProcessRunner::new(self.root.logs_dir()));
        if desired.plan.compose.is_some() {
            probe
                .probe_compose(&desired.plan.checkout, self.cancellation)
                .await
                .map(|_| ())
                .map_err(message)
        } else {
            probe
                .probe_docker(&desired.plan.checkout, self.cancellation)
                .await
                .map(|_| ())
                .map_err(message)
        }
    }
}

struct ProductionEnvironment<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    installation: cdenv_core::InstallationId,
    cancellation: &'a CancellationToken,
    no_cache: bool,
    prepared_image: Option<&'a PreparedImage>,
}

impl EnvironmentReconciler<ProductionPlan> for ProductionEnvironment<'_> {
    type Error = WorkflowMessage;

    #[expect(
        clippy::too_many_lines,
        reason = "scenario selection and exact mutation boundaries remain visible in one transaction"
    )]
    async fn reconcile(
        &self,
        request: EnvironmentReconciliationRequest<'_, ProductionPlan>,
    ) -> Result<ReadyEnvironment, Self::Error> {
        let plan = &request.desired.plan;
        if request.drift.create {
            return Err(message(
                "container-create configuration drift requires `cdenv rebuild`; no implicit replacement was attempted",
            ));
        }
        HostLifecycleExecutor
            .initialize(
                &plan.lifecycle.initialize,
                &plan.checkout,
                LifecycleInput::Inherit,
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
        let connector = endpoint
            .bollard_connector(BOLLARD_CONTROL_TIMEOUT)
            .map_err(message)?;
        let engine = BollardAdapter::from_connector(&connector);
        let generation = request
            .active
            .map_or(plan.generation, ActiveGeneration::generation);
        let identity = DockerResourceIdentity {
            installation: &self.installation,
            workspace: self.workspace,
            generation,
            profile: &plan.profile,
        };
        let (facts, transition) = if let Some(compose) = &plan.compose {
            let adapter = ComposeAdapter::system(
                endpoint.clone(),
                ProcessRunner::new(self.root.logs_dir()),
                self.root.temp_dir(),
            );
            let orchestrator = ComposeLifecycleOrchestrator::new(adapter.clone(), engine.clone());
            let project = ComposeProject {
                files: &compose.files,
                project_name: compose.plan.project_name(),
                working_directory: &plan.checkout,
            };
            if let Some(active) = request.active {
                let (project_name, managed) = active
                    .scenario()
                    .compose()
                    .ok_or_else(|| message("active scenario differs from desired Compose plan"))?;
                let was_running = engine
                    .inspect_container(active.container_id())
                    .await
                    .map_err(message)?
                    .running;
                let resumed = orchestrator
                    .resume(
                        &RecordedComposeRequest {
                            identity,
                            project: project_name,
                            primary_service: &compose.primary_service,
                            primary: active.container_id(),
                            managed_services: managed,
                        },
                        self.cancellation,
                    )
                    .await
                    .map_err(message)?;
                let image = ImageId::parse(active.image_id()).map_err(message)?;
                let architecture = engine
                    .inspect_image(image.as_str())
                    .await
                    .map_err(message)?
                    .architecture;
                (
                    EnvironmentFacts {
                        container: resumed.primary,
                        image,
                        architecture,
                        generation,
                    },
                    if was_running {
                        EnvironmentTransition::AlreadyRunning
                    } else {
                        EnvironmentTransition::Started
                    },
                )
            } else {
                let base = adapter
                    .prepare_base(
                        &ComposeBaseRequest {
                            project,
                            service: &compose.primary_service,
                            has_build: compose.primary_has_build,
                            base_tag: &compose.base_tag,
                            no_cache: self.no_cache,
                        },
                        self.cancellation,
                    )
                    .await
                    .map_err(message)?;
                let final_image = if let Some(generated) = &plan.generated {
                    let BuildPlan::Dockerfile(generated_build) = &plan.docker.build else {
                        return Err(message("internal generated Compose plan mismatch"));
                    };
                    let generated_context =
                        DockerBuildContext::Generated(generated.files().to_vec());
                    crate::DockerCliAdapter::system(
                        endpoint.clone(),
                        ProcessRunner::new(self.root.logs_dir()),
                        self.root.temp_dir(),
                    )
                    .build(
                        &DockerBuildRequest {
                            plan: generated_build,
                            checkout: &plan.checkout,
                            tag: &format!("cdenv/{}:{generation}", self.workspace),
                            identity,
                            context: &generated_context,
                            dockerfile: DockerfileInput::Generated(generated.dockerfile()),
                            no_cache: self.no_cache,
                        },
                        self.cancellation,
                    )
                    .await
                    .map_err(message)?
                    .image_id
                } else {
                    base.image_id
                };
                let created = orchestrator
                    .create_or_rebuild(
                        &CreateComposeRequest {
                            up: ComposeUpRequest {
                                project,
                                plan: &compose.plan,
                                force_recreate: true,
                            },
                            identity,
                            primary_image: &final_image,
                            ports: &plan.ports,
                        },
                        self.cancellation,
                    )
                    .await
                    .map_err(message)?;
                let architecture = engine
                    .inspect_image(final_image.as_str())
                    .await
                    .map_err(message)?
                    .architecture;
                (
                    EnvironmentFacts {
                        container: created.primary,
                        image: final_image,
                        architecture,
                        generation,
                    },
                    EnvironmentTransition::Created,
                )
            }
        } else {
            let docker = crate::DockerCliAdapter::system(
                endpoint,
                ProcessRunner::new(self.root.logs_dir()),
                self.root.temp_dir(),
            );
            if request.active.is_none()
                && let Some(base_build) = &plan.base_build
            {
                let context = DockerBuildContext::Repository;
                docker
                    .build(
                        &DockerBuildRequest {
                            plan: base_build,
                            checkout: &plan.checkout,
                            tag: &format!("cdenv/{}-feature-base:{generation}", self.workspace),
                            identity,
                            context: &context,
                            dockerfile: DockerfileInput::Repository,
                            no_cache: self.no_cache,
                        },
                        self.cancellation,
                    )
                    .await
                    .map_err(message)?;
            }
            let orchestrator = ImageContainerOrchestrator::new(docker, engine.clone());
            let name = format!("cdenv-{}-{generation}", self.workspace);
            let (facts, transition) = if let Some(active) = request.active {
                let image = ImageId::parse(active.image_id()).map_err(message)?;
                let recorded = crate::RecordedContainerRequest {
                    container: active.container_id(),
                    container_name: &name,
                    image: &image,
                    identity,
                    runtime: &plan.runtime,
                    ports: &plan.ports,
                };
                match orchestrator
                    .verify_recorded(&recorded, self.cancellation)
                    .await
                {
                    Ok(facts) => (facts, EnvironmentTransition::AlreadyRunning),
                    Err(ImageContainerError::UnsafeContainerState {
                        state: ImageContainerMatchState::RecordedStopped,
                    }) => (
                        orchestrator
                            .restart_recorded(&recorded, self.cancellation)
                            .await
                            .map_err(message)?,
                        EnvironmentTransition::Started,
                    ),
                    Err(error) => return Err(message(error)),
                }
            } else {
                let context = plan.generated.as_ref().map_or_else(
                    || DockerBuildContext::Repository,
                    |generated| DockerBuildContext::Generated(generated.files().to_vec()),
                );
                let dockerfile = plan
                    .generated
                    .as_ref()
                    .map_or(DockerfileInput::Repository, |generated| {
                        DockerfileInput::Generated(generated.dockerfile())
                    });
                let command = if plan.runtime.override_command {
                    vec![
                        "/bin/sh".to_owned(),
                        "-c".to_owned(),
                        "trap 'exit 0' TERM; while sleep 3600; do :; done".to_owned(),
                    ]
                } else {
                    Vec::new()
                };
                let build_tag = format!("cdenv/{}:{generation}", self.workspace);
                let create = ImageContainerCreateRequest {
                    build: &plan.docker.build,
                    checkout: &plan.checkout,
                    build_tag: matches!(plan.docker.build, BuildPlan::Dockerfile(_))
                        .then_some(build_tag.as_str()),
                    build_context: &context,
                    dockerfile,
                    container_name: &name,
                    identity,
                    runtime: &plan.runtime,
                    create_options: &plan.docker.create,
                    ports: &plan.ports,
                    command: &command,
                    no_cache: self.no_cache,
                    cdenv_owned_targets: &plan.owned_targets,
                    host_requirements: plan.host_requirements.as_ref(),
                    host_capabilities: &plan.capabilities,
                    recorded_container: None,
                };
                let facts = if let Some(prepared) = self.prepared_image {
                    orchestrator
                        .create_prepared(&create, prepared, self.cancellation)
                        .await
                } else {
                    orchestrator.create(&create, self.cancellation).await
                }
                .map_err(message)?;
                (facts, EnvironmentTransition::Created)
            };
            (
                EnvironmentFacts {
                    container: facts.container,
                    image: facts.image,
                    architecture: facts.architecture,
                    generation: facts.generation,
                },
                transition,
            )
        };
        let initial_creation = request.active.is_none();
        let readiness = self
            .finish_readiness(
                plan,
                &request.desired.fingerprints,
                request.active,
                request.drift.runtime,
                facts.clone(),
                transition,
                engine.clone(),
            )
            .await;
        if readiness.is_err() && initial_creation && plan.compose.is_none() {
            let name = format!("cdenv-{}-{generation}", self.workspace);
            let _ = engine
                .stop(&facts.container, crate::CONTAINER_STOP_GRACE)
                .await;
            let _ = engine
                .cleanup_container(crate::ContainerExpectation {
                    id: &facts.container,
                    name: &name,
                    image_id: &facts.image,
                    installation: &self.installation,
                    workspace: self.workspace,
                    generation,
                    profile: &plan.profile,
                    project: None,
                    service: None,
                    running: Some(false),
                })
                .await;
            if matches!(plan.docker.build, BuildPlan::Dockerfile(_)) {
                let _ = engine
                    .cleanup_image(crate::ImageCleanupExpectation {
                        id: &facts.image,
                        installation: &self.installation,
                        workspace: self.workspace,
                        generation,
                        profile: &plan.profile,
                    })
                    .await;
            }
        }
        readiness
    }
}

#[derive(Clone)]
struct EnvironmentFacts {
    container: ContainerId,
    image: ImageId,
    architecture: cdenv_core::ContainerArchitecture,
    generation: GenerationId,
}

impl ProductionEnvironment<'_> {
    #[expect(
        clippy::too_many_lines,
        clippy::similar_names,
        reason = "provisioning and provisioned facts are distinct ordered readiness stages"
    )]
    #[expect(
        clippy::too_many_arguments,
        reason = "readiness receives each commit boundary explicitly"
    )]
    async fn finish_readiness(
        &self,
        plan: &ProductionPlan,
        fingerprints: &PlanFingerprints,
        previous: Option<&ActiveGeneration>,
        runtime_drift: bool,
        facts: EnvironmentFacts,
        transition: EnvironmentTransition,
        engine: BollardAdapter,
    ) -> Result<ReadyEnvironment, WorkflowMessage> {
        ensure_workspace_ssh_identity(self.root, self.workspace).map_err(message)?;
        let ssh = load_workspace_ssh_assets(self.root, self.workspace).map_err(message)?;
        let assets = ssh.installed_provision_assets();
        let build_id = AgentArtifactProvider::embedded_identity().map_err(message)?;
        let expected =
            AgentArtifactIdentity::new(build_id, ProtocolVersion::new(1).map_err(message)?);
        let provisioner = AgentProvisioner::new(engine.clone(), AgentArtifactProvider::embedded());
        let provisioned = provisioner
            .provision(
                &AgentProvisionRequest {
                    container: &facts.container,
                    remote_user: plan.runtime.remote_user.as_str(),
                    architecture: facts.architecture,
                    expected: &expected,
                    checkout: plan.runtime.workspace.folder.as_str(),
                    assets: &assets,
                },
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let environment_request = AgentEnvironmentRequest {
            container: &facts.container,
            remote_user: plan.runtime.remote_user.as_str(),
            provisioned: &provisioned,
            generation: facts.generation,
            checkout: plan.runtime.workspace.folder.as_str(),
            environment: &plan.runtime.environment,
            probe: plan.runtime.user_env_probe,
        };
        let capturer = AgentEnvironmentCapturer::new(engine.clone());
        let initial = capturer
            .capture_for_readiness(&environment_request, self.cancellation)
            .await
            .map_err(message)?;
        let persisted = load_workspace_state(&self.root.workspace(self.workspace).state_file())
            .map_err(message)?
            .into_state();
        let credential_lease = crate::credentials::credential_supervisor_lease(
            self.root,
            &persisted,
            cdenv_core::credential_broker::CredentialUserIdentity {
                uid: provisioned.identity.uid,
                gid: provisioned.identity.gid,
            },
            Path::new(&provisioned.identity.home),
            facts.generation,
        )
        .map_err(message)?;
        let forwarding = reconcile_forwarding(
            self.root,
            self.workspace,
            &self.installation,
            &facts,
            &provisioned,
            &plan.ports,
            previous.map(ActiveGeneration::forwarding),
            credential_lease.as_ref(),
        )
        .await?;
        let lifecycle_identity = LifecycleExecIdentity {
            installation: self.installation.to_string(),
            workspace: self.workspace.to_string(),
            container: facts.container.to_string(),
            generation: facts.generation.to_string(),
            build: provisioned.build_id.to_string(),
            protocol: provisioned.protocol_version.get(),
        };
        let lifecycle_record = lifecycle_exec_path(self.root, self.workspace);
        execute_lifecycle(
            &engine,
            &facts.container,
            plan.runtime.remote_user.as_str(),
            &provisioned,
            initial.snapshot_path(),
            plan.runtime.workspace.folder.as_str(),
            &plan.lifecycle,
            transition,
            previous.is_none(),
            LifecycleExecution::Foreground,
            &lifecycle_identity,
            &lifecycle_record,
            credential_lease.as_ref(),
            self.cancellation,
        )
        .await?;
        let ssh_environment = capturer
            .recapture_for_ssh(&environment_request, &initial, self.cancellation)
            .await
            .map_err(message)?;
        install_post_attach_manifest(
            &engine,
            &facts,
            plan.runtime.remote_user.as_str(),
            &provisioned,
            ssh_environment.snapshot_path(),
            plan.runtime.workspace.folder.as_str(),
            &plan.lifecycle.post_attach,
            credential_lease.as_ref(),
            self.cancellation,
        )
        .await?;
        execute_lifecycle(
            &engine,
            &facts.container,
            plan.runtime.remote_user.as_str(),
            &provisioned,
            ssh_environment.snapshot_path(),
            plan.runtime.workspace.folder.as_str(),
            &plan.lifecycle,
            transition,
            previous.is_none(),
            LifecycleExecution::Background,
            &lifecycle_identity,
            &lifecycle_record,
            credential_lease.as_ref(),
            self.cancellation,
        )
        .await?;
        let running = [
            (previous.is_none() || transition == EnvironmentTransition::Started)
                .then_some(&plan.lifecycle.on_create),
            (previous.is_none() || transition == EnvironmentTransition::Started)
                .then_some(&plan.lifecycle.update_content),
            (previous.is_none() || transition == EnvironmentTransition::Started)
                .then_some(&plan.lifecycle.post_create),
            (transition != EnvironmentTransition::AlreadyRunning)
                .then_some(&plan.lifecycle.post_start),
        ]
        .into_iter()
        .flatten()
        .find(|stage| stage.stage > plan.lifecycle.readiness && !stage.commands.is_empty())
        .map(|stage| map_stage(stage.stage))
        .or_else(|| {
            (transition == EnvironmentTransition::AlreadyRunning)
                .then(|| previous.and_then(|active| active.lifecycle().running()))
                .flatten()
        });
        let checkpoint =
            LifecycleCheckpoint::new(Some(map_stage(plan.lifecycle.readiness)), running, false);
        let state = ProvisionedState::new(
            plan.runtime.remote_user.as_str().to_owned(),
            plan.runtime.workspace.folder.as_str().to_owned(),
            facts.architecture,
            provisioned.agent_path,
            provisioned.build_id,
            provisioned.protocol_version,
            ssh_environment.snapshot_path().to_owned(),
        );
        let active = ActiveGeneration::new(
            facts.generation,
            plan.scenario.clone(),
            facts.container,
            facts.image.as_str().to_owned(),
            fingerprints.clone(),
            plan.feature_digests.clone(),
            checkpoint,
            forwarding,
            state,
        );
        Ok(ReadyEnvironment {
            active,
            transition,
            runtime: if runtime_drift {
                RuntimeReconciliation::Applied
            } else {
                RuntimeReconciliation::Current
            },
        })
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "generation, provisioning, forwarding, and credential identities remain explicit"
)]
async fn reconcile_forwarding(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    installation: &cdenv_core::InstallationId,
    facts: &EnvironmentFacts,
    provisioned: &crate::AgentProvisioningFacts,
    ports: &cdenv_devcontainer::PortPlan,
    previous: Option<&ActiveForwarding>,
    credential_lease: Option<&crate::SupervisorCredentialLease>,
) -> Result<ActiveForwarding, WorkflowMessage> {
    let desired = DesiredForwardingPlan::from_port_plan(ports).map_err(message)?;
    let paths = root.workspace(workspace);
    let host_build = AgentArtifactProvider::embedded_identity().map_err(message)?;
    let claim = SupervisorClaim {
        installation,
        workspace,
        generation: facts.generation,
        host_build_id: &host_build,
        agent_build_id: &provisioned.build_id,
        agent_protocol: provisioned.protocol_version,
    };
    if credential_lease.is_none()
        && previous.is_some_and(|previous| previous.requested() == desired.requested())
        && let Ok(state) = load_supervisor_state(&paths.supervisor_state_file())
    {
        let token = supervisor_control_token(&state).to_owned();
        if let Ok((live, None)) =
            crate::supervisor_credential_status(&paths.supervisor_socket(), &token, &claim).await
        {
            return active_forwarding(&desired, &host_build, &live);
        }
    }
    let rollback = previous
        .filter(|previous| !previous.requested().is_empty())
        .map(|previous| previous.requested().to_vec());
    if paths
        .supervisor_state_file()
        .try_exists()
        .map_err(message)?
    {
        let state = load_supervisor_state(&paths.supervisor_state_file()).map_err(message)?;
        let token = supervisor_control_token(&state).to_owned();
        if stop_supervisor(&paths.supervisor_socket(), &token, &claim)
            .await
            .is_err()
        {
            crate::forwarding::remove_stale_supervisor_runtime(
                &paths.supervisor_socket(),
                &paths.supervisor_state_file(),
                &paths.supervisor_lifetime_lock(),
                &claim,
            )
            .map_err(message)?;
        }
    }
    if desired.is_empty() && credential_lease.is_none() {
        return Ok(ActiveForwarding::new(None, Vec::new(), Vec::new()));
    }
    let state = match start_forwarding_supervisor(
        root,
        workspace,
        installation,
        facts,
        provisioned,
        &host_build,
        desired.requested(),
        credential_lease,
    )
    .await
    {
        Ok(state) => state,
        Err(primary) => {
            if let Some(rollback) = rollback {
                let _ = start_forwarding_supervisor(
                    root,
                    workspace,
                    installation,
                    facts,
                    provisioned,
                    &host_build,
                    &rollback,
                    credential_lease,
                )
                .await;
            }
            return Err(primary);
        }
    };
    active_forwarding(&desired, &host_build, &state)
}

#[expect(
    clippy::too_many_arguments,
    reason = "the detached supervisor receives each verified identity explicitly"
)]
async fn start_forwarding_supervisor(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    installation: &cdenv_core::InstallationId,
    facts: &EnvironmentFacts,
    provisioned: &crate::AgentProvisioningFacts,
    host_build: &cdenv_core::AgentBuildId,
    requested: &[DeclaredForward],
    credential_lease: Option<&crate::SupervisorCredentialLease>,
) -> Result<crate::SupervisorState, WorkflowMessage> {
    let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
    let paths = root.workspace(workspace);
    let forwards = requested
        .iter()
        .map(|forward| SupervisorForward {
            requested_port: forward.requested().port().get(),
            require_local_port: forward.require_local_port(),
            target_host: forward.target_host().to_owned(),
            target_port: forward.target_port().get(),
        })
        .collect();
    let manifest = SupervisorManifest::new(
        installation.clone(),
        workspace.clone(),
        facts.generation,
        host_build.clone(),
        provisioned.build_id.clone(),
        provisioned.protocol_version,
        facts.container.clone(),
        provisioned.agent_path.clone(),
        endpoint.socket_path().to_path_buf(),
        paths.supervisor_socket(),
        paths.supervisor_state_file(),
        paths.supervisor_lifetime_lock(),
        forwards,
    )
    .map_err(message)?;
    let manifest = if let Some(lease) = credential_lease {
        manifest.with_credential_lease(lease.clone())
    } else {
        manifest
    };
    start_detached_supervisor(&manifest).await.map_err(message)
}

fn active_forwarding(
    desired: &DesiredForwardingPlan,
    host_build: &cdenv_core::AgentBuildId,
    state: &crate::SupervisorState,
) -> Result<ActiveForwarding, WorkflowMessage> {
    if state.listeners.len() != desired.requested().len() {
        return Err(message(
            "forwarding supervisor returned an incomplete listener set",
        ));
    }
    let assigned = desired
        .requested()
        .iter()
        .zip(&state.listeners)
        .map(|(request, listener)| {
            let port = TcpPort::from_u32(u32::from(listener.port())).map_err(message)?;
            let endpoint = ForwardingEndpoint::new(listener.ip(), port).map_err(message)?;
            Ok(ForwardingEndpointAssignment::new(
                request.requested(),
                Some(endpoint),
            ))
        })
        .collect::<Result<Vec<_>, WorkflowMessage>>()?;
    Ok(ActiveForwarding::new(
        Some(host_build.clone()),
        desired.requested().to_vec(),
        assigned,
    ))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PostAttachManifestRequest {
    generation: String,
    build_id: String,
    protocol_version: u32,
    state_directory: String,
    environment_path: String,
    workspace_folder: String,
    uid: u32,
    gid: u32,
    stages: Vec<AgentLifecycleStagePlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_runtime: Option<String>,
    #[serde(skip_serializing_if = "cdenv_core::credentials::CredentialGrants::is_empty")]
    credential_grants: cdenv_core::credentials::CredentialGrants,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AgentLifecycleStagePlan {
    stage: AgentLifecycleStage,
    commands: Vec<AgentLifecycleCommand>,
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
enum AgentLifecycleStage {
    OnCreate,
    UpdateContent,
    PostCreate,
    PostStart,
    PostAttach,
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum AgentLifecycleCommand {
    Process {
        process: AgentLifecycleProcess,
    },
    Parallel {
        processes: BTreeMap<String, AgentLifecycleProcess>,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum AgentLifecycleProcess {
    Shell { command: AgentLifecycleValue },
    Exec { arguments: Vec<AgentLifecycleValue> },
}

#[derive(Serialize)]
#[serde(transparent)]
struct AgentLifecycleValue(Vec<AgentLifecycleValueSegment>);

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum AgentLifecycleValueSegment {
    Literal { value: String },
    ContainerEnvironment { name: String, default: String },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InstalledLifecycleManifest {
    manifest_path: String,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the immutable postAttach identity and selected-user boundary remain explicit"
)]
async fn install_post_attach_manifest(
    engine: &BollardAdapter,
    facts: &EnvironmentFacts,
    user: &str,
    provisioned: &crate::AgentProvisioningFacts,
    environment: &str,
    workspace: &str,
    post_attach: &cdenv_devcontainer::LifecycleStagePlan,
    credential_lease: Option<&crate::SupervisorCredentialLease>,
    cancellation: &CancellationToken,
) -> Result<(), WorkflowMessage> {
    let environment_path = Path::new(environment);
    let environment_directory = environment_path
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "environment"))
        .ok_or_else(|| message("SSH environment path has no safe lifecycle state root"))?;
    let state_root = environment_directory
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == ".cdenv"))
        .ok_or_else(|| message("SSH environment path has no safe lifecycle state root"))?;
    let state_directory = state_root.join("lifecycle");
    let expected = state_directory.join(format!(".cdenv-post-attach-{}.json", facts.generation));
    let request = PostAttachManifestRequest {
        generation: facts.generation.to_string(),
        build_id: provisioned.build_id.as_str().to_owned(),
        protocol_version: provisioned.protocol_version.get(),
        state_directory: state_directory.display().to_string(),
        environment_path: environment.to_owned(),
        workspace_folder: workspace.to_owned(),
        uid: provisioned.identity.uid,
        gid: provisioned.identity.gid,
        stages: vec![agent_lifecycle_stage(post_attach)],
        credential_runtime: credential_lease.map(|lease| lease.runtime_directory.clone()),
        credential_grants: credential_lease
            .map(|lease| lease.grants.clone())
            .unwrap_or_default(),
    };
    let input = serde_json::to_vec(&request)
        .map_err(|_| message("cannot encode postAttach lifecycle manifest"))?;
    let command = [
        provisioned.agent_path.clone(),
        "install-post-attach".to_owned(),
    ];
    let output = engine
        .execute(
            crate::ExecCommand {
                container: &facts.container,
                command: &command,
                user: Some(user),
                working_directory: None,
                environment: &[],
            },
            &input,
            cancellation,
        )
        .await
        .map_err(message)?;
    if !output.stderr.is_empty() {
        return Err(message(
            "postAttach lifecycle manifest installation emitted diagnostics",
        ));
    }
    let installed: InstalledLifecycleManifest =
        serde_json::from_slice(&output.stdout).map_err(|_| {
            message("postAttach lifecycle manifest installation returned invalid output")
        })?;
    if Path::new(&installed.manifest_path) != expected {
        return Err(message(
            "postAttach lifecycle manifest installation returned an unexpected path",
        ));
    }
    Ok(())
}

fn agent_lifecycle_stage(
    stage: &cdenv_devcontainer::LifecycleStagePlan,
) -> AgentLifecycleStagePlan {
    AgentLifecycleStagePlan {
        stage: match stage.stage {
            DevLifecycleStage::OnCreate => AgentLifecycleStage::OnCreate,
            DevLifecycleStage::UpdateContent => AgentLifecycleStage::UpdateContent,
            DevLifecycleStage::PostCreate => AgentLifecycleStage::PostCreate,
            DevLifecycleStage::PostStart => AgentLifecycleStage::PostStart,
            DevLifecycleStage::PostAttach => AgentLifecycleStage::PostAttach,
            DevLifecycleStage::Initialize => unreachable!("initialize runs on the host"),
        },
        commands: stage.commands.iter().map(agent_lifecycle_command).collect(),
    }
}

fn agent_lifecycle_command(command: &LifecycleCommand) -> AgentLifecycleCommand {
    match command {
        LifecycleCommand::Process(process) => AgentLifecycleCommand::Process {
            process: agent_lifecycle_process(process),
        },
        LifecycleCommand::Parallel(processes) => AgentLifecycleCommand::Parallel {
            processes: processes
                .iter()
                .map(|(name, process)| (name.clone(), agent_lifecycle_process(process)))
                .collect(),
        },
    }
}

fn agent_lifecycle_process(process: &LifecycleProcess) -> AgentLifecycleProcess {
    match process {
        LifecycleProcess::Shell(command) => AgentLifecycleProcess::Shell {
            command: agent_lifecycle_value(command),
        },
        LifecycleProcess::Exec(arguments) => AgentLifecycleProcess::Exec {
            arguments: arguments.iter().map(agent_lifecycle_value).collect(),
        },
    }
}

fn agent_lifecycle_value(value: &cdenv_devcontainer::DeferredString) -> AgentLifecycleValue {
    AgentLifecycleValue(
        value
            .segments()
            .map(|segment| match segment {
                DeferredSegment::Literal(value) => AgentLifecycleValueSegment::Literal {
                    value: value.to_owned(),
                },
                DeferredSegment::ContainerEnvironment { name, default } => {
                    AgentLifecycleValueSegment::ContainerEnvironment {
                        name: name.to_owned(),
                        default: default.to_owned(),
                    }
                }
            })
            .collect(),
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LifecycleExecution {
    Foreground,
    Background,
}

#[expect(
    clippy::too_many_arguments,
    reason = "the agent execution identity is explicit"
)]
async fn execute_lifecycle(
    engine: &BollardAdapter,
    container: &ContainerId,
    user: &str,
    provisioned: &crate::AgentProvisioningFacts,
    environment: &str,
    workspace: &str,
    plan: &cdenv_devcontainer::LifecyclePlan,
    transition: EnvironmentTransition,
    new_generation: bool,
    execution: LifecycleExecution,
    lifecycle_identity: &LifecycleExecIdentity,
    lifecycle_record: &Path,
    credential_lease: Option<&crate::SupervisorCredentialLease>,
    cancellation: &CancellationToken,
) -> Result<(), WorkflowMessage> {
    let agent = provisioned.agent_path.as_str();
    let mut stages = Vec::new();
    if new_generation
        || (execution == LifecycleExecution::Background
            && transition == EnvironmentTransition::Started)
    {
        // Fresh owned runners consult the generation checkpoint before executing
        // any one-time background group. Completed prefixes are never replayed.
        stages.extend([&plan.on_create, &plan.update_content, &plan.post_create]);
    }
    if transition != EnvironmentTransition::AlreadyRunning {
        stages.push(&plan.post_start);
    }
    let mut background = Vec::new();
    for stage in stages {
        let stage_execution = if stage.stage <= plan.readiness {
            LifecycleExecution::Foreground
        } else {
            LifecycleExecution::Background
        };
        if stage_execution != execution {
            continue;
        }
        if execution == LifecycleExecution::Background {
            if !stage.commands.is_empty() {
                background.push(agent_lifecycle_stage(stage));
            }
            continue;
        }
        for group in &stage.commands {
            let script = lifecycle_group_script(group)?;
            let command = lifecycle_agent_command(agent, environment, script, credential_lease)?;
            let output = engine
                .execute(
                    crate::ExecCommand {
                        container,
                        command: &command,
                        user: Some(user),
                        working_directory: Some(workspace),
                        environment: &[],
                    },
                    &[],
                    cancellation,
                )
                .await
                .map_err(message)?;
            std::io::stdout()
                .write_all(&output.stdout)
                .map_err(message)?;
            std::io::stderr()
                .write_all(&output.stderr)
                .map_err(message)?;
        }
    }
    if !background.is_empty() {
        let request = detached_lifecycle_request(
            lifecycle_identity,
            provisioned,
            environment,
            workspace,
            background,
            credential_lease,
            new_generation,
        )?;
        let command = vec![
            agent.to_owned(),
            "lifecycle-run-owned".to_owned(),
            serde_json::to_string(&request).map_err(message)?,
        ];
        let exec = crate::ExecCommand {
            container,
            command: &command,
            user: Some(user),
            working_directory: Some(workspace),
            environment: &[],
        };
        let detached = engine.create_detached_exec(&exec).await.map_err(message)?;
        // Publish the exact identity before start. An interrupted start remains
        // unknown (no successful final exit), never guessed to be complete.
        persist_owned_lifecycle_exec(
            lifecycle_record,
            lifecycle_identity,
            detached.id(),
            false,
            &request,
        )?;
        engine
            .start_detached_exec(&detached)
            .await
            .map_err(message)?;
        persist_owned_lifecycle_exec(
            lifecycle_record,
            lifecycle_identity,
            detached.id(),
            true,
            &request,
        )?;
    }
    Ok(())
}

fn detached_lifecycle_request(
    identity: &LifecycleExecIdentity,
    provisioned: &crate::AgentProvisioningFacts,
    environment: &str,
    workspace: &str,
    stages: Vec<AgentLifecycleStagePlan>,
    credential_lease: Option<&crate::SupervisorCredentialLease>,
    initialize_one_time: bool,
) -> Result<serde_json::Value, WorkflowMessage> {
    let state_root = Path::new(environment)
        .parent()
        .filter(|path| path.file_name().is_some_and(|name| name == "environment"))
        .and_then(Path::parent)
        .filter(|path| path.file_name().is_some_and(|name| name == ".cdenv"))
        .ok_or_else(|| message("environment path has no safe lifecycle state root"))?;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(message)?;
    let request = PostAttachManifestRequest {
        generation: identity.generation.clone(),
        build_id: identity.build.clone(),
        protocol_version: identity.protocol,
        state_directory: state_root.join("lifecycle").display().to_string(),
        environment_path: environment.to_owned(),
        workspace_folder: workspace.to_owned(),
        uid: provisioned.identity.uid,
        gid: provisioned.identity.gid,
        stages,
        credential_runtime: credential_lease.map(|lease| lease.runtime_directory.clone()),
        credential_grants: credential_lease
            .map(|lease| lease.grants.clone())
            .unwrap_or_default(),
    };
    let mut value = serde_json::to_value(request).map_err(message)?;
    if initialize_one_time {
        value["initializeOneTime"] = serde_json::json!(true);
    }
    value["ownership"] = serde_json::json!({
        "installation": identity.installation,
        "workspace": identity.workspace,
        "container": identity.container,
        "execution": hex::encode(nonce),
    });
    Ok(value)
}

fn lifecycle_agent_command(
    agent: &str,
    environment: &str,
    script: String,
    credential_lease: Option<&crate::SupervisorCredentialLease>,
) -> Result<Vec<String>, WorkflowMessage> {
    let mut command = vec![agent.to_owned()];
    if let Some(lease) = credential_lease {
        command.extend([
            "run-managed-environment".to_owned(),
            environment.to_owned(),
            lease.runtime_directory.clone(),
            serde_json::to_string(&lease.grants).map_err(message)?,
        ]);
    } else {
        command.extend(["run-environment".to_owned(), environment.to_owned()]);
    }
    command.extend([
        "--".to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script,
        "--".to_owned(),
    ]);
    Ok(command)
}

fn lifecycle_group_script(group: &LifecycleCommand) -> Result<String, WorkflowMessage> {
    match group {
        LifecycleCommand::Process(process) => lifecycle_process_script(process),
        LifecycleCommand::Parallel(processes) => {
            let mut script = String::from("pids='';status=0;");
            for process in processes.values() {
                script.push('(');
                script.push_str(&lifecycle_process_script(process)?);
                script.push_str(")&pids=\"$pids $!\";");
            }
            script.push_str(
                "for p in $pids;do wait \"$p\"||status=$?;done;[ \"$status\" -eq 0 ]||exit \"$status\"",
            );
            Ok(script)
        }
    }
}

fn lifecycle_process_script(process: &LifecycleProcess) -> Result<String, WorkflowMessage> {
    Ok(match process {
        LifecycleProcess::Shell(value) => {
            let mut script = String::from("v='';");
            append_deferred_value(&mut script, value);
            script.push_str("exec /bin/sh -c \"$v\"");
            script
        }
        LifecycleProcess::Exec(arguments) => {
            if arguments.is_empty() {
                return Err(message("lifecycle command has an empty argument vector"));
            }
            let mut script = String::from("set --;");
            for argument in arguments {
                script.push_str("v='';");
                append_deferred_value(&mut script, argument);
                script.push_str("set -- \"$@\" \"$v\";");
            }
            script.push_str("exec \"$@\"");
            script
        }
    })
}

fn append_deferred_value(output: &mut String, value: &cdenv_devcontainer::DeferredString) {
    for segment in value.segments() {
        match segment {
            DeferredSegment::Literal(value) => {
                output.push_str("v=$v");
                output.push_str(&shell_quote(value));
                output.push(';');
            }
            DeferredSegment::ContainerEnvironment { name, default } => {
                output.push_str("if [ \"${");
                output.push_str(name);
                output.push_str("+x}\" = x ];then v=$v\"$");
                output.push_str(name);
                output.push_str("\";else v=$v");
                output.push_str(&shell_quote(default));
                output.push_str(";fi;");
            }
        }
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn map_stage(stage: DevLifecycleStage) -> LifecycleStage {
    match stage {
        DevLifecycleStage::Initialize => LifecycleStage::InitializeCommand,
        DevLifecycleStage::OnCreate => LifecycleStage::OnCreateCommand,
        DevLifecycleStage::UpdateContent => LifecycleStage::UpdateContentCommand,
        DevLifecycleStage::PostCreate => LifecycleStage::PostCreateCommand,
        DevLifecycleStage::PostStart => LifecycleStage::PostStartCommand,
        DevLifecycleStage::PostAttach => LifecycleStage::PostAttachCommand,
    }
}

#[expect(
    clippy::type_complexity,
    clippy::similar_names,
    reason = "resolved graph, generated material, and persisted digests are distinct Feature products"
)]
fn resolve_feature_plan(
    root: &CdenvRoot,
    profile: &cdenv_devcontainer::RawProfile,
    configuration_directory: &Path,
) -> Result<
    (
        cdenv_devcontainer::ResolvedFeatures,
        Vec<GeneratedFeature>,
        BTreeMap<String, String>,
    ),
    WorkflowMessage,
> {
    let requests = crate::feature_lock::root_requests(profile).map_err(message)?;
    if requests.is_empty() {
        return Ok((
            cdenv_devcontainer::ResolvedFeatures::default(),
            Vec::new(),
            BTreeMap::new(),
        ));
    }
    let lock_path = configuration_directory.join(crate::FEATURE_LOCK_FILE);
    let bytes = std::fs::read(&lock_path).map_err(|error| {
        message(format!(
            "cannot read frozen Feature lock {}; run `cdenv lock`: {error}",
            lock_path.display()
        ))
    })?;
    let lock = cdenv_devcontainer::FeatureLock::parse(&bytes).map_err(message)?;
    lock.validate_requested(&requests).map_err(message)?;
    let resolver = FeatureSourceResolver::new(root.cache().blobs_dir()).map_err(message)?;
    let mut packages = BTreeMap::new();
    let mut artifacts = BTreeMap::new();
    for (reference, record) in &lock.features {
        let reference = cdenv_devcontainer::FeatureReference::parse(reference).map_err(message)?;
        let verified = resolver
            .resolve_locked(&reference, record, configuration_directory)
            .map_err(message)?;
        artifacts.insert(reference.clone(), verified.artifact);
        packages.insert(reference, verified.package);
    }
    let resolved = resolve_features(
        &requests,
        &packages,
        &profile.common.override_feature_install_order,
    )
    .map_err(message)?;
    lock.validate_frozen(&requests, &resolved, &packages)
        .map_err(message)?;
    let generated = resolved
        .installation_order
        .iter()
        .cloned()
        .map(|feature| {
            let directory = artifacts
                .get(&feature.reference)
                .ok_or_else(|| message("frozen Feature artifact is missing"))?;
            GeneratedFeature::from_directory(feature, directory).map_err(message)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let digests = resolved
        .installation_order
        .iter()
        .filter_map(|feature| {
            lock.features.get(feature.reference.as_str()).map(|record| {
                (
                    feature.reference.as_str().to_owned(),
                    record.integrity.clone(),
                )
            })
        })
        .collect();
    Ok((resolved, generated, digests))
}

const MAXIMUM_LIFECYCLE_RECORD_BYTES: u64 = 96 * 1024;

const fn host_architecture() -> cdenv_core::ContainerArchitecture {
    #[cfg(target_arch = "x86_64")]
    {
        cdenv_core::ContainerArchitecture::X86_64
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        cdenv_core::ContainerArchitecture::Aarch64
    }
}

fn unknown_host_capabilities() -> HostCapabilities {
    HostCapabilities {
        cpus: Measured::Unknown(UnknownMeasurement::Unmeasurable),
        memory_bytes: Measured::Unknown(UnknownMeasurement::Unmeasurable),
        storage_bytes: Measured::Unknown(UnknownMeasurement::Unmeasurable),
        gpu: Measured::Unknown(UnknownMeasurement::Unmeasurable),
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LifecycleExecIdentity {
    installation: String,
    workspace: String,
    container: String,
    generation: String,
    build: String,
    protocol: u32,
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LifecycleExecRecord {
    identity: LifecycleExecIdentity,
    exec_id: String,
    started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    request: Option<serde_json::Value>,
}

fn lifecycle_exec_path(root: &CdenvRoot, workspace: &WorkspaceName) -> PathBuf {
    root.workspace(workspace)
        .runtime_dir()
        .join("lifecycle-exec.json")
}

#[cfg(test)]
fn persist_lifecycle_exec(
    path: &Path,
    identity: &LifecycleExecIdentity,
    exec_id: &crate::ExecId,
    started: bool,
) -> Result<(), WorkflowMessage> {
    persist_lifecycle_record(path, identity, exec_id, started, None)
}

fn persist_owned_lifecycle_exec(
    path: &Path,
    identity: &LifecycleExecIdentity,
    exec_id: &crate::ExecId,
    started: bool,
    request: &serde_json::Value,
) -> Result<(), WorkflowMessage> {
    persist_lifecycle_record(path, identity, exec_id, started, Some(request.clone()))
}

fn persist_lifecycle_record(
    path: &Path,
    identity: &LifecycleExecIdentity,
    exec_id: &crate::ExecId,
    started: bool,
    request: Option<serde_json::Value>,
) -> Result<(), WorkflowMessage> {
    crate::ensure_private_directory(
        path.parent()
            .ok_or_else(|| message("lifecycle record parent"))?,
    )
    .map_err(message)?;
    let bytes = serde_json::to_vec(&LifecycleExecRecord {
        identity: identity.clone(),
        exec_id: exec_id.to_string(),
        started,
        request,
    })
    .map_err(message)?;
    if bytes.len() as u64 > MAXIMUM_LIFECYCLE_RECORD_BYTES {
        return Err(message("lifecycle execution record exceeded its bound"));
    }
    crate::atomic_write(path, &bytes, crate::ManagedMode::PrivateFile).map_err(message)
}

fn open_lifecycle_exec(path: &Path, expected: &std::fs::Metadata) -> Option<std::fs::File> {
    use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};

    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .ok()?;
    let opened = file.metadata().ok()?;
    if opened.dev() != expected.dev()
        || opened.ino() != expected.ino()
        || !opened.is_file()
        || opened.uid() != nix::unistd::geteuid().as_raw()
        || opened.mode() & 0o777 != 0o600
        || opened.nlink() != 1
        || opened.len() > MAXIMUM_LIFECYCLE_RECORD_BYTES
    {
        return None;
    }
    Some(file)
}

fn load_lifecycle_exec(path: &Path) -> Option<LifecycleExecRecord> {
    use std::io::Read as _;

    // Inspection must not repair state or follow links, including parent links.
    if std::fs::canonicalize(path).ok()?.as_path() != path {
        return None;
    }
    let metadata = std::fs::symlink_metadata(path).ok()?;
    let file = open_lifecycle_exec(path, &metadata)?;
    let mut bytes = Vec::new();
    file.take(MAXIMUM_LIFECYCLE_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > MAXIMUM_LIFECYCLE_RECORD_BYTES {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

fn completed_lifecycle_exec(
    record: &LifecycleExecRecord,
    expected: &LifecycleExecIdentity,
    inspected: &crate::ExecInspect,
) -> LifecycleStopOutcome {
    if record.started
        && record.identity == *expected
        && record.exec_id == inspected.id.as_str()
        && record.identity.container == inspected.container.as_str()
        && !inspected.running
        && inspected.exit_code == Some(0)
    {
        LifecycleStopOutcome::NotRunning
    } else {
        LifecycleStopOutcome::Indeterminate
    }
}

struct ProductionLifecycleStop<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    installation: &'a cdenv_core::InstallationId,
    engine: &'a BollardAdapter,
}

impl LifecycleRunnerStop for ProductionLifecycleStop<'_> {
    type Error = WorkflowMessage;

    async fn cancel(
        &self,
        active: Option<&ActiveGeneration>,
        grace: std::time::Duration,
    ) -> Result<LifecycleStopOutcome, Self::Error> {
        let Some(active) = active.filter(|active| active.lifecycle().running().is_some()) else {
            return Ok(LifecycleStopOutcome::NotRunning);
        };
        let expected = LifecycleExecIdentity {
            installation: self.installation.to_string(),
            workspace: self.workspace.to_string(),
            container: active.container_id().to_string(),
            generation: active.generation().to_string(),
            build: active.provisioned().agent_build_id().to_string(),
            protocol: active.provisioned().protocol_version().get(),
        };
        let Some(record) = load_lifecycle_exec(&lifecycle_exec_path(self.root, self.workspace))
            .filter(|record| record.identity == expected)
        else {
            return Ok(LifecycleStopOutcome::Indeterminate);
        };
        let Ok(exec_id) = crate::ExecId::parse(&record.exec_id) else {
            return Ok(LifecycleStopOutcome::Indeterminate);
        };
        // Docker Exec identity is authoritative. Never signal a guessed PID or
        // treat a missing/active/failed legacy runner as successfully completed.
        let Ok(inspected) = self.engine.inspect_exec(&exec_id).await else {
            return Ok(LifecycleStopOutcome::Indeterminate);
        };
        let Some(request) = &record.request else {
            return Ok(completed_lifecycle_exec(&record, &expected, &inspected));
        };
        let provisioned = active.provisioned();
        let expected_command = vec![
            provisioned.agent_path().to_owned(),
            "lifecycle-run-owned".to_owned(),
            serde_json::to_string(request).map_err(message)?,
        ];
        if !record.started
            || inspected.command.as_ref() != Some(&expected_command)
            || inspected.id.as_str() != record.exec_id
            || inspected.container.as_str() != expected.container
            || !owned_request_matches(request, &expected)
        {
            return Ok(LifecycleStopOutcome::Indeterminate);
        }
        let command = vec![
            provisioned.agent_path().to_owned(),
            "lifecycle-cancel-owned".to_owned(),
            serde_json::to_string(request).map_err(message)?,
            grace.as_millis().to_string(),
        ];
        let cancellation = CancellationToken::default();
        let Ok(Ok(output)) = tokio::time::timeout(
            grace + std::time::Duration::from_secs(1),
            self.engine.execute(
                crate::ExecCommand {
                    container: active.container_id(),
                    command: &command,
                    user: Some(provisioned.remote_user()),
                    working_directory: None,
                    environment: &[],
                },
                &[],
                &cancellation,
            ),
        )
        .await
        else {
            return Ok(LifecycleStopOutcome::Indeterminate);
        };
        let outcome = owned_cancellation_outcome(request, &output.stdout);
        if outcome == LifecycleStopOutcome::Indeterminate {
            return Ok(outcome);
        }
        // Releasing the runner lock precedes exiting its Docker Exec. Require both
        // acknowledgements rather than trusting a durable phase or a diagnostic PID.
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            let Ok(final_exec) = self.engine.inspect_exec(&exec_id).await else {
                return Ok(LifecycleStopOutcome::Indeterminate);
            };
            if final_exec.id.as_str() != record.exec_id
                || final_exec.container.as_str() != expected.container
            {
                return Ok(LifecycleStopOutcome::Indeterminate);
            }
            if !final_exec.running {
                return Ok(if final_exec.exit_code == Some(0) {
                    outcome
                } else {
                    LifecycleStopOutcome::Indeterminate
                });
            }
            if tokio::time::Instant::now() >= deadline {
                return Ok(LifecycleStopOutcome::Indeterminate);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    }
}

fn owned_request_matches(request: &serde_json::Value, expected: &LifecycleExecIdentity) -> bool {
    request["ownership"]["installation"].as_str() == Some(expected.installation.as_str())
        && request["ownership"]["workspace"].as_str() == Some(expected.workspace.as_str())
        && request["ownership"]["container"].as_str() == Some(expected.container.as_str())
        && request["generation"].as_str() == Some(expected.generation.as_str())
        && request["buildId"].as_str() == Some(expected.build.as_str())
        && request["protocolVersion"].as_u64() == Some(u64::from(expected.protocol))
        && request["ownership"]["execution"]
            .as_str()
            .is_some_and(|execution| {
                execution.len() == 64 && execution.bytes().all(|byte| byte.is_ascii_hexdigit())
            })
}

fn owned_cancellation_outcome(request: &serde_json::Value, output: &[u8]) -> LifecycleStopOutcome {
    let Ok(inspection) = serde_json::from_slice::<serde_json::Value>(output) else {
        return LifecycleStopOutcome::Indeterminate;
    };
    if inspection["runnerActive"].as_bool() != Some(false)
        || inspection["state"]["plan"] != *request
        || !inspection["state"]["runnerPid"].is_null()
        || inspection["state"].get("runnerPid").is_none()
    {
        return LifecycleStopOutcome::Indeterminate;
    }
    match inspection["state"]["phase"].as_str() {
        Some("complete") => LifecycleStopOutcome::NotRunning,
        Some("cancelled") => {
            let pending_one_time = request["stages"].as_array().is_some_and(|stages| {
                let index = inspection["state"]["stageIndex"]
                    .as_u64()
                    .and_then(|index| usize::try_from(index).ok())
                    .unwrap_or(0);
                stages.iter().skip(index).any(|stage| {
                    matches!(
                        stage["stage"].as_str(),
                        Some("onCreate" | "updateContent" | "postCreate")
                    )
                })
            });
            if pending_one_time && inspection["pendingOneTimeRetryable"].as_bool() != Some(true) {
                LifecycleStopOutcome::Indeterminate
            } else {
                LifecycleStopOutcome::Graceful
            }
        }
        // Interrupted one-time work remains indeterminate even after child cleanup.
        _ => LifecycleStopOutcome::Indeterminate,
    }
}

struct ProductionManagedStop<'a> {
    installation: &'a cdenv_core::InstallationId,
    workspace: &'a WorkspaceName,
    profile: &'a ProfileId,
    engine: BollardAdapter,
    cancellation: &'a CancellationToken,
}

impl ManagedEnvironmentStop for ProductionManagedStop<'_> {
    type Error = WorkflowMessage;

    #[expect(
        clippy::too_many_lines,
        reason = "selection, verification, stop, and post-stop verification remain auditable"
    )]
    async fn stop(
        &self,
        active: Option<&ActiveGeneration>,
        grace: std::time::Duration,
    ) -> Result<EnvironmentStopOutcome, Self::Error> {
        let Some(active) = active else {
            return Ok(EnvironmentStopOutcome::Missing);
        };
        if self.cancellation.is_cancelled() {
            return Err(message("managed shutdown was cancelled"));
        }
        let discovered = self
            .engine
            .discover(crate::ContainerDiscoveryScope {
                installation: self.installation,
                workspace: Some(self.workspace),
                generation: Some(active.generation()),
            })
            .await
            .map_err(message)?;
        let mut selected = Vec::new();
        match active.scenario() {
            ActiveScenario::Image | ActiveScenario::Dockerfile => {
                let matches = discovered
                    .iter()
                    .filter(|container| container.id == *active.container_id())
                    .collect::<Vec<_>>();
                if matches.is_empty() {
                    return Ok(EnvironmentStopOutcome::Missing);
                }
                if matches.len() != 1 || discovered.len() != 1 {
                    return Err(message("recorded image container identity is ambiguous"));
                }
                selected.push((None, active.container_id().clone()));
            }
            ActiveScenario::Compose {
                project,
                managed_services,
            } => {
                for service in managed_services {
                    let matches = discovered
                        .iter()
                        .filter(|container| {
                            container
                                .labels
                                .get(COMPOSE_PROJECT_LABEL)
                                .map(String::as_str)
                                == Some(project.as_str())
                                && container
                                    .labels
                                    .get(COMPOSE_SERVICE_LABEL)
                                    .map(String::as_str)
                                    == Some(service.as_str())
                        })
                        .collect::<Vec<_>>();
                    if matches.len() != 1 {
                        return Err(message(format!(
                            "recorded Compose service `{service}` is missing or ambiguous"
                        )));
                    }
                    selected.push((
                        Some((project.as_str(), service.as_str())),
                        matches[0].id.clone(),
                    ));
                }
                let primary_matches = selected
                    .iter()
                    .filter(|(_, id)| id == active.container_id())
                    .count();
                if primary_matches != 1 {
                    return Err(message(
                        "recorded Compose primary identity was not verified",
                    ));
                }
            }
        }

        let image = ImageId::parse(active.image_id()).map_err(message)?;
        let mut running = Vec::new();
        for (compose, id) in &selected {
            let inspection = self.engine.inspect_container(id).await.map_err(message)?;
            let expected_image = if id == active.container_id() {
                &image
            } else {
                &inspection.image_id
            };
            crate::bollard::verify_container(
                &inspection,
                crate::ContainerExpectation {
                    id,
                    name: &inspection.name,
                    image_id: expected_image,
                    installation: self.installation,
                    workspace: self.workspace,
                    generation: active.generation(),
                    profile: self.profile,
                    project: compose.map(|value| value.0),
                    service: compose.map(|value| value.1),
                    running: None,
                },
            )
            .map_err(message)?;
            if inspection.running {
                running.push(id.clone());
            }
        }
        for id in &running {
            if self.cancellation.is_cancelled() {
                return Err(message("managed shutdown was cancelled"));
            }
            self.engine.stop(id, grace).await.map_err(message)?;
        }
        for (compose, id) in &selected {
            let inspection = self.engine.inspect_container(id).await.map_err(message)?;
            crate::bollard::verify_container(
                &inspection,
                crate::ContainerExpectation {
                    id,
                    name: &inspection.name,
                    image_id: &inspection.image_id,
                    installation: self.installation,
                    workspace: self.workspace,
                    generation: active.generation(),
                    profile: self.profile,
                    project: compose.map(|value| value.0),
                    service: compose.map(|value| value.1),
                    running: Some(false),
                },
            )
            .map_err(message)?;
        }
        Ok(if running.is_empty() {
            EnvironmentStopOutcome::AlreadyStopped
        } else {
            EnvironmentStopOutcome::Stopped
        })
    }
}

/// Stops one production workspace using persisted, verified resource identity.
///
/// # Errors
///
/// Returns workspace, Docker, supervisor, cancellation, or state failures.
pub fn down_production(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
) -> Result<(), ProductionWorkflowError> {
    let paths = root.workspace(workspace);
    if !paths.root().exists() {
        return Err(ProductionWorkflowError::MissingWorkspace(workspace.clone()));
    }
    let state = load_workspace_state(&paths.state_file())?.into_state();
    let installation = state.installation_id().clone();
    let profile = state.devcontainer_profile().clone();
    let host_build = AgentArtifactProvider::embedded_identity()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let cancellation = CancellationToken::default();
    let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let connector = endpoint
        .bollard_connector(BOLLARD_CONTROL_TIMEOUT)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let engine = BollardAdapter::from_connector(&connector);
    let forwarding =
        ScopedForwardingSupervisor::new(root.clone(), installation.clone(), host_build);
    let managed = ProductionManagedStop {
        installation: &installation,
        workspace,
        profile: &profile,
        engine,
        cancellation: &cancellation,
    };
    let time = crate::create::current_timestamp()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let id = crate::create::random_operation_id()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation = OperationState::active(ForegroundOperation::Stopping, id, time)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    runtime
        .block_on(async {
            let signal_cancellation = cancellation.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_cancellation.cancel();
                }
            });
            let result = down_workspace(
                root,
                DownRequest::new(workspace, operation),
                &forwarding,
                &ProductionLifecycleStop {
                    root,
                    workspace,
                    installation: &installation,
                    engine: &managed.engine,
                },
                &managed,
            )
            .await;
            signal.abort();
            result
        })
        .map(|outcome| {
            for warning in outcome.warnings {
                eprintln!("cdenv: warning: {warning:?}");
            }
        })
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))
}

struct ProductionImageRebuild<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    installation: cdenv_core::InstallationId,
    profile: ProfileId,
    cancellation: &'a CancellationToken,
    engine: BollardAdapter,
    docker: crate::DockerCliAdapter,
    forwarding: ScopedForwardingSupervisor,
}

impl ProductionImageRebuild<'_> {
    fn identity<'a>(
        &'a self,
        plan: &'a ProductionPlan,
        generation: GenerationId,
    ) -> DockerResourceIdentity<'a> {
        DockerResourceIdentity {
            installation: &self.installation,
            workspace: self.workspace,
            generation,
            profile: &plan.profile,
        }
    }

    async fn restore_previous_forwarding(
        &self,
        previous: &ActiveGeneration,
    ) -> Result<(), WorkflowMessage> {
        if previous.forwarding().requested().is_empty() {
            return Ok(());
        }
        let facts = EnvironmentFacts {
            container: previous.container_id().clone(),
            image: ImageId::parse(previous.image_id()).map_err(message)?,
            architecture: previous.provisioned().container_architecture(),
            generation: previous.generation(),
        };
        start_forwarding_supervisor_from_provisioned(
            self.root,
            self.workspace,
            &self.installation,
            &facts,
            previous.provisioned(),
            previous.forwarding().requested(),
        )
        .await
        .map(|_| ())
    }
}

impl RebuildEnvironment<ProductionPlan> for ProductionImageRebuild<'_> {
    type Build = PreparedImage;
    type Error = WorkflowMessage;

    async fn build(
        &self,
        desired: &PreparedRebuildPlan<ProductionPlan>,
        generation: GenerationId,
        no_cache: bool,
    ) -> Result<Self::Build, Self::Error> {
        let plan = &desired.plan;
        if plan.compose.is_some() {
            return Err(message("image rebuild adapter received a Compose plan"));
        }
        let identity = self.identity(plan, generation);
        let orchestrator =
            ImageContainerOrchestrator::new(self.docker.clone(), self.engine.clone());
        if let Some(base) = &plan.base_build {
            let context = DockerBuildContext::Repository;
            let tag = format!("cdenv/{}-feature-base:{generation}", self.workspace);
            let claim = self
                .docker
                .build(
                    &DockerBuildRequest {
                        plan: base,
                        checkout: &plan.checkout,
                        tag: &tag,
                        identity,
                        context: &context,
                        dockerfile: DockerfileInput::Repository,
                        no_cache,
                    },
                    self.cancellation,
                )
                .await
                .map_err(message)?;
            let inspection = self
                .engine
                .inspect_image(claim.image_id.as_str())
                .await
                .map_err(message)?;
            if inspection.id != claim.image_id
                || inspection
                    .labels
                    .get("cdenv.installation")
                    .map(String::as_str)
                    != Some(self.installation.as_str())
                || inspection.labels.get("cdenv.workspace").map(String::as_str)
                    != Some(self.workspace.as_str())
                || inspection
                    .labels
                    .get("cdenv.generation")
                    .map(String::as_str)
                    != Some(generation.to_string().as_str())
                || inspection.labels.get("cdenv.profile").map(String::as_str)
                    != Some(plan.profile.as_str())
                || inspection.labels.get("cdenv.generated").map(String::as_str) != Some("true")
            {
                return Err(message("feature base build image identity mismatch"));
            }
        }
        let context = plan.generated.as_ref().map_or_else(
            || DockerBuildContext::Repository,
            |generated| DockerBuildContext::Generated(generated.files().to_vec()),
        );
        let dockerfile = plan
            .generated
            .as_ref()
            .map_or(DockerfileInput::Repository, |generated| {
                DockerfileInput::Generated(generated.dockerfile())
            });
        let build_tag = matches!(plan.docker.build, BuildPlan::Dockerfile(_))
            .then(|| format!("cdenv/{}:{generation}", self.workspace));
        orchestrator
            .prepare_image(
                &ImageContainerBuildRequest {
                    build: &plan.docker.build,
                    checkout: &plan.checkout,
                    build_tag: build_tag.as_deref(),
                    build_context: &context,
                    dockerfile,
                    identity,
                    no_cache,
                },
                self.cancellation,
            )
            .await
            .map_err(message)
    }

    async fn quiesce(&self, previous: Option<&ActiveGeneration>) -> Result<(), Self::Error> {
        if self.cancellation.is_cancelled() {
            return Err(message("rebuild was cancelled before quiescing"));
        }
        self.forwarding
            .stop(self.workspace, previous)
            .await
            .map_err(message)?;
        match (ProductionLifecycleStop {
            root: self.root,
            workspace: self.workspace,
            installation: &self.installation,
            engine: &self.engine,
        })
        .cancel(previous, crate::LIFECYCLE_STOP_GRACE)
        .await?
        {
            LifecycleStopOutcome::NotRunning | LifecycleStopOutcome::Graceful => Ok(()),
            LifecycleStopOutcome::Forced => Err(message(
                "lifecycle runner required forced cancellation without persisted runner identity",
            )),
            LifecycleStopOutcome::Indeterminate => Err(message(
                "background lifecycle completion or cancellation could not be verified",
            )),
        }
    }

    async fn backup(
        &self,
        previous: &ActiveGeneration,
        backup_name: &str,
    ) -> Result<(), Self::Error> {
        if self.cancellation.is_cancelled() {
            return Err(message(
                "rebuild was cancelled before old-primary replacement",
            ));
        }
        let image = ImageId::parse(previous.image_id()).map_err(message)?;
        let original_name = format!("cdenv-{}-{}", self.workspace, previous.generation());
        let inspection = self
            .engine
            .inspect_container(previous.container_id())
            .await
            .map_err(message)?;
        crate::bollard::verify_container(
            &inspection,
            crate::ContainerExpectation {
                id: previous.container_id(),
                name: &original_name,
                image_id: &image,
                installation: &self.installation,
                workspace: self.workspace,
                generation: previous.generation(),
                profile: &self.profile,
                project: None,
                service: None,
                running: None,
            },
        )
        .map_err(message)?;
        if inspection.running {
            self.engine
                .stop(previous.container_id(), crate::CONTAINER_STOP_GRACE)
                .await
                .map_err(message)?;
        }
        let stopped = self
            .engine
            .inspect_container(previous.container_id())
            .await
            .map_err(message)?;
        if stopped.running || stopped.id != *previous.container_id() {
            return Err(message(
                "old primary did not reach a verified stopped state",
            ));
        }
        self.engine
            .rename(previous.container_id(), backup_name)
            .await
            .map_err(message)?;
        let renamed = self
            .engine
            .inspect_container(previous.container_id())
            .await
            .map_err(message)?;
        if renamed.id != *previous.container_id() || renamed.name != backup_name || renamed.running
        {
            return Err(message("old primary backup rename was not verified"));
        }
        Ok(())
    }

    async fn create_ready(
        &self,
        desired: &PreparedRebuildPlan<ProductionPlan>,
        build: &Self::Build,
        generation: GenerationId,
    ) -> Result<ActiveGeneration, Self::Error> {
        let plan = &desired.plan;
        HostLifecycleExecutor
            .initialize(
                &plan.lifecycle.initialize,
                &plan.checkout,
                LifecycleInput::Inherit,
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let identity = self.identity(plan, generation);
        let orchestrator =
            ImageContainerOrchestrator::new(self.docker.clone(), self.engine.clone());
        let context = plan.generated.as_ref().map_or_else(
            || DockerBuildContext::Repository,
            |generated| DockerBuildContext::Generated(generated.files().to_vec()),
        );
        let dockerfile = plan
            .generated
            .as_ref()
            .map_or(DockerfileInput::Repository, |generated| {
                DockerfileInput::Generated(generated.dockerfile())
            });
        let command = if plan.runtime.override_command {
            vec![
                "/bin/sh".to_owned(),
                "-c".to_owned(),
                "trap 'exit 0' TERM; while sleep 3600; do :; done".to_owned(),
            ]
        } else {
            Vec::new()
        };
        let name = format!("cdenv-{}-{generation}", self.workspace);
        let build_tag = format!("cdenv/{}:{generation}", self.workspace);
        let facts = orchestrator
            .create_prepared(
                &ImageContainerCreateRequest {
                    build: &plan.docker.build,
                    checkout: &plan.checkout,
                    build_tag: matches!(plan.docker.build, BuildPlan::Dockerfile(_))
                        .then_some(build_tag.as_str()),
                    build_context: &context,
                    dockerfile,
                    container_name: &name,
                    identity,
                    runtime: &plan.runtime,
                    create_options: &plan.docker.create,
                    ports: &plan.ports,
                    command: &command,
                    no_cache: false,
                    cdenv_owned_targets: &plan.owned_targets,
                    host_requirements: plan.host_requirements.as_ref(),
                    host_capabilities: &plan.capabilities,
                    recorded_container: None,
                },
                build,
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let environment = ProductionEnvironment {
            root: self.root,
            workspace: self.workspace,
            installation: self.installation.clone(),
            cancellation: self.cancellation,
            no_cache: false,
            prepared_image: Some(build),
        };
        let ready = environment
            .finish_readiness(
                plan,
                &desired.fingerprints,
                None,
                false,
                EnvironmentFacts {
                    container: facts.container,
                    image: facts.image,
                    architecture: facts.architecture,
                    generation: facts.generation,
                },
                EnvironmentTransition::Created,
                self.engine.clone(),
            )
            .await?;
        if ready.active.generation() != generation {
            return Err(message("prepared candidate generation mismatch"));
        }
        Ok(ready.active)
    }

    async fn handoff(
        &self,
        _previous: Option<&ActiveGeneration>,
        replacement: &ActiveGeneration,
    ) -> Result<(), Self::Error> {
        if self.cancellation.is_cancelled() {
            return Err(message("rebuild was cancelled during forwarding handoff"));
        }
        if replacement.forwarding().requested().is_empty() {
            return Ok(());
        }
        let paths = self.root.workspace(self.workspace);
        let state = load_supervisor_state(&paths.supervisor_state_file()).map_err(message)?;
        let host_build = AgentArtifactProvider::embedded_identity().map_err(message)?;
        let claim = SupervisorClaim {
            installation: &self.installation,
            workspace: self.workspace,
            generation: replacement.generation(),
            host_build_id: &host_build,
            agent_build_id: replacement.provisioned().agent_build_id(),
            agent_protocol: replacement.provisioned().protocol_version(),
        };
        supervisor_status(
            &paths.supervisor_socket(),
            supervisor_control_token(&state),
            &claim,
        )
        .await
        .map(|_| ())
        .map_err(message)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "candidate cleanup and exact old-primary restoration remain auditable together"
    )]
    async fn rollback(
        &self,
        request: RebuildRollbackRequest<'_, Self::Build>,
    ) -> Result<(), Self::Error> {
        let mut failures = Vec::new();
        if let Some(candidate) = request.candidate {
            match self.forwarding.stop(self.workspace, Some(candidate)).await {
                Ok(
                    crate::ForwardingStopOutcome::Stopped | crate::ForwardingStopOutcome::Missing,
                ) => {}
                Ok(crate::ForwardingStopOutcome::Degraded) => failures.push(
                    "candidate forwarding state was degraded and could not be stopped safely"
                        .to_owned(),
                ),
                Err(error) => failures.push(error.to_string()),
            }
            let name = format!("cdenv-{}-{}", self.workspace, candidate.generation());
            let image = ImageId::parse(candidate.image_id()).map_err(message)?;
            if let Ok(inspection) = self
                .engine
                .inspect_container(candidate.container_id())
                .await
            {
                let expected = crate::ContainerExpectation {
                    id: candidate.container_id(),
                    name: &name,
                    image_id: &image,
                    installation: &self.installation,
                    workspace: self.workspace,
                    generation: candidate.generation(),
                    profile: &self.profile,
                    project: None,
                    service: None,
                    running: None,
                };
                if crate::bollard::verify_container(&inspection, expected).is_ok() {
                    if inspection.running
                        && let Err(error) = self
                            .engine
                            .stop(candidate.container_id(), crate::CONTAINER_STOP_GRACE)
                            .await
                    {
                        failures.push(error.to_string());
                    }
                    if let Err(error) = self.engine.cleanup_container(expected).await {
                        failures.push(error.to_string());
                    }
                }
            }
        }
        if request.build.generated
            && let Err(error) = self
                .engine
                .cleanup_image(crate::ImageCleanupExpectation {
                    id: &request.build.image,
                    installation: &self.installation,
                    workspace: self.workspace,
                    generation: request.generation,
                    profile: &self.profile,
                })
                .await
        {
            failures.push(error.to_string());
        }
        if let Some(previous) = request.previous {
            let original_name = format!("cdenv-{}-{}", self.workspace, previous.generation());
            let image = ImageId::parse(previous.image_id()).map_err(message)?;
            match self.engine.inspect_container(previous.container_id()).await {
                Ok(inspection)
                    if inspection.name == request.backup_name
                        || inspection.name == original_name =>
                {
                    let expected_name = inspection.name.clone();
                    let expected = crate::ContainerExpectation {
                        id: previous.container_id(),
                        name: &expected_name,
                        image_id: &image,
                        installation: &self.installation,
                        workspace: self.workspace,
                        generation: previous.generation(),
                        profile: &self.profile,
                        project: None,
                        service: None,
                        running: None,
                    };
                    if let Err(error) = crate::bollard::verify_container(&inspection, expected) {
                        failures.push(error.to_string());
                    } else if inspection.name == request.backup_name {
                        if inspection.running {
                            failures.push(
                                "verified old backup unexpectedly remained running".to_owned(),
                            );
                        } else if let Err(error) = self
                            .engine
                            .rename(previous.container_id(), &original_name)
                            .await
                        {
                            failures.push(error.to_string());
                        } else if let Err(error) = self.engine.start(previous.container_id()).await
                        {
                            failures.push(error.to_string());
                        }
                    } else if !inspection.running
                        && let Err(error) = self.engine.start(previous.container_id()).await
                    {
                        failures.push(error.to_string());
                    }
                }
                Ok(_) => {
                    failures
                        .push("verified old primary had an unexpected name or state".to_owned());
                }
                Err(error) => failures.push(error.to_string()),
            }
            if failures.is_empty()
                && let Err(error) = self.restore_previous_forwarding(previous).await
            {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(message(format!(
                "rollback incomplete: {}",
                failures.join("; ")
            )))
        }
    }

    async fn cleanup(
        &self,
        request: RebuildCleanupRequest<'_, Self::Build>,
    ) -> Result<(), Self::Error> {
        let Some(previous) = request.previous else {
            return Ok(());
        };
        let image = ImageId::parse(previous.image_id()).map_err(message)?;
        self.engine
            .cleanup_container(crate::ContainerExpectation {
                id: previous.container_id(),
                name: request.backup_name,
                image_id: &image,
                installation: &self.installation,
                workspace: self.workspace,
                generation: previous.generation(),
                profile: &self.profile,
                project: None,
                service: None,
                running: Some(false),
            })
            .await
            .map_err(message)
    }
}

async fn start_forwarding_supervisor_from_provisioned(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    installation: &cdenv_core::InstallationId,
    facts: &EnvironmentFacts,
    provisioned: &ProvisionedState,
    requested: &[DeclaredForward],
) -> Result<crate::SupervisorState, WorkflowMessage> {
    let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
    let paths = root.workspace(workspace);
    let host_build = AgentArtifactProvider::embedded_identity().map_err(message)?;
    let forwards = requested
        .iter()
        .map(|forward| SupervisorForward {
            requested_port: forward.requested().port().get(),
            require_local_port: forward.require_local_port(),
            target_host: forward.target_host().to_owned(),
            target_port: forward.target_port().get(),
        })
        .collect();
    let manifest = SupervisorManifest::new(
        installation.clone(),
        workspace.clone(),
        facts.generation,
        host_build,
        provisioned.agent_build_id().clone(),
        provisioned.protocol_version(),
        facts.container.clone(),
        provisioned.agent_path().to_owned(),
        endpoint.socket_path().to_path_buf(),
        paths.supervisor_socket(),
        paths.supervisor_state_file(),
        paths.supervisor_lifetime_lock(),
        forwards,
    )
    .map_err(message)?;
    start_detached_supervisor(&manifest).await.map_err(message)
}

/// Replaces a production workspace with a newly planned generation.
///
/// Planning and frozen-input validation complete before the old generation is stopped. Image
/// scenarios retain the stopped old generation as rollback evidence; Compose failures retain the
/// coordinator's non-atomic failure rather than adopting an unverified service.
///
/// # Errors
///
/// Returns planning, shutdown, Docker, readiness, invariant, or persistence failures.
pub fn rebuild_production(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    config: Option<&RepoRelativeConfigPath>,
    no_cache: bool,
) -> Result<(), ProductionWorkflowError> {
    let paths = root.workspace(workspace);
    if !paths.root().exists() {
        return Err(ProductionWorkflowError::MissingWorkspace(workspace.clone()));
    }
    let initial = load_workspace_state(&paths.state_file())?.into_state();
    if matches!(
        initial.active().map(ActiveGeneration::scenario),
        Some(ActiveScenario::Compose { .. })
    ) {
        return rebuild_compose_production(root, workspace, config, no_cache);
    }
    let installation = initial.installation_id().clone();
    let profile = initial.devcontainer_profile().clone();
    let cancellation = CancellationToken::default();
    let planner = ProductionPlanner {
        root,
        workspace,
        cancellation: &cancellation,
        replacement: true,
    };
    let endpoint = DockerEndpoint::resolve(&ProcessDockerEnvironment)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let connector = endpoint
        .bollard_connector(BOLLARD_CONTROL_TIMEOUT)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let engine = BollardAdapter::from_connector(&connector);
    let host_build = AgentArtifactProvider::embedded_identity()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let environment = ProductionImageRebuild {
        root,
        workspace,
        installation: installation.clone(),
        profile,
        cancellation: &cancellation,
        engine,
        docker: crate::DockerCliAdapter::system(
            endpoint,
            ProcessRunner::new(root.logs_dir()),
            root.temp_dir(),
        ),
        forwarding: ScopedForwardingSupervisor::new(root.clone(), installation, host_build),
    };
    let completed_at = crate::create::current_timestamp()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation_id = crate::create::random_operation_id()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation = OperationState::active(
        ForegroundOperation::Rebuilding,
        operation_id.clone(),
        completed_at.clone(),
    )
    .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let backup_name = format!("cdenv-backup-{operation_id}");
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let outcome = runtime
        .block_on(async {
            let signal_cancellation = cancellation.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_cancellation.cancel();
                }
            });
            let result = rebuild_workspace(
                root,
                RebuildRequest {
                    workspace,
                    config,
                    operation,
                    backup_name: &backup_name,
                    no_cache,
                    completed_at,
                },
                &planner,
                &GitAdapter::system(),
                &environment,
            )
            .await;
            signal.abort();
            result
        })
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    if let Some(warning) = outcome.cleanup_warning {
        eprintln!("cdenv: warning: post-rebuild cleanup failed safely: {warning}");
    }
    Ok(())
}

#[derive(Clone)]
struct ProductionComposeBuild {
    image: ImageId,
}

struct ProductionComposeRebuild<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    installation: cdenv_core::InstallationId,
    previous: Option<ActiveGeneration>,
    cancellation: &'a CancellationToken,
    adapter: ComposeAdapter,
    engine: BollardAdapter,
    state: Mutex<WorkspaceState>,
    completed_at: crate::StateTimestamp,
    created: Mutex<Option<ComposeLifecycleFacts>>,
}

impl ProductionComposeRebuild<'_> {
    fn identity<'a>(
        &'a self,
        plan: &'a ProductionPlan,
        generation: GenerationId,
    ) -> DockerResourceIdentity<'a> {
        DockerResourceIdentity {
            installation: &self.installation,
            workspace: self.workspace,
            generation,
            profile: &plan.profile,
        }
    }

    fn compose(plan: &ProductionPlan) -> Result<&ProductionComposePlan, WorkflowMessage> {
        plan.compose
            .as_ref()
            .ok_or_else(|| message("Compose rebuild adapter received an image plan"))
    }
}

impl ComposeRebuildRuntime<PreparedRebuildPlan<ProductionPlan>> for ProductionComposeRebuild<'_> {
    type Build = ProductionComposeBuild;
    type Error = WorkflowMessage;

    async fn build_complete(
        &self,
        desired: &PreparedRebuildPlan<ProductionPlan>,
        generation: GenerationId,
        no_cache: bool,
    ) -> Result<Self::Build, Self::Error> {
        let plan = &desired.plan;
        let compose = Self::compose(plan)?;
        let project = ComposeProject {
            files: &compose.files,
            project_name: compose.plan.project_name(),
            working_directory: &plan.checkout,
        };
        let base = self
            .adapter
            .prepare_base(
                &ComposeBaseRequest {
                    project,
                    service: &compose.primary_service,
                    has_build: compose.primary_has_build,
                    base_tag: &compose.base_tag,
                    no_cache,
                },
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let image = if let Some(generated) = &plan.generated {
            let BuildPlan::Dockerfile(build) = &plan.docker.build else {
                return Err(message("internal generated Compose plan mismatch"));
            };
            crate::DockerCliAdapter::system(
                self.adapter.endpoint().clone(),
                ProcessRunner::new(self.root.logs_dir()),
                self.root.temp_dir(),
            )
            .build(
                &DockerBuildRequest {
                    plan: build,
                    checkout: &plan.checkout,
                    tag: &format!("cdenv/{}:{generation}", self.workspace),
                    identity: self.identity(plan, generation),
                    context: &DockerBuildContext::Generated(generated.files().to_vec()),
                    dockerfile: DockerfileInput::Generated(generated.dockerfile()),
                    no_cache,
                },
                self.cancellation,
            )
            .await
            .map_err(message)?
            .image_id
        } else {
            base.image_id
        };
        let inspected = self
            .engine
            .inspect_image(image.as_str())
            .await
            .map_err(message)?;
        if inspected.id != image {
            return Err(message("Compose replacement image identity mismatch"));
        }
        Ok(ProductionComposeBuild { image })
    }

    async fn force_recreate(
        &self,
        desired: &PreparedRebuildPlan<ProductionPlan>,
        build: &Self::Build,
        generation: GenerationId,
    ) -> Result<(), Self::Error> {
        let plan = &desired.plan;
        let compose = Self::compose(plan)?;
        HostLifecycleExecutor
            .initialize(
                &plan.lifecycle.initialize,
                &plan.checkout,
                LifecycleInput::Inherit,
                self.cancellation,
            )
            .await
            .map_err(message)?;
        let host_build = AgentArtifactProvider::embedded_identity().map_err(message)?;
        ScopedForwardingSupervisor::new(self.root.clone(), self.installation.clone(), host_build)
            .stop(self.workspace, self.previous.as_ref())
            .await
            .map_err(message)?;
        match (ProductionLifecycleStop {
            root: self.root,
            workspace: self.workspace,
            installation: &self.installation,
            engine: &self.engine,
        })
        .cancel(self.previous.as_ref(), crate::LIFECYCLE_STOP_GRACE)
        .await?
        {
            LifecycleStopOutcome::NotRunning | LifecycleStopOutcome::Graceful => {}
            LifecycleStopOutcome::Forced | LifecycleStopOutcome::Indeterminate => {
                return Err(message(
                    "background lifecycle completion or cancellation could not be verified",
                ));
            }
        }
        if self.cancellation.is_cancelled() {
            return Err(message("Compose rebuild was cancelled before recreation"));
        }
        let orchestrator =
            ComposeLifecycleOrchestrator::new(self.adapter.clone(), self.engine.clone());
        let facts = orchestrator
            .create_or_rebuild(
                &CreateComposeRequest {
                    up: ComposeUpRequest {
                        project: ComposeProject {
                            files: &compose.files,
                            project_name: compose.plan.project_name(),
                            working_directory: &plan.checkout,
                        },
                        plan: &compose.plan,
                        force_recreate: true,
                    },
                    identity: self.identity(plan, generation),
                    primary_image: &build.image,
                    ports: &plan.ports,
                },
                self.cancellation,
            )
            .await
            .map_err(message)?;
        *self
            .created
            .lock()
            .map_err(|_| message("Compose rebuild claim lock was poisoned"))? = Some(facts);
        Ok(())
    }

    async fn inspect_services(
        &self,
        generation: GenerationId,
    ) -> Result<Vec<ObservedComposeService>, Self::Error> {
        let discovered = self
            .engine
            .discover(crate::ContainerDiscoveryScope {
                installation: &self.installation,
                workspace: Some(self.workspace),
                generation: Some(generation),
            })
            .await
            .map_err(message)?;
        let mut observed = Vec::new();
        for container in discovered {
            let Some(service) = container.labels.get(COMPOSE_SERVICE_LABEL) else {
                continue;
            };
            let inspection = self
                .engine
                .inspect_container(&container.id)
                .await
                .map_err(message)?;
            observed.push(ObservedComposeService {
                service: service.clone(),
                container: container.id,
                running: inspection.running,
                health: match inspection.healthy {
                    Some(true) if inspection.running => ComposeServiceHealth::Healthy,
                    Some(false) => ComposeServiceHealth::Unhealthy,
                    None | Some(true) => ComposeServiceHealth::Unknown,
                },
            });
        }
        Ok(observed)
    }

    async fn complete_readiness(
        &self,
        desired: &PreparedRebuildPlan<ProductionPlan>,
        build: &Self::Build,
        generation: GenerationId,
        managed: &BTreeMap<String, ContainerId>,
    ) -> Result<ActiveGeneration, Self::Error> {
        let created = self
            .created
            .lock()
            .map_err(|_| message("Compose rebuild claim lock was poisoned"))?
            .clone()
            .ok_or_else(|| message("Compose recreation did not retain a verified claim"))?;
        if &created.managed != managed {
            return Err(message(
                "Compose rediscovery differed from the verified recreation claim",
            ));
        }
        let image = self
            .engine
            .inspect_image(build.image.as_str())
            .await
            .map_err(message)?;
        let environment = ProductionEnvironment {
            root: self.root,
            workspace: self.workspace,
            installation: self.installation.clone(),
            cancellation: self.cancellation,
            no_cache: false,
            prepared_image: None,
        };
        environment
            .finish_readiness(
                &desired.plan,
                &desired.fingerprints,
                None,
                false,
                EnvironmentFacts {
                    container: created.primary,
                    image: build.image.clone(),
                    architecture: image.architecture,
                    generation,
                },
                EnvironmentTransition::Created,
                self.engine.clone(),
            )
            .await
            .map(|ready| ready.active)
    }

    async fn handoff_forwarding(&self, active: &ActiveGeneration) -> Result<(), Self::Error> {
        if self.cancellation.is_cancelled() {
            return Err(message(
                "Compose rebuild was cancelled during forwarding handoff",
            ));
        }
        if active.forwarding().requested().is_empty() {
            return Ok(());
        }
        let paths = self.root.workspace(self.workspace);
        let state = load_supervisor_state(&paths.supervisor_state_file()).map_err(message)?;
        let host_build = AgentArtifactProvider::embedded_identity().map_err(message)?;
        supervisor_status(
            &paths.supervisor_socket(),
            supervisor_control_token(&state),
            &SupervisorClaim {
                installation: &self.installation,
                workspace: self.workspace,
                generation: active.generation(),
                host_build_id: &host_build,
                agent_build_id: active.provisioned().agent_build_id(),
                agent_protocol: active.provisioned().protocol_version(),
            },
        )
        .await
        .map(|_| ())
        .map_err(message)
    }

    async fn commit_active(
        &self,
        active: &ActiveGeneration,
        managed: &BTreeMap<String, ContainerId>,
    ) -> Result<(), Self::Error> {
        let Some((_, expected)) = active.scenario().compose() else {
            return Err(message(
                "Compose readiness returned a non-Compose active scenario",
            ));
        };
        if expected.iter().ne(managed.keys()) {
            return Err(message(
                "Compose active managed set differs from rediscovery",
            ));
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| message("workspace state lock was poisoned"))?;
        state.commit_active(active.clone(), self.completed_at.clone());
        crate::persist_workspace_state(&self.root.workspace(self.workspace).state_file(), &state)
            .map_err(message)
    }

    async fn remove_orphans(
        &self,
        _desired_managed: &BTreeMap<String, ContainerId>,
    ) -> Result<(), Self::Error> {
        // No production orphan removal is attempted until exact orphan IDs can be independently
        // verified. Leaving an orphan is safer than broad Compose project mutation.
        Ok(())
    }

    async fn cleanup_generated_images(&self) -> Result<(), Self::Error> {
        // Generated image discovery does not yet expose repository-tag/reference evidence. A safe
        // no-op preserves user, base, tagged, cache, and referenced images.
        Ok(())
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the locked Compose transaction and durable recovery boundary remain visible"
)]
fn rebuild_compose_production(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    config: Option<&RepoRelativeConfigPath>,
    no_cache: bool,
) -> Result<(), ProductionWorkflowError> {
    let cancellation = CancellationToken::default();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    runtime
        .block_on(async {
            let signal_cancellation = cancellation.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_cancellation.cancel();
                }
            });
            let result = async {
                let paths = root.workspace(workspace);
                let _lock = crate::LockGuard::acquire(
                    &paths.lock_file(),
                    crate::LockMode::Exclusive,
                    crate::LockBehavior::Wait,
                )
                .map_err(message)?;
                let mut state = load_workspace_state(&paths.state_file())
                    .map_err(message)?
                    .into_state();
                let previous = state.active().cloned();
                let planner = ProductionPlanner {
                    root,
                    workspace,
                    cancellation: &cancellation,
                    replacement: true,
                };
                let desired = RebuildPlanner::prepare(
                    &planner,
                    &paths.checkout(),
                    config.map(RepoRelativeConfigPath::as_path),
                    &state,
                )?;
                planner.validate_frozen(&desired).await?;
                let compose = ProductionComposeRebuild::compose(&desired.plan)?;
                let generation_value = previous
                    .as_ref()
                    .map_or(1, |active| active.generation().get().saturating_add(1));
                if previous
                    .as_ref()
                    .is_some_and(|active| generation_value == active.generation().get())
                {
                    return Err(message("active generation cannot be incremented"));
                }
                let generation = GenerationId::new(generation_value).map_err(message)?;
                let completed_at = crate::create::current_timestamp().map_err(message)?;
                let operation_id = crate::create::random_operation_id().map_err(message)?;
                state.update_desired(
                    desired.profile.clone(),
                    desired.config.clone(),
                    desired.fingerprints.clone(),
                );
                state.set_operation(
                    OperationState::active(
                        ForegroundOperation::Rebuilding,
                        operation_id,
                        completed_at.clone(),
                    )
                    .map_err(message)?,
                );
                state.set_last_error(None);
                crate::persist_workspace_state(&paths.state_file(), &state).map_err(message)?;
                let endpoint =
                    DockerEndpoint::resolve(&ProcessDockerEnvironment).map_err(message)?;
                let connector = endpoint
                    .bollard_connector(BOLLARD_CONTROL_TIMEOUT)
                    .map_err(message)?;
                let adapter = ComposeAdapter::system(
                    endpoint,
                    ProcessRunner::new(root.logs_dir()),
                    root.temp_dir(),
                );
                let production = ProductionComposeRebuild {
                    root,
                    workspace,
                    installation: state.installation_id().clone(),
                    previous: previous.clone(),
                    cancellation: &cancellation,
                    adapter,
                    engine: BollardAdapter::from_connector(&connector),
                    state: Mutex::new(state),
                    completed_at,
                    created: Mutex::new(None),
                };
                let orchestrator = ComposeRebuildOrchestrator::new(production);
                let request = ComposeRebuildRequest {
                    desired: &desired,
                    managed_services: compose.plan.managed_services(),
                    primary_service: &compose.primary_service,
                    generation,
                    previous_primary: previous.as_ref().map(ActiveGeneration::container_id),
                    no_cache,
                };
                match orchestrator.rebuild(request, &cancellation).await {
                    Ok(outcome) => {
                        for warning in outcome.cleanup_warnings {
                            let kind = match warning.kind {
                                ComposeCleanupKind::Orphans => "orphan",
                                ComposeCleanupKind::GeneratedImages => "generated-image",
                            };
                            eprintln!(
                                "cdenv: warning: {kind} cleanup failed safely: {}",
                                warning.source
                            );
                        }
                        Ok(())
                    }
                    Err(error) => {
                        let recovery = error.recovery(generation);
                        let mut state = load_workspace_state(&paths.state_file())
                            .map_err(message)?
                            .into_state();
                        if recovery.is_some() {
                            state.set_interrupted_compose_rebuild(recovery);
                        }
                        state.set_last_error(Some(crate::SanitizedSummary::redact(
                            &error.to_string(),
                            [],
                        )));
                        if matches!(
                            error,
                            ComposeRebuildError::Build { .. }
                                | ComposeRebuildError::CancelledBeforeRecreation
                        ) {
                            state.set_operation(OperationState::idle());
                        }
                        crate::persist_workspace_state(&paths.state_file(), &state)
                            .map_err(message)?;
                        Err(message(error))
                    }
                }
            }
            .await;
            signal.abort();
            result
        })
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))
}

/// Runs the common post-checkout production transaction.
///
/// # Errors
///
/// Returns a typed workspace, planning, dependency, Docker, provisioning, lifecycle, forwarding,
/// or persistence error. Post-clone failures retain the checkout and previous active generation.
pub fn reconcile_production(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    config: Option<&RepoRelativeConfigPath>,
) -> Result<(), ProductionWorkflowError> {
    let paths = root.workspace(workspace);
    if !paths.root().exists() {
        return Err(ProductionWorkflowError::MissingWorkspace(workspace.clone()));
    }
    let state = load_workspace_state(&paths.state_file())?.into_state();
    let installation = state.installation_id().clone();
    let cancellation = CancellationToken::default();
    let operation_time = crate::create::current_timestamp()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation_id = crate::create::random_operation_id()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation = OperationState::active(
        ForegroundOperation::Starting,
        operation_id,
        operation_time.clone(),
    )
    .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let planner = ProductionPlanner {
        root,
        workspace,
        cancellation: &cancellation,
        replacement: false,
    };
    let environment = ProductionEnvironment {
        root,
        workspace,
        installation,
        cancellation: &cancellation,
        no_cache: false,
        prepared_image: None,
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    runtime
        .block_on(async {
            let signal_cancellation = cancellation.clone();
            let signal = tokio::spawn(async move {
                if tokio::signal::ctrl_c().await.is_ok() {
                    signal_cancellation.cancel();
                }
            });
            let result = reconcile_workspace(
                root,
                ReconciliationRequest {
                    workspace,
                    config,
                    operation,
                    completed_at: operation_time,
                },
                &planner,
                &environment,
            )
            .await;
            signal.abort();
            result
        })
        .map(|_| ())
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn post_attach_manifest_wire_matches_agent_for_empty_and_configured_hooks() {
        let mut request = PostAttachManifestRequest {
            generation: "7".to_owned(),
            build_id: cdenv_agent::BUILD_ID.to_owned(),
            protocol_version: cdenv_agent::PROTOCOL_VERSION,
            state_directory: "/home/dev/.cdenv/lifecycle".to_owned(),
            environment_path: "/home/dev/.cdenv/environment/7.bin".to_owned(),
            workspace_folder: "/workspaces/example".to_owned(),
            uid: 1000,
            gid: 1000,
            stages: vec![AgentLifecycleStagePlan {
                stage: AgentLifecycleStage::PostAttach,
                commands: Vec::new(),
            }],
            credential_runtime: None,
            credential_grants: cdenv_core::credentials::CredentialGrants::default(),
        };
        let empty: cdenv_agent::LifecycleRunRequest =
            serde_json::from_slice(&serde_json::to_vec(&request).expect("wire encoding"))
                .expect("agent wire decoding");
        assert!(empty.stages[0].commands.is_empty());

        request.stages[0]
            .commands
            .push(AgentLifecycleCommand::Process {
                process: AgentLifecycleProcess::Shell {
                    command: AgentLifecycleValue(vec![AgentLifecycleValueSegment::Literal {
                        value: "touch attached".to_owned(),
                    }]),
                },
            });
        let configured: cdenv_agent::LifecycleRunRequest =
            serde_json::from_slice(&serde_json::to_vec(&request).expect("wire encoding"))
                .expect("agent wire decoding");
        assert_eq!(configured.stages[0].commands.len(), 1);
    }

    fn lifecycle_exec_fixture() -> (LifecycleExecRecord, crate::ExecInspect) {
        let identity = LifecycleExecIdentity {
            installation: "installation-1".to_owned(),
            workspace: "workspace".to_owned(),
            container: "a".repeat(64),
            generation: "1".to_owned(),
            build: "build-1".to_owned(),
            protocol: 1,
        };
        let inspected = crate::ExecInspect {
            id: crate::ExecId::parse(&"b".repeat(64)).expect("exec"),
            container: ContainerId::parse(&identity.container).expect("container"),
            running: false,
            exit_code: Some(0),
            command: None,
        };
        let record = LifecycleExecRecord {
            identity,
            exec_id: inspected.id.to_string(),
            started: true,
            request: None,
        };
        (record, inspected)
    }

    #[test]
    fn detached_launch_uses_fresh_execution_identity_and_agent_compatible_plan() {
        let (record, _) = lifecycle_exec_fixture();
        let provisioned = crate::AgentProvisioningFacts {
            identity: crate::RemoteAgentIdentity {
                uid: 1000,
                gid: 1000,
                home: "/home/dev".to_owned(),
                shell: "/bin/sh".to_owned(),
            },
            agent_path: "/usr/local/bin/cdenv-agent".to_owned(),
            build_id: cdenv_core::AgentBuildId::parse(&record.identity.build).expect("build"),
            protocol_version: ProtocolVersion::new(1).expect("protocol"),
        };
        let make_request = || {
            detached_lifecycle_request(
                &record.identity,
                &provisioned,
                "/home/dev/.cdenv/environment/1.bin",
                "/workspaces/test",
                vec![AgentLifecycleStagePlan {
                    stage: AgentLifecycleStage::PostStart,
                    commands: vec![AgentLifecycleCommand::Process {
                        process: AgentLifecycleProcess::Shell {
                            command: AgentLifecycleValue(vec![
                                AgentLifecycleValueSegment::Literal {
                                    value: "sleep 30".to_owned(),
                                },
                            ]),
                        },
                    }],
                }],
                None,
                true,
            )
            .expect("request")
        };
        let first = make_request();
        let second = make_request();
        assert_ne!(
            first["ownership"]["execution"],
            second["ownership"]["execution"]
        );
        assert!(owned_request_matches(&first, &record.identity));
        assert_eq!(first["stages"][0]["stage"], "postStart");
        assert_eq!(first["stateDirectory"], "/home/dev/.cdenv/lifecycle");
        #[cfg(target_os = "linux")]
        {
            let decoded: cdenv_agent::LifecycleRunRequest =
                serde_json::from_value(first.clone()).expect("agent request wire compatibility");
            assert_eq!(
                serde_json::to_value(decoded).expect("agent encoding"),
                first
            );
        }
    }

    fn owned_request_fixture(identity: &LifecycleExecIdentity) -> serde_json::Value {
        serde_json::json!({
            "ownership": {
                "installation": identity.installation,
                "workspace": identity.workspace,
                "container": identity.container,
                "execution": "e".repeat(64),
            },
            "generation": identity.generation,
            "buildId": identity.build,
            "protocolVersion": identity.protocol,
        })
    }

    #[test]
    fn owned_cancellation_requires_every_identity_and_exact_terminal_acknowledgement() {
        let (record, _) = lifecycle_exec_fixture();
        let request = owned_request_fixture(&record.identity);
        assert!(owned_request_matches(&request, &record.identity));
        for path in [
            "/ownership/installation",
            "/ownership/workspace",
            "/ownership/container",
            "/ownership/execution",
            "/generation",
            "/buildId",
            "/protocolVersion",
        ] {
            let mut wrong = request.clone();
            *wrong.pointer_mut(path).expect("field") = serde_json::Value::Null;
            assert!(!owned_request_matches(&wrong, &record.identity), "{path}");
        }
        let acknowledgement = serde_json::json!({
            "runnerActive": false,
            "state": {"plan": request, "phase": "cancelled", "runnerPid": null},
        });
        let outcome = |value: &serde_json::Value| {
            owned_cancellation_outcome(
                &request,
                &serde_json::to_vec(value).expect("acknowledgement"),
            )
        };
        assert_eq!(outcome(&acknowledgement), LifecycleStopOutcome::Graceful);
        for phase in ["running", "before", "after", "failed", "indeterminate"] {
            let mut wrong = acknowledgement.clone();
            wrong["state"]["phase"] = serde_json::json!(phase);
            assert_eq!(outcome(&wrong), LifecycleStopOutcome::Indeterminate);
        }
        for (path, value) in [
            ("/runnerActive", serde_json::json!(true)),
            ("/state/runnerPid", serde_json::json!(123)),
            (
                "/state/plan/ownership/execution",
                serde_json::json!("f".repeat(64)),
            ),
        ] {
            let mut wrong = acknowledgement.clone();
            *wrong.pointer_mut(path).expect("field") = value;
            assert_eq!(
                outcome(&wrong),
                LifecycleStopOutcome::Indeterminate,
                "{path}"
            );
        }
        let mut complete = acknowledgement;
        complete["state"]["phase"] = serde_json::json!("complete");
        assert_eq!(outcome(&complete), LifecycleStopOutcome::NotRunning);
        assert_eq!(
            owned_cancellation_outcome(&request, b"{}"),
            LifecycleStopOutcome::Indeterminate
        );
    }

    #[test]
    fn cancelled_pending_one_time_work_keeps_recovery_required() {
        let (record, _) = lifecycle_exec_fixture();
        let mut request = owned_request_fixture(&record.identity);
        request["stages"] = serde_json::json!([
            {"stage": "postCreate", "commands": []},
            {"stage": "postStart", "commands": []},
        ]);
        let mut acknowledgement = serde_json::json!({
            "runnerActive": false,
            "state": {"plan": request, "phase": "cancelled", "runnerPid": null, "stageIndex": 0},
        });
        assert_eq!(
            owned_cancellation_outcome(
                &request,
                &serde_json::to_vec(&acknowledgement).expect("JSON")
            ),
            LifecycleStopOutcome::Indeterminate
        );
        acknowledgement["pendingOneTimeRetryable"] = serde_json::json!(true);
        assert_eq!(
            owned_cancellation_outcome(
                &request,
                &serde_json::to_vec(&acknowledgement).expect("JSON")
            ),
            LifecycleStopOutcome::Graceful
        );
        acknowledgement["pendingOneTimeRetryable"] = serde_json::json!(false);
        acknowledgement["state"]["stageIndex"] = serde_json::json!(1);
        assert_eq!(
            owned_cancellation_outcome(
                &request,
                &serde_json::to_vec(&acknowledgement).expect("JSON")
            ),
            LifecycleStopOutcome::Graceful
        );
    }

    #[test]
    fn lifecycle_completion_requires_exact_identity_and_successful_final_exit() {
        let (record, inspected) = lifecycle_exec_fixture();
        assert_eq!(
            completed_lifecycle_exec(&record, &record.identity, &inspected),
            LifecycleStopOutcome::NotRunning
        );
        for exit_code in [None, Some(1)] {
            let mut unverified = inspected.clone();
            unverified.exit_code = exit_code;
            assert_eq!(
                completed_lifecycle_exec(&record, &record.identity, &unverified),
                LifecycleStopOutcome::Indeterminate
            );
        }
        let mut running = inspected.clone();
        running.running = true;
        assert_eq!(
            completed_lifecycle_exec(&record, &record.identity, &running),
            LifecycleStopOutcome::Indeterminate
        );
        let mut wrong_container = inspected.clone();
        wrong_container.container = ContainerId::parse(&"c".repeat(64)).expect("container");
        assert_eq!(
            completed_lifecycle_exec(&record, &record.identity, &wrong_container),
            LifecycleStopOutcome::Indeterminate
        );
        let mut wrong_exec = inspected;
        wrong_exec.id = crate::ExecId::parse(&"d".repeat(64)).expect("exec");
        assert_eq!(
            completed_lifecycle_exec(&record, &record.identity, &wrong_exec),
            LifecycleStopOutcome::Indeterminate
        );
    }

    #[test]
    fn lifecycle_completion_rejects_created_but_unconfirmed_started_exec() {
        let (mut record, inspected) = lifecycle_exec_fixture();
        record.started = false;
        assert_eq!(
            completed_lifecycle_exec(&record, &record.identity, &inspected),
            LifecycleStopOutcome::Indeterminate
        );
    }

    #[test]
    fn lifecycle_completion_rejects_every_persisted_identity_mismatch() {
        let (record, inspected) = lifecycle_exec_fixture();
        for field in 0..6 {
            let mut expected = record.identity.clone();
            match field {
                0 => expected.installation.push('x'),
                1 => expected.workspace.push('x'),
                2 => expected.container = "c".repeat(64),
                3 => expected.generation = "2".to_owned(),
                4 => expected.build.push('x'),
                _ => expected.protocol += 1,
            }
            assert_eq!(
                completed_lifecycle_exec(&record, &expected, &inspected),
                LifecycleStopOutcome::Indeterminate
            );
        }
    }

    #[test]
    fn lifecycle_exec_record_is_private_and_replaced_for_each_start() {
        use std::os::unix::fs::PermissionsExt as _;
        let parent = std::fs::canonicalize("/tmp").expect("temp root");
        let temporary = tempfile::tempdir_in(parent).expect("temporary");
        let path = temporary.path().join("runtime/lifecycle-exec.json");
        let (record, inspected) = lifecycle_exec_fixture();
        persist_lifecycle_exec(&path, &record.identity, &inspected.id, true).expect("persist");
        assert_eq!(
            load_lifecycle_exec(&path).expect("load").exec_id,
            inspected.id.as_str()
        );
        let replacement = crate::ExecId::parse(&"e".repeat(64)).expect("exec");
        persist_lifecycle_exec(&path, &record.identity, &replacement, true).expect("replace");
        assert_eq!(
            load_lifecycle_exec(&path)
                .expect("load replacement")
                .exec_id,
            replacement.as_str()
        );
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("mode");
        assert!(load_lifecycle_exec(&path).is_none());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        let link = temporary.path().join("linked.json");
        std::os::unix::fs::symlink(&path, &link).expect("symlink");
        assert!(load_lifecycle_exec(&link).is_none());
        std::fs::hard_link(&path, temporary.path().join("hardlink")).expect("hardlink");
        assert!(load_lifecycle_exec(&path).is_none());
    }

    #[test]
    fn lifecycle_exec_open_rejects_leaf_replacement_after_inspection() {
        let parent = std::fs::canonicalize("/tmp").expect("temp root");
        let temporary = tempfile::tempdir_in(parent).expect("temporary");
        let path = temporary.path().join("record.json");
        let (record, inspected) = lifecycle_exec_fixture();
        persist_lifecycle_exec(&path, &record.identity, &inspected.id, true).expect("persist");
        let metadata = std::fs::symlink_metadata(&path).expect("inspection");
        let saved = temporary.path().join("saved.json");
        std::fs::rename(&path, &saved).expect("move inspected inode");
        std::os::unix::fs::symlink(&saved, &path).expect("replacement link");
        assert!(open_lifecycle_exec(&path, &metadata).is_none());
        std::fs::remove_file(&path).expect("remove link");
        nix::unistd::mkfifo(&path, nix::sys::stat::Mode::S_IRUSR).expect("replacement FIFO");
        assert!(open_lifecycle_exec(&path, &metadata).is_none());
        std::fs::remove_file(&path).expect("remove FIFO");
        persist_lifecycle_exec(&path, &record.identity, &inspected.id, true).expect("new inode");
        assert!(open_lifecycle_exec(&path, &metadata).is_none());
    }

    #[test]
    fn lifecycle_stop_fails_closed_without_persisted_runner_control_identity() {
        let temporary = tempfile::tempdir().expect("temporary");
        assert!(load_lifecycle_exec(&temporary.path().join("missing.json")).is_none());
    }
}
