//! Immutable lifecycle command planning and checkpoint transitions.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::{
    DeferredString, EffectiveMetadata, HostSubstitutionInputs, RawCommand, SubstitutionError,
    SubstitutionProperty, WaitFor, substitute_host,
};

/// Lifecycle execution stage in specification order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum LifecycleStage {
    /// Host-side initialization, repeatable before container mutation.
    Initialize,
    /// First command in a newly created generation.
    OnCreate,
    /// Content-update command in a newly created generation.
    UpdateContent,
    /// Final create command in a newly created generation.
    PostCreate,
    /// Command after each actual successful container start.
    PostStart,
    /// Command once per newly established transport.
    PostAttach,
}

impl LifecycleStage {
    /// Returns whether this stage belongs to the immutable one-time generation sequence.
    #[must_use]
    pub const fn is_one_time(self) -> bool {
        matches!(
            self,
            Self::OnCreate | Self::UpdateContent | Self::PostCreate
        )
    }

    /// Returns whether a stage may continue in the background after readiness.
    #[must_use]
    pub const fn background_eligible(self) -> bool {
        matches!(
            self,
            Self::OnCreate | Self::UpdateContent | Self::PostCreate | Self::PostStart
        )
    }

    /// Returns the event which permits this stage to execute.
    #[must_use]
    pub const fn trigger(self) -> LifecycleTrigger {
        match self {
            Self::Initialize => LifecycleTrigger::BeforeContainerMutation,
            Self::OnCreate | Self::UpdateContent | Self::PostCreate => {
                LifecycleTrigger::NewGeneration
            }
            Self::PostStart => LifecycleTrigger::SuccessfulStart,
            Self::PostAttach => LifecycleTrigger::NewTransport,
        }
    }
}

/// Event semantics which gate lifecycle stage invocation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleTrigger {
    /// Repeatable host-side invocation before create, up, or rebuild mutation.
    BeforeContainerMutation,
    /// Exactly once in one newly created generation.
    NewGeneration,
    /// After an actual successful start, never an idempotent already-running `up`.
    SuccessfulStart,
    /// Once for each new SSH or forwarding transport, not each multiplexed channel.
    NewTransport,
}

/// One command process, either shell-evaluated or direct argv.
#[derive(Clone, PartialEq, Eq)]
pub enum LifecycleProcess {
    /// Execute through the applicable `/bin/sh`.
    Shell(DeferredString),
    /// Execute directly without a shell.
    Exec(Vec<DeferredString>),
}

impl std::fmt::Debug for LifecycleProcess {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shell(_) => formatter.write_str("Shell(<redacted>)"),
            Self::Exec(arguments) => formatter
                .debug_tuple("Exec")
                .field(&format_args!("{} redacted arguments", arguments.len()))
                .finish(),
        }
    }
}

/// One sequential lifecycle command group.
#[derive(Clone, PartialEq, Eq)]
pub enum LifecycleCommand {
    /// A single process which may inherit interactive standard input.
    Process(LifecycleProcess),
    /// Stable-keyed processes executed concurrently with closed standard input.
    Parallel(BTreeMap<String, LifecycleProcess>),
}

impl LifecycleCommand {
    /// Returns whether the command receives closed standard input.
    #[must_use]
    pub const fn stdin_is_closed(&self) -> bool {
        matches!(self, Self::Parallel(_))
    }

    /// Returns whether any command value still needs actual container environment data.
    #[must_use]
    pub fn requires_container_environment(&self) -> bool {
        match self {
            Self::Process(process) => process_requires_container_environment(process),
            Self::Parallel(processes) => processes
                .values()
                .any(process_requires_container_environment),
        }
    }
}

impl std::fmt::Debug for LifecycleCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Process(process) => process.fmt(formatter),
            Self::Parallel(processes) => formatter
                .debug_struct("Parallel")
                .field("keys", &processes.keys().collect::<Vec<_>>())
                .finish(),
        }
    }
}

