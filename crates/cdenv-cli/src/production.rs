//! Production composition for environment lifecycle command paths.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::io::Write as _;
use std::path::{Path, PathBuf};

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
use thiserror::Error;

use crate::agent_provisioning::AgentProvisioningEngine;
use crate::bollard::{COMPOSE_PROJECT_LABEL, COMPOSE_SERVICE_LABEL};
use crate::image_orchestration::ImageContainerMatchState;
use crate::lifecycle_orchestration::HostLifecycle;
use crate::{
    ActiveForwarding, ActiveGeneration, ActiveScenario, AgentArtifactIdentity,
    AgentArtifactProvider, AgentEnvironmentCapturer, AgentEnvironmentRequest,
    AgentProvisionRequest, AgentProvisioner, BOLLARD_CONTROL_TIMEOUT, BollardAdapter,
    CancellationToken, CategoryDrift, CdenvRoot, ComposeAdapter, ComposeBaseRequest,
    ComposeLifecycleOrchestrator, ComposeProject, ComposeUpRequest, CreateComposeRequest,
    DeclaredForward, DesiredForwardingPlan, DockerBuildContext, DockerBuildRequest,
    DockerCommandProbe, DockerEndpoint, DockerResourceIdentity, DockerfileInput, DownRequest,
    EnvironmentReconciler, EnvironmentReconciliationRequest, EnvironmentStopOutcome,
    EnvironmentTransition, FeatureSourcePolicy, FeatureSourceResolver, FingerprintKeyState,
    GeneratedFeature, GeneratedImagePlan, GeneratedUidGidUpdate, HostLifecycleExecutor,
    ImageContainerCreateRequest, ImageContainerError, ImageContainerOrchestrator, ImageId,
    Installation, LifecycleCheckpoint, LifecycleInput, LifecycleRunnerStop, LifecycleStage,
    LifecycleStopOutcome, LockBehavior, LockGuard, LockMode, ManagedEnvironmentStop,
    OperationState, PlanFingerprintCategory, PlanFingerprints, PreparedDesiredPlan,
    ProcessDockerEnvironment, ProcessRunner, ProvisionedState, ReadyEnvironment,
    ReconciliationPlanner, ReconciliationRequest, RecordedComposeRequest, RepoRelativeConfigPath,
    RuntimeReconciliation, SanitizedSummary, ScopedForwardingSupervisor, SupervisorClaim,
    SupervisorForward, SupervisorManifest, UidGidMutation, WorkspaceState, WorkspaceStateError,
    credential_status, discover_and_read_config, down_workspace, ensure_workspace_ssh_identity,
    load_supervisor_state, load_workspace_ssh_assets, load_workspace_state,
    persist_workspace_state, reconcile_workspace, start_detached_supervisor, stop_supervisor,
    supervisor_control_token, supervisor_status,
};

const PROFILE: &str = "cdenv-devcontainer-v1";
const AGENT_HOST_KEY: &str = "/var/lib/cdenv/ssh/host-key";
const AGENT_AUTHORIZED_KEY: &str = "/var/lib/cdenv/ssh/authorized-key";

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
        let selected = explicit.map_or_else(
            || PathBuf::from(current.desired_devcontainer_config().as_str()),
            Path::to_path_buf,
        );
        let source = discover_and_read_config(checkout, Some(&selected))
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
        let credentials = credential_status(self.root, self.workspace);
        if !credentials.grants().is_empty() {
            return Err(message(
                "configured credential capabilities require integration that is unavailable; container lifecycle was not started",
            ));
        }
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

