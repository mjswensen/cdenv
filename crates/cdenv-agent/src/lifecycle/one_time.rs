//! Generation-owned progress shared by fresh detached executions.

use super::{
    File, Flock, FlockArg, LifecycleError, LifecyclePhase, LifecycleRunRequest,
    LifecycleRunnerState, LifecycleStagePlan, Path, PathBuf, atomic_write, fs, open_lock,
    validate_restricted_file,
};
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Plan {
    installation: String,
    workspace: String,
    container: String,
    generation: String,
    build_id: String,
    protocol_version: u32,
    uid: u32,
    gid: u32,
    workspace_folder: String,
    stages: Vec<LifecycleStagePlan>,
}

impl Plan {
    fn from_request(request: &LifecycleRunRequest) -> Option<Self> {
        let owner = request.ownership.as_ref()?;
        let stages: Vec<_> = request
            .stages
            .iter()
            .filter(|stage| stage.stage.is_one_time() && !stage.commands.is_empty())
            .cloned()
            .collect();
        if stages.is_empty() {
            return None;
        }
        Some(Self {
            installation: owner.installation.clone(),
            workspace: owner.workspace.clone(),
            container: owner.container.clone(),
            generation: request.generation.clone(),
            build_id: request.build_id.clone(),
            protocol_version: request.protocol_version,
            uid: request.uid,
            gid: request.gid,
            workspace_folder: request.workspace_folder.clone(),
            stages,
        })
    }

    fn count(&self) -> usize {
        self.stages.iter().map(|stage| stage.commands.len()).sum()
    }
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Checkpoint {
    plan: Plan,
    completed: usize,
    phase: LifecyclePhase,
}

fn path(request: &LifecycleRunRequest) -> PathBuf {
    Path::new(&request.state_directory).join(format!(".cdenv-once-{}.json", request.generation))
}

fn load(path: &Path, expected: &Plan) -> Result<Checkpoint, LifecycleError> {
    validate_restricted_file(path)?;
    let checkpoint: Checkpoint = serde_json::from_slice(
        &fs::read(path).map_err(|source| LifecycleError::StateIo { source })?,
    )
    .map_err(|_| LifecycleError::UnsafeState)?;
    if checkpoint.plan != *expected {
        return Err(LifecycleError::PlanMismatch);
    }
    let count = expected.count();
    if checkpoint.completed > count
        || (checkpoint.phase == LifecyclePhase::Complete && checkpoint.completed != count)
        || (checkpoint.phase != LifecyclePhase::Complete && checkpoint.completed == count)
        || !matches!(
            checkpoint.phase,
            LifecyclePhase::Before
                | LifecyclePhase::Running
                | LifecyclePhase::Complete
                | LifecyclePhase::Failed
                | LifecyclePhase::Indeterminate
        )
    {
        return Err(LifecycleError::UnsafeState);
    }
    Ok(checkpoint)
}

fn expected_plan(request: &LifecycleRunRequest) -> Result<Option<Plan>, LifecycleError> {
    let plan = Plan::from_request(request);
    if plan.is_none() && request.ownership.is_some() {
        // Omitting every one-time stage must not bypass existing generation progress.
        match fs::symlink_metadata(path(request)) {
            Ok(_) => return Err(LifecycleError::PlanMismatch),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => return Err(LifecycleError::StateIo { source }),
        }
    }
    Ok(plan)
}

pub(super) struct OneTimeRun {
    _lock: Flock<File>,
    checkpoint: Checkpoint,
    path: PathBuf,
}

impl OneTimeRun {
    pub(super) fn open(
        request: &LifecycleRunRequest,
        state: &LifecycleRunnerState,
    ) -> Result<Option<Self>, LifecycleError> {
        let Some(plan) = expected_plan(request)? else {
            return Ok(None);
        };
        let path = path(request);
        let lock = open_lock(&path.with_extension("lock"))?;
        let lock = Flock::lock(lock, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, _)| LifecycleError::AlreadyRunning)?;
        let missing = match fs::symlink_metadata(&path) {
            Ok(_) => false,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
            Err(source) => return Err(LifecycleError::StateIo { source }),
        };
        let checkpoint = if missing
            && request.initialize_one_time
            && state.phase == LifecyclePhase::Before
            && state.stage_index == 0
            && state.command_index == 0
        {
            Checkpoint {
                plan,
                completed: 0,
                phase: LifecyclePhase::Before,
            }
        } else {
            load(&path, &plan)?
        };
        let mut run = Self {
            _lock: lock,
            checkpoint,
            path,
        };
        if run.checkpoint.phase == LifecyclePhase::Running {
            run.checkpoint.phase = LifecyclePhase::Indeterminate;
        }
        run.store()?;
        match run.checkpoint.phase {
            LifecyclePhase::Indeterminate => Err(LifecycleError::Indeterminate),
            LifecyclePhase::Failed => Err(LifecycleError::OneTimeFailed),
            _ => Ok(Some(run)),
        }
    }

    pub(super) fn already_completed(&self, index: usize) -> bool {
        index < self.checkpoint.completed
    }

    pub(super) fn begin(&mut self, index: usize) -> Result<(), LifecycleError> {
        if index != self.checkpoint.completed || self.checkpoint.phase != LifecyclePhase::Before {
            return Err(LifecycleError::UnsafeState);
        }
        self.checkpoint.phase = LifecyclePhase::Running;
        self.store()
    }

    pub(super) fn complete(&mut self) -> Result<(), LifecycleError> {
        self.checkpoint.completed += 1;
        self.checkpoint.phase = if self.checkpoint.completed == self.checkpoint.plan.count() {
            LifecyclePhase::Complete
        } else {
            LifecyclePhase::Before
        };
        self.store()
    }

    pub(super) fn fail(&mut self, phase: LifecyclePhase) -> Result<(), LifecycleError> {
        self.checkpoint.phase = phase;
        self.store()
    }

    fn store(&self) -> Result<(), LifecycleError> {
        atomic_write(
            &self.path,
            &serde_json::to_vec(&self.checkpoint).map_err(|_| LifecycleError::UnsafeState)?,
        )
    }
}

pub(super) fn pending_retryable(
    request: &LifecycleRunRequest,
    state: &LifecycleRunnerState,
) -> Result<bool, LifecycleError> {
    let Some(plan) = expected_plan(request)? else {
        return Ok(false);
    };
    let checkpoint = load(&path(request), &plan)?;
    Ok(state.phase == LifecyclePhase::Cancelled
        && state.failure.is_none()
        && checkpoint.phase == LifecyclePhase::Before)
}