/// Commands for one stage, sequential between entries and concurrent within keyed groups.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleStagePlan {
    /// Stage semantics.
    pub stage: LifecycleStage,
    /// Source-ordered commands. Feature contributions precede repository commands.
    pub commands: Vec<LifecycleCommand>,
}

/// Immutable effective lifecycle commands captured for one desired or active generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecyclePlan {
    /// Repeatable host initialization commands.
    pub initialize: LifecycleStagePlan,
    /// Immutable one-time generation commands.
    pub on_create: LifecycleStagePlan,
    /// Immutable one-time generation commands.
    pub update_content: LifecycleStagePlan,
    /// Immutable one-time generation commands.
    pub post_create: LifecycleStagePlan,
    /// Commands for an actual successful start.
    pub post_start: LifecycleStagePlan,
    /// Commands once per new transport.
    pub post_attach: LifecycleStagePlan,
    /// Stage after which provisioning, capture, and forwarding may proceed.
    pub readiness: LifecycleStage,
}

impl LifecyclePlan {
    /// Returns one stage plan.
    #[must_use]
    pub const fn stage(&self, stage: LifecycleStage) -> &LifecycleStagePlan {
        match stage {
            LifecycleStage::Initialize => &self.initialize,
            LifecycleStage::OnCreate => &self.on_create,
            LifecycleStage::UpdateContent => &self.update_content,
            LifecycleStage::PostCreate => &self.post_create,
            LifecycleStage::PostStart => &self.post_start,
            LifecycleStage::PostAttach => &self.post_attach,
        }
    }

    /// Reports whether a stage can run in the background for this plan.
    #[must_use]
    pub fn runs_after_readiness(&self, stage: LifecycleStage) -> bool {
        stage.background_eligible() && stage > self.readiness
    }
}

/// Plans lifecycle substitutions and immutable stage command lists.
///
/// Metadata merging has already retained Feature contributions before the repository contribution.
///
/// # Errors
///
/// Returns the stage and substitution error without including command contents.
pub fn plan_lifecycle(
    effective: &EffectiveMetadata,
    substitutions: &HostSubstitutionInputs<'_>,
) -> Result<LifecyclePlan, LifecyclePlanningError> {
    let lifecycle = &effective.lifecycle;
    Ok(LifecyclePlan {
        initialize: stage_plan(
            LifecycleStage::Initialize,
            &lifecycle.initialize,
            substitutions,
        )?,
        on_create: stage_plan(
            LifecycleStage::OnCreate,
            &lifecycle.on_create,
            substitutions,
        )?,
        update_content: stage_plan(
            LifecycleStage::UpdateContent,
            &lifecycle.update_content,
            substitutions,
        )?,
        post_create: stage_plan(
            LifecycleStage::PostCreate,
            &lifecycle.post_create,
            substitutions,
        )?,
        post_start: stage_plan(
            LifecycleStage::PostStart,
            &lifecycle.post_start,
            substitutions,
        )?,
        post_attach: stage_plan(
            LifecycleStage::PostAttach,
            &lifecycle.post_attach,
            substitutions,
        )?,
        readiness: readiness_stage(effective.wait_for),
    })
}

fn stage_plan(
    stage: LifecycleStage,
    commands: &[RawCommand],
    substitutions: &HostSubstitutionInputs<'_>,
) -> Result<LifecycleStagePlan, LifecyclePlanningError> {
    let commands = commands
        .iter()
        .map(|command| normalize_command(stage, command, substitutions))
        .collect::<Result<_, _>>()?;
    Ok(LifecycleStagePlan { stage, commands })
}