struct ProductionEnvironment<'a> {
    root: &'a CdenvRoot,
    workspace: &'a WorkspaceName,
    installation: cdenv_core::InstallationId,
    cancellation: &'a CancellationToken,
    no_cache: bool,
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
                (
                    orchestrator
                        .create(&create, self.cancellation)
                        .await
                        .map_err(message)?,
                    EnvironmentTransition::Created,
                )
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
            .finish_readiness(request, facts.clone(), transition, engine.clone())
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
    async fn finish_readiness(
        &self,
        request: EnvironmentReconciliationRequest<'_, ProductionPlan>,
        facts: EnvironmentFacts,
        transition: EnvironmentTransition,
        engine: BollardAdapter,
    ) -> Result<ReadyEnvironment, WorkflowMessage> {
        let plan = &request.desired.plan;
        ensure_workspace_ssh_identity(self.root, self.workspace).map_err(message)?;
        let ssh = load_workspace_ssh_assets(self.root, self.workspace).map_err(message)?;
        let assets = ssh.provision_assets(AGENT_HOST_KEY, AGENT_AUTHORIZED_KEY);
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
        execute_lifecycle(
            &engine,
            &facts.container,
            plan.runtime.remote_user.as_str(),
            provisioned.agent_path.as_str(),
            initial.snapshot_path(),
            plan.runtime.workspace.folder.as_str(),
            &plan.lifecycle,
            transition,
            request.active.is_none(),
            LifecycleExecution::Foreground,
            self.cancellation,
        )
        .await?;
        let ssh_environment = capturer
            .recapture_for_ssh(&environment_request, &initial, self.cancellation)
            .await
            .map_err(message)?;
        let forwarding = reconcile_forwarding(
            self.root,
            self.workspace,
            &self.installation,
            &facts,
            &provisioned,
            &plan.ports,
            request.active.map(ActiveGeneration::forwarding),
        )
        .await?;
        execute_lifecycle(
            &engine,
            &facts.container,
            plan.runtime.remote_user.as_str(),
            provisioned.agent_path.as_str(),
            ssh_environment.snapshot_path(),
            plan.runtime.workspace.folder.as_str(),
            &plan.lifecycle,
            transition,
            request.active.is_none(),
            LifecycleExecution::Background,
            self.cancellation,
        )
        .await?;
        let running = [
            request
                .active
                .is_none()
                .then_some(&plan.lifecycle.on_create),
            request
                .active
                .is_none()
                .then_some(&plan.lifecycle.update_content),
            request
                .active
                .is_none()
                .then_some(&plan.lifecycle.post_create),
            (transition != EnvironmentTransition::AlreadyRunning)
                .then_some(&plan.lifecycle.post_start),
        ]
        .into_iter()
        .flatten()
        .find(|stage| stage.stage > plan.lifecycle.readiness && !stage.commands.is_empty())
        .map(|stage| map_stage(stage.stage));
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
            request.desired.fingerprints.clone(),
            plan.feature_digests.clone(),
            checkpoint,
            forwarding,
            state,
        );
        Ok(ReadyEnvironment {
            active,
            transition,
            runtime: if request.drift.runtime {
                RuntimeReconciliation::Applied
            } else {
                RuntimeReconciliation::Current
            },
        })
    }
}

async fn reconcile_forwarding(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    installation: &cdenv_core::InstallationId,
    facts: &EnvironmentFacts,
    provisioned: &crate::AgentProvisioningFacts,
    ports: &cdenv_devcontainer::PortPlan,
    previous: Option<&ActiveForwarding>,
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
    if previous.is_some_and(|previous| previous.requested() == desired.requested())
        && let Ok(state) = load_supervisor_state(&paths.supervisor_state_file())
    {
        let token = supervisor_control_token(&state).to_owned();
        if let Ok(live) = supervisor_status(&paths.supervisor_socket(), &token, &claim).await {
            return active_forwarding(&desired, &host_build, &live);
        }
    }
    let rollback = previous
        .filter(|previous| !previous.requested().is_empty())
        .map(|previous| previous.requested().to_vec());
    if let Ok(state) = load_supervisor_state(&paths.supervisor_state_file()) {
        let token = supervisor_control_token(&state).to_owned();
        stop_supervisor(&paths.supervisor_socket(), &token, &claim)
            .await
            .map_err(message)?;
    }
    if desired.is_empty() {
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
                )
                .await;
            }
            return Err(primary);
        }
    };
    active_forwarding(&desired, &host_build, &state)
}

async fn start_forwarding_supervisor(
    root: &CdenvRoot,
    workspace: &WorkspaceName,
    installation: &cdenv_core::InstallationId,
    facts: &EnvironmentFacts,
    provisioned: &crate::AgentProvisioningFacts,
    host_build: &cdenv_core::AgentBuildId,
    requested: &[DeclaredForward],
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
    agent: &str,
    environment: &str,
    workspace: &str,
    plan: &cdenv_devcontainer::LifecyclePlan,
    transition: EnvironmentTransition,
    new_generation: bool,
    execution: LifecycleExecution,
    cancellation: &CancellationToken,
) -> Result<(), WorkflowMessage> {
    let mut stages = Vec::new();
    if new_generation {
        stages.extend([&plan.on_create, &plan.update_content, &plan.post_create]);
    }
    if transition != EnvironmentTransition::AlreadyRunning {
        stages.push(&plan.post_start);
    }
    let mut background = String::new();
    for stage in stages {
        let stage_execution = if stage.stage <= plan.readiness {
            LifecycleExecution::Foreground
        } else {
            LifecycleExecution::Background
        };
        if stage_execution != execution {
            continue;
        }
        for group in &stage.commands {
            let script = lifecycle_group_script(group)?;
            if execution == LifecycleExecution::Background {
                background.push('(');
                background.push_str(&script);
                background.push_str(")||exit $?;");
                continue;
            }
            let command = lifecycle_agent_command(agent, environment, script);
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
        let command = lifecycle_agent_command(agent, environment, background);
        let exec = crate::ExecCommand {
            container,
            command: &command,
            user: Some(user),
            working_directory: Some(workspace),
            environment: &[],
        };
        let detached = engine.create_detached_exec(&exec).await.map_err(message)?;
        engine
            .start_detached_exec(&detached)
            .await
            .map_err(message)?;
    }
    Ok(())
}

fn lifecycle_agent_command(agent: &str, environment: &str, script: String) -> Vec<String> {
    vec![
        agent.to_owned(),
        "run-environment".to_owned(),
        environment.to_owned(),
        "--".to_owned(),
        "/bin/sh".to_owned(),
        "-c".to_owned(),
        script,
        "--".to_owned(),
    ]
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

const fn host_architecture() -> cdenv_core::ContainerArchitecture {
    #[cfg(target_arch = "aarch64")]
    {
        cdenv_core::ContainerArchitecture::Aarch64
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        cdenv_core::ContainerArchitecture::X86_64
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

struct ProductionLifecycleStop;

impl LifecycleRunnerStop for ProductionLifecycleStop {
    type Error = WorkflowMessage;

    async fn cancel(
        &self,
        active: Option<&ActiveGeneration>,
        _grace: std::time::Duration,
    ) -> Result<LifecycleStopOutcome, Self::Error> {
        Ok(
            if active.is_some_and(|active| active.lifecycle().running().is_some()) {
                // Production currently has no authenticated runner-control record. Never guess an
                // exec identity: retain the one-time-work ambiguity for a required rebuild.
                LifecycleStopOutcome::Indeterminate
            } else {
                LifecycleStopOutcome::NotRunning
            },
        )
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
                &ProductionLifecycleStop,
                &managed,
            )
            .await;
            signal.abort();
            result
        })
        .map(|_| ())
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))
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
#[expect(
    clippy::too_many_lines,
    reason = "the production replacement and failure persistence boundaries remain visible"
)]
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
    let previous = initial.active().cloned();
    let cancellation = CancellationToken::default();
    let planner = ProductionPlanner {
        root,
        workspace,
        cancellation: &cancellation,
        replacement: true,
    };
    let desired = planner
        .prepare(
            &paths.checkout(),
            config.map(RepoRelativeConfigPath::as_path),
            &initial,
            FeatureSourcePolicy::FrozenOffline,
        )
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    runtime
        .block_on(planner.preflight(&desired, FeatureSourcePolicy::FrozenOffline))
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;

    // This operation intentionally preserves containers, images, volumes, networks, checkout
    // changes, SSH identity, and credential grants. Only verified active resources are stopped.
    down_production(root, workspace)?;

    let _lock = LockGuard::acquire(&paths.lock_file(), LockMode::Exclusive, LockBehavior::Wait)
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let mut state = load_workspace_state(&paths.state_file())?.into_state();
    let completed_at = crate::create::current_timestamp()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation_id = crate::create::random_operation_id()
        .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    let operation = OperationState::active(
        ForegroundOperation::Rebuilding,
        operation_id,
        completed_at.clone(),
    )
    .map_err(|error| ProductionWorkflowError::Workflow(error.to_string()))?;
    state.update_desired(
        desired.profile.clone(),
        desired.config.clone(),
        desired.fingerprints.clone(),
    );
    state.set_operation(operation);
    state.set_last_error(None);
    persist_workspace_state(&paths.state_file(), &state)?;

    let environment = ProductionEnvironment {
        root,
        workspace,
        installation: state.installation_id().clone(),
        cancellation: &cancellation,
        no_cache,
    };
    let result = runtime.block_on(async {
        let signal_cancellation = cancellation.clone();
        let signal = tokio::spawn(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                signal_cancellation.cancel();
            }
        });
        let result = environment
            .reconcile(EnvironmentReconciliationRequest {
                desired: &desired,
                active: None,
                drift: CategoryDrift::default(),
            })
            .await;
        signal.abort();
        result
    });
    match result {
        Ok(ready)
            if ready.active.generation() == desired.plan.generation
                && previous.as_ref().is_none_or(|old| {
                    old.container_id() != ready.active.container_id()
                        && ready.active.generation().get() > old.generation().get()
                }) =>
        {
            state.commit_active(ready.active, completed_at);
            persist_workspace_state(&paths.state_file(), &state)?;
            Ok(())
        }
        Ok(_) => {
            // Keep rebuilding intent durable: an invariant violation may leave an owned candidate
            // requiring authenticated recovery rather than an ordinary lifecycle retry.
            state.set_last_error(Some(SanitizedSummary::redact(
                "rebuild replacement identity invariant failed",
                [],
            )));
            persist_workspace_state(&paths.state_file(), &state)?;
            Err(ProductionWorkflowError::Workflow(
                "rebuild replacement identity invariant failed".to_owned(),
            ))
        }
        Err(error) => {
            // Compose recreation is non-atomic, and image rollback can require a later verified
            // restart. Retain explicit interrupted intent instead of claiming an idle outcome.
            state.set_last_error(Some(SanitizedSummary::redact(&error.to_string(), [])));
            persist_workspace_state(&paths.state_file(), &state)?;
            Err(ProductionWorkflowError::Workflow(error.to_string()))
        }
    }
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