fn normalize_command(
    stage: LifecycleStage,
    command: &RawCommand,
    substitutions: &HostSubstitutionInputs<'_>,
) -> Result<LifecycleCommand, LifecyclePlanningError> {
    let process = |command: &str| {
        let value = substitute_host(
            SubstitutionProperty::LifecycleCommand,
            command,
            substitutions,
        )
        .map_err(|source| LifecyclePlanningError::Substitution { stage, source })?;
        if stage == LifecycleStage::Initialize && value.requires_container_environment() {
            return Err(LifecyclePlanningError::RuntimeValueInInitialize);
        }
        Ok(value)
    };
    match command {
        RawCommand::Shell(command) => process(command)
            .map(LifecycleProcess::Shell)
            .map(LifecycleCommand::Process),
        RawCommand::Exec(arguments) => arguments
            .iter()
            .map(|argument| process(argument))
            .collect::<Result<_, _>>()
            .map(LifecycleProcess::Exec)
            .map(LifecycleCommand::Process),
        RawCommand::Parallel(commands) => commands
            .iter()
            .map(|(key, command)| {
                let process = match command {
                    crate::CommandValue::Shell(command) => {
                        LifecycleProcess::Shell(process(command)?)
                    }
                    crate::CommandValue::Exec(arguments) => LifecycleProcess::Exec(
                        arguments
                            .iter()
                            .map(|argument| process(argument))
                            .collect::<Result<_, _>>()?,
                    ),
                };
                Ok((key.clone(), process))
            })
            .collect::<Result<_, _>>()
            .map(LifecycleCommand::Parallel),
    }
}

fn readiness_stage(wait_for: Option<WaitFor>) -> LifecycleStage {
    match wait_for.unwrap_or(WaitFor::UpdateContent) {
        WaitFor::Initialize => LifecycleStage::Initialize,
        WaitFor::OnCreate => LifecycleStage::OnCreate,
        WaitFor::UpdateContent => LifecycleStage::UpdateContent,
        WaitFor::PostCreate => LifecycleStage::PostCreate,
        WaitFor::PostStart => LifecycleStage::PostStart,
    }
}

fn process_requires_container_environment(process: &LifecycleProcess) -> bool {
    match process {
        LifecycleProcess::Shell(command) => command.requires_container_environment(),
        LifecycleProcess::Exec(arguments) => arguments
            .iter()
            .any(DeferredString::requires_container_environment),
    }
}

/// Lifecycle plan construction failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum LifecyclePlanningError {
    /// Host initialization cannot depend on an environment from a container not yet started.
    #[error("initializeCommand cannot use runtime container environment substitution")]
    RuntimeValueInInitialize,
    /// A lifecycle value contains an invalid or disallowed substitution.
    #[error("invalid lifecycle substitution in {stage:?}: {source}")]
    Substitution {
        /// Affected stage.
        stage: LifecycleStage,
        /// Typed substitution failure, which never includes the command value.
        source: SubstitutionError,
    },
}

/// Durable execution state for the immutable one-time lifecycle sequence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct LifecycleCheckpoint {
    completed_through: Option<LifecycleStage>,
    running: Option<LifecycleStage>,
    failure: Option<LifecycleStage>,
    indeterminate: bool,
}

impl LifecycleCheckpoint {
    /// Returns the last successfully completed one-time stage.
    #[must_use]
    pub const fn completed_through(self) -> Option<LifecycleStage> {
        self.completed_through
    }

    /// Returns the currently running one-time stage.
    #[must_use]
    pub const fn running(self) -> Option<LifecycleStage> {
        self.running
    }

    /// Returns the definitely failed one-time stage.
    #[must_use]
    pub const fn failure(self) -> Option<LifecycleStage> {
        self.failure
    }

    /// Reports whether interruption made execution outcome unknowable.
    #[must_use]
    pub const fn is_indeterminate(self) -> bool {
        self.indeterminate
    }

    /// Starts the next one-time stage.
    ///
    /// # Errors
    ///
    /// Rejects duplicate, out-of-order, non-one-time, failed, running, or indeterminate execution.
    pub fn start(&mut self, stage: LifecycleStage) -> Result<(), LifecycleTransitionError> {
        if !stage.is_one_time()
            || self.running.is_some()
            || self.failure.is_some()
            || self.indeterminate
        {
            return Err(LifecycleTransitionError::InvalidStart { stage });
        }
        let expected = next_stage(self.completed_through);
        if expected != Some(stage) {
            return Err(LifecycleTransitionError::OutOfOrder { stage, expected });
        }
        self.running = Some(stage);
        Ok(())
    }

    /// Marks the running stage successful.
    ///
    /// # Errors
    ///
    /// Rejects completion when that stage is not running.
    pub fn succeed(&mut self, stage: LifecycleStage) -> Result<(), LifecycleTransitionError> {
        if self.running != Some(stage) {
            return Err(LifecycleTransitionError::NotRunning { stage });
        }
        self.running = None;
        self.completed_through = Some(stage);
        Ok(())
    }

    /// Marks the running stage definitely failed and prevents later stages.
    ///
    /// # Errors
    ///
    /// Rejects failure when that stage is not running.
    pub fn fail(&mut self, stage: LifecycleStage) -> Result<(), LifecycleTransitionError> {
        if self.running != Some(stage) {
            return Err(LifecycleTransitionError::NotRunning { stage });
        }
        self.running = None;
        self.failure = Some(stage);
        Ok(())
    }

    /// Marks a running one-time stage indeterminate after cancellation or a crash.
    ///
    /// # Errors
    ///
    /// Rejects the transition when no one-time stage is running.
    pub fn interrupt(&mut self) -> Result<(), LifecycleTransitionError> {
        if self.running.take().is_none() {
            return Err(LifecycleTransitionError::NothingRunning);
        }
        self.indeterminate = true;
        Ok(())
    }
}

const fn next_stage(completed: Option<LifecycleStage>) -> Option<LifecycleStage> {
    match completed {
        None => Some(LifecycleStage::OnCreate),
        Some(LifecycleStage::OnCreate) => Some(LifecycleStage::UpdateContent),
        Some(LifecycleStage::UpdateContent) => Some(LifecycleStage::PostCreate),
        _ => None,
    }
}

/// Invalid one-time lifecycle checkpoint transition.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum LifecycleTransitionError {
    /// The stage cannot start in the current checkpoint state.
    #[error("cannot start lifecycle stage {stage:?} in the current checkpoint state")]
    InvalidStart {
        /// Rejected stage.
        stage: LifecycleStage,
    },
    /// The stage skipped or repeated part of the immutable sequence.
    #[error("lifecycle stage {stage:?} is out of order; expected {expected:?}")]
    OutOfOrder {
        /// Rejected stage.
        stage: LifecycleStage,
        /// Required next stage, or none when complete.
        expected: Option<LifecycleStage>,
    },
    /// Success/failure was reported for a stage which was not running.
    #[error("lifecycle stage {stage:?} is not running")]
    NotRunning {
        /// Rejected stage.
        stage: LifecycleStage,
    },
    /// Interruption was reported with no running stage.
    #[error("no lifecycle stage is running")]
    NothingRunning,
}

/// Clones the desired commands when a generation is created, preventing later desired changes
/// from mutating active-generation lifecycle execution.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActiveGenerationLifecycle {
    plan: LifecyclePlan,
    checkpoint: LifecycleCheckpoint,
}

impl ActiveGenerationLifecycle {
    /// Captures immutable commands for a new generation.
    #[must_use]
    pub fn capture(desired: &LifecyclePlan) -> Self {
        Self {
            plan: desired.clone(),
            checkpoint: LifecycleCheckpoint::default(),
        }
    }

    /// Returns the generation-owned command plan.
    #[must_use]
    pub const fn plan(&self) -> &LifecyclePlan {
        &self.plan
    }

    /// Returns mutable checkpoint state without allowing command replacement.
    #[must_use]
    pub fn checkpoint_mut(&mut self) -> &mut LifecycleCheckpoint {
        &mut self.checkpoint
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        ConfigPath, ParseLimits, StableIdentityLabels, merge_image_metadata, parse_jsonc,
        validate_profile,
    };

    fn plan(source: &str, metadata: &[serde_json::Value]) -> LifecyclePlan {
        let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("fixture path");
        let document =
            parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("JSONC");
        let profile = validate_profile(&document).expect("profile");
        let metadata = metadata
            .iter()
            .enumerate()
            .map(|(index, value)| {
                crate::ImageMetadata::from_value(format!("feature-{index}"), value)
                    .expect("metadata")
            })
            .collect::<Vec<_>>();
        let effective = merge_image_metadata(&metadata, &profile).expect("merge");
        let environment = BTreeMap::new();
        let labels = StableIdentityLabels::new("install", "workspace");
        plan_lifecycle(
            &effective,
            &HostSubstitutionInputs {
                local_workspace_folder: "/checkout/workspace",
                container_workspace_folder: "/workspaces/workspace",
                local_env: &environment,
                identity_labels: &labels,
            },
        )
        .expect("lifecycle plan")
    }

    #[test]
    fn feature_commands_precede_repository_and_parallel_groups_stay_concurrent() {
        let result = plan(
            r#"{"image":"x","onCreateCommand":{"repo-a":"echo a","repo-b":["echo","b"]}}"#,
            &[serde_json::json!({"id":"feature","onCreateCommand":"feature"})],
        );
        assert!(matches!(
            result.on_create.commands[0],
            LifecycleCommand::Process(_)
        ));
        let LifecycleCommand::Parallel(commands) = &result.on_create.commands[1] else {
            panic!("repository object stays parallel")
        };
        assert_eq!(
            commands.keys().collect::<Vec<_>>(),
            vec!["repo-a", "repo-b"]
        );
        assert!(result.on_create.commands[1].stdin_is_closed());
    }

    #[test]
    fn wait_for_controls_background_eligibility_and_default_is_update_content() {
        let default = plan(r#"{"image":"x"}"#, &[]);
        let early = plan(r#"{"image":"x","waitFor":"onCreateCommand"}"#, &[]);
        assert_eq!(default.readiness, LifecycleStage::UpdateContent);
        assert!(early.runs_after_readiness(LifecycleStage::PostCreate));
    }

    #[test]
    fn checkpoint_transitions_enforce_one_time_order_and_indeterminate_recovery() {
        let mut checkpoint = LifecycleCheckpoint::default();
        assert!(matches!(
            checkpoint.start(LifecycleStage::PostCreate),
            Err(LifecycleTransitionError::OutOfOrder { .. })
        ));
        checkpoint.start(LifecycleStage::OnCreate).expect("start");
        checkpoint
            .succeed(LifecycleStage::OnCreate)
            .expect("success");
        checkpoint
            .start(LifecycleStage::UpdateContent)
            .expect("start");
        checkpoint.interrupt().expect("interrupt");
        assert!(checkpoint.is_indeterminate());
        assert!(matches!(
            checkpoint.start(LifecycleStage::UpdateContent),
            Err(LifecycleTransitionError::InvalidStart { .. })
        ));
    }

    #[test]
    fn runtime_only_lifecycle_substitution_remains_deferred_until_container_capture() {
        let result = plan(
            r#"{"image":"x","postStartCommand":"echo ${containerEnv:MARKER}"}"#,
            &[],
        );
        assert!(result.post_start.commands[0].requires_container_environment());
    }

    #[test]
    fn active_generation_keeps_captured_commands_when_desired_changes() {
        let original = plan(r#"{"image":"x","postStartCommand":"original"}"#, &[]);
        let replacement = plan(r#"{"image":"x","postStartCommand":"changed"}"#, &[]);
        let active = ActiveGenerationLifecycle::capture(&original);
        assert_eq!(active.plan(), &original);
        assert_ne!(active.plan(), &replacement);
    }
}
