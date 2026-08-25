//! Host lifecycle readiness orchestration component contracts.

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use cdenv_cli::{
    BackgroundReadiness, BackgroundRunnerOutcome, CancellationToken, ContainerLifecycle,
    HostLifecycle, HostLifecycleExecutor, LifecycleCommandFailure, LifecycleInput,
    LifecycleMutation, LifecycleMutationFailure, LifecycleMutationOutcome, LifecycleOperation,
    LifecycleOrchestrationRequest, LifecycleOrchestrator, LifecycleRetryClassification,
    LifecycleScenario,
};
use cdenv_devcontainer::{
    ConfigPath, HostSubstitutionInputs, ImageMetadata, LifecycleCommand, LifecyclePlan,
    LifecycleProcess, LifecycleStage, LifecycleStagePlan, ParseLimits, StableIdentityLabels,
    merge_image_metadata, parse_jsonc, plan_lifecycle, validate_profile,
};
use tempfile::TempDir;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct FakeError(&'static str);

impl fmt::Display for FakeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for FakeError {}

#[derive(Clone)]
struct FakeHost {
    events: Arc<Mutex<Vec<String>>>,
}

impl HostLifecycle for FakeHost {
    type Error = FakeError;

    async fn initialize(
        &self,
        _: &LifecycleStagePlan,
        _: &Path,
        _: LifecycleInput,
        _: &CancellationToken,
    ) -> Result<(), Self::Error> {
        self.events
            .lock()
            .expect("events")
            .push("initialize".to_owned());
        Ok(())
    }
}

#[derive(Clone)]
struct FakeMutation {
    events: Arc<Mutex<Vec<String>>>,
    outcome: LifecycleMutationOutcome,
}

impl LifecycleMutation for FakeMutation {
    type Error = FakeError;

    async fn mutate(
        &self,
        operation: LifecycleOperation,
        scenario: LifecycleScenario,
        _: &CancellationToken,
    ) -> Result<LifecycleMutationOutcome, LifecycleMutationFailure<Self::Error>> {
        self.events
            .lock()
            .expect("events")
            .push(format!("mutate:{operation:?}:{scenario:?}"));
        Ok(self.outcome)
    }
}

#[derive(Clone)]
struct FakeRuntime {
    events: Arc<Mutex<Vec<String>>>,
    runner_active: Arc<Mutex<bool>>,
    stage_failure: Option<(LifecycleStage, FailureKind)>,
    forwarding_failure: bool,
    runner_outcome: Option<BackgroundRunnerOutcome>,
}

#[derive(Clone, Copy)]
enum FailureKind {
    Before,
    Definite,
    Indeterminate,
}

impl ContainerLifecycle for FakeRuntime {
    type Error = FakeError;
    type Provisioned = &'static str;
    type InitialEnvironment = &'static str;
    type Environment = &'static str;
    type Forwarding = &'static str;

    async fn execute_stage(
        &self,
        plan: &LifecycleStagePlan,
        _: &CancellationToken,
    ) -> Result<(), LifecycleCommandFailure<Self::Error>> {
        self.events
            .lock()
            .expect("events")
            .push(format!("stage:{:?}", plan.stage));
        match self.stage_failure {
            Some((stage, FailureKind::Before)) if stage == plan.stage => {
                Err(LifecycleCommandFailure::BeforeStart(FakeError("before")))
            }
            Some((stage, FailureKind::Definite)) if stage == plan.stage => {
                Err(LifecycleCommandFailure::Definite(FakeError("failed")))
            }
            Some((stage, FailureKind::Indeterminate)) if stage == plan.stage => Err(
                LifecycleCommandFailure::Indeterminate(FakeError("interrupted")),
            ),
            _ => Ok(()),
        }
    }

    async fn provision(&self, _: &CancellationToken) -> Result<Self::Provisioned, Self::Error> {
        self.events
            .lock()
            .expect("events")
            .push("provision".to_owned());
        Ok("provisioned")
    }

    async fn capture_environment<'a>(
        &'a self,
        _: &'a Self::Provisioned,
        _: &'a CancellationToken,
    ) -> Result<Self::InitialEnvironment, Self::Error> {
        self.events
            .lock()
            .expect("events")
            .push("capture".to_owned());
        Ok("initial")
    }

    async fn recapture_environment<'a>(
        &'a self,
        _: &'a Self::Provisioned,
        _: &'a Self::InitialEnvironment,
        _: &'a CancellationToken,
    ) -> Result<Self::Environment, Self::Error> {
        self.events
            .lock()
            .expect("events")
            .push("recapture".to_owned());
        Ok("ssh")
    }

    async fn forwarding_ready<'a>(
        &'a self,
        _: &'a Self::Environment,
        _: &'a CancellationToken,
    ) -> Result<Self::Forwarding, Self::Error> {
        self.events
            .lock()
            .expect("events")
            .push("forwarding".to_owned());
        if self.forwarding_failure {
            Err(FakeError("forwarding unavailable"))
        } else {
            Ok("forwarding-ready")
        }
    }

    async fn start_or_verify_runner<'a>(
        &'a self,
        later: &'a [LifecycleStagePlan],
        _: &'a CancellationToken,
    ) -> Result<BackgroundRunnerOutcome, Self::Error> {
        let stages = later
            .iter()
            .map(|plan| format!("{:?}", plan.stage))
            .collect::<Vec<_>>()
            .join(",");
        let mut active = self.runner_active.lock().expect("runner");
        let outcome = self.runner_outcome.unwrap_or_else(|| {
            if *active {
                BackgroundRunnerOutcome::VerifiedRunning
            } else {
                *active = true;
                BackgroundRunnerOutcome::Started
            }
        });
        self.events.lock().expect("events").push(format!(
            "runner:{}:{stages}",
            if outcome == BackgroundRunnerOutcome::Started {
                "start"
            } else {
                "verify"
            }
        ));
        Ok(outcome)
    }
}

fn plan(wait_for: &str) -> LifecyclePlan {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
    let source = format!(
        r#"{{"image":"x","initializeCommand":"initialize","onCreateCommand":"on-create","updateContentCommand":"update","postCreateCommand":"post-create","postStartCommand":"post-start","waitFor":"{wait_for}"}}"#
    );
    let document = parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("parse");
    let profile = validate_profile(&document).expect("profile");
    let effective = merge_image_metadata(&[], &profile).expect("metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    plan_lifecycle(
        &effective,
        &HostSubstitutionInputs {
            local_workspace_folder: "/checkout/project",
            container_workspace_folder: "/workspaces/project",
            local_env: &BTreeMap::new(),
            identity_labels: &labels,
        },
    )
    .expect("lifecycle")
}

type Events = Arc<Mutex<Vec<String>>>;
type FakeOrchestrator = LifecycleOrchestrator<FakeHost, FakeMutation, FakeRuntime>;

fn fixture(outcome: LifecycleMutationOutcome) -> (Events, FakeOrchestrator) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let host = FakeHost {
        events: events.clone(),
    };
    let mutation = FakeMutation {
        events: events.clone(),
        outcome,
    };
    let runtime = FakeRuntime {
        events: events.clone(),
        runner_active: Arc::new(Mutex::new(false)),
        stage_failure: None,
        forwarding_failure: false,
        runner_outcome: None,
    };
    (events, LifecycleOrchestrator::new(host, mutation, runtime))
}

#[tokio::test]
async fn every_wait_for_and_scenario_has_exact_foreground_and_readiness_order() {
    let waits = [
        ("initializeCommand", vec![]),
        ("onCreateCommand", vec!["stage:OnCreate"]),
        (
            "updateContentCommand",
            vec!["stage:OnCreate", "stage:UpdateContent"],
        ),
        (
            "postCreateCommand",
            vec!["stage:OnCreate", "stage:UpdateContent", "stage:PostCreate"],
        ),
        (
            "postStartCommand",
            vec![
                "stage:OnCreate",
                "stage:UpdateContent",
                "stage:PostCreate",
                "stage:PostStart",
            ],
        ),
    ];
    for scenario in [
        LifecycleScenario::Image,
        LifecycleScenario::Dockerfile,
        LifecycleScenario::Compose,
    ] {
        for (wait, foreground) in &waits {
            let plan = plan(wait);
            let (events, orchestrator) = fixture(LifecycleMutationOutcome {
                new_generation: true,
                actual_start: true,
            });
            let request = LifecycleOrchestrationRequest {
                plan: &plan,
                checkout: Path::new("/checkout/project"),
                operation: LifecycleOperation::Create,
                scenario,
                input: LifecycleInput::Inherit,
            };
            orchestrator
                .orchestrate(&request, &CancellationToken::default())
                .await
                .expect("readiness");
            let actual = events.lock().expect("events").clone();
            let mut expected = vec![
                "initialize".to_owned(),
                format!("mutate:Create:{scenario:?}"),
                "provision".to_owned(),
                "capture".to_owned(),
            ];
            expected.extend(foreground.iter().map(|event| (*event).to_owned()));
            expected.extend(["recapture", "forwarding"].map(str::to_owned));
            if wait != &"postStartCommand" {
                expected.push(format!(
                    "runner:start:{}",
                    match *wait {
                        "initializeCommand" => "OnCreate,UpdateContent,PostCreate,PostStart",
                        "onCreateCommand" => "UpdateContent,PostCreate,PostStart",
                        "updateContentCommand" => "PostCreate,PostStart",
                        "postCreateCommand" => "PostStart",
                        _ => unreachable!(),
                    }
                ));
            }
            assert_eq!(actual, expected, "scenario {scenario:?}, waitFor {wait}");
        }
    }
}

#[tokio::test]
async fn feature_command_groups_reach_the_runtime_before_repository_groups() {
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
    let document = parse_jsonc(
        &path,
        br#"{"image":"x","onCreateCommand":"repository"}"#,
        ParseLimits::default(),
    )
    .expect("parse");
    let profile = validate_profile(&document).expect("profile");
    let feature = ImageMetadata::from_value(
        "feature",
        &serde_json::json!({"id":"feature","onCreateCommand":"feature"}),
    )
    .expect("feature metadata");
    let effective = merge_image_metadata(&[feature], &profile).expect("metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    let plan = plan_lifecycle(
        &effective,
        &HostSubstitutionInputs {
            local_workspace_folder: "/checkout/project",
            container_workspace_folder: "/workspaces/project",
            local_env: &BTreeMap::new(),
            identity_labels: &labels,
        },
    )
    .expect("lifecycle");
    let values = plan
        .on_create
        .commands
        .iter()
        .map(|command| match command {
            LifecycleCommand::Process(LifecycleProcess::Shell(value)) => {
                value.resolve(&BTreeMap::new()).expose().to_owned()
            }
            _ => panic!("fixture commands are scalar shell forms"),
        })
        .collect::<Vec<_>>();
    assert_eq!(values, ["feature", "repository"]);

    let (events, orchestrator) = fixture(LifecycleMutationOutcome {
        new_generation: true,
        actual_start: false,
    });
    orchestrator
        .orchestrate(
            &LifecycleOrchestrationRequest {
                plan: &plan,
                checkout: Path::new("/checkout/project"),
                operation: LifecycleOperation::Create,
                scenario: LifecycleScenario::Image,
                input: LifecycleInput::Closed,
            },
            &CancellationToken::default(),
        )
        .await
        .expect("readiness");
    assert!(
        events
            .lock()
            .expect("events")
            .iter()
            .any(|event| event == "stage:OnCreate")
    );
}

#[tokio::test]
async fn provisioning_repeats_but_existing_background_work_is_verified_not_duplicated() {
    let plan = plan("onCreateCommand");
    let (events, orchestrator) = fixture(LifecycleMutationOutcome {
        new_generation: true,
        actual_start: true,
    });
    let request = LifecycleOrchestrationRequest {
        plan: &plan,
        checkout: Path::new("/checkout/project"),
        operation: LifecycleOperation::Up,
        scenario: LifecycleScenario::Image,
        input: LifecycleInput::Closed,
    };

    let first = orchestrator
        .orchestrate(&request, &CancellationToken::default())
        .await
        .expect("first readiness");
    let second = orchestrator
        .orchestrate(&request, &CancellationToken::default())
        .await
        .expect("repeat readiness");

    assert_eq!(
        first.background,
        BackgroundReadiness::Runner(BackgroundRunnerOutcome::Started)
    );
    assert_eq!(
        second.background,
        BackgroundReadiness::Runner(BackgroundRunnerOutcome::VerifiedRunning)
    );
    let events = events.lock().expect("events");
    assert_eq!(
        events.iter().filter(|event| *event == "provision").count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.starts_with("runner:start:"))
            .count(),
        1
    );
}

#[tokio::test]
async fn post_start_is_absent_for_idempotent_up_and_runs_only_after_an_actual_start() {
    let plan = plan("postStartCommand");
    for (actual_start, expected) in [(false, 0), (true, 1)] {
        let (events, orchestrator) = fixture(LifecycleMutationOutcome {
            new_generation: false,
            actual_start,
        });
        let request = LifecycleOrchestrationRequest {
            plan: &plan,
            checkout: Path::new("/checkout/project"),
            operation: LifecycleOperation::Up,
            scenario: LifecycleScenario::Compose,
            input: LifecycleInput::Closed,
        };
        orchestrator
            .orchestrate(&request, &CancellationToken::default())
            .await
            .expect("readiness");
        assert_eq!(
            events
                .lock()
                .expect("events")
                .iter()
                .filter(|event| *event == "stage:PostStart")
                .count(),
            expected
        );
    }
}

#[tokio::test]
async fn failures_classify_retry_safety_and_stop_before_provisioning_or_later_stages() {
    for (kind, classification) in [
        (FailureKind::Before, LifecycleRetryClassification::SafeRetry),
        (
            FailureKind::Definite,
            LifecycleRetryClassification::DefiniteFailure,
        ),
        (
            FailureKind::Indeterminate,
            LifecycleRetryClassification::IndeterminateOneTime,
        ),
    ] {
        let plan = plan("postCreateCommand");
        let events = Arc::new(Mutex::new(Vec::new()));
        let orchestrator = LifecycleOrchestrator::new(
            FakeHost {
                events: events.clone(),
            },
            FakeMutation {
                events: events.clone(),
                outcome: LifecycleMutationOutcome {
                    new_generation: true,
                    actual_start: true,
                },
            },
            FakeRuntime {
                events: events.clone(),
                runner_active: Arc::new(Mutex::new(false)),
                stage_failure: Some((LifecycleStage::UpdateContent, kind)),
                forwarding_failure: false,
                runner_outcome: None,
            },
        );
        let error = orchestrator
            .orchestrate(
                &LifecycleOrchestrationRequest {
                    plan: &plan,
                    checkout: Path::new("/checkout/project"),
                    operation: LifecycleOperation::Create,
                    scenario: LifecycleScenario::Image,
                    input: LifecycleInput::Closed,
                },
                &CancellationToken::default(),
            )
            .await
            .expect_err("stage failure");
        assert_eq!(error.retry_classification(), classification);
        let events = events.lock().expect("events");
        assert!(!events.iter().any(|event| event == "stage:PostCreate"));
        assert!(events.iter().any(|event| event == "provision"));
        assert!(events.iter().any(|event| event == "capture"));
        assert!(!events.iter().any(|event| event == "recapture"));
    }
}

#[tokio::test]
async fn forwarding_failure_follows_recapture_and_returns_no_commit_result() {
    let plan = plan("updateContentCommand");
    let events = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = LifecycleOrchestrator::new(
        FakeHost {
            events: events.clone(),
        },
        FakeMutation {
            events: events.clone(),
            outcome: LifecycleMutationOutcome {
                new_generation: true,
                actual_start: true,
            },
        },
        FakeRuntime {
            events: events.clone(),
            runner_active: Arc::new(Mutex::new(false)),
            stage_failure: None,
            forwarding_failure: true,
            runner_outcome: None,
        },
    );

    let result = orchestrator
        .orchestrate(
            &LifecycleOrchestrationRequest {
                plan: &plan,
                checkout: Path::new("/checkout/project"),
                operation: LifecycleOperation::Rebuild,
                scenario: LifecycleScenario::Dockerfile,
                input: LifecycleInput::Closed,
            },
            &CancellationToken::default(),
        )
        .await;

    let error = result.expect_err("forwarding readiness failure");
    assert_eq!(
        error.retry_classification(),
        LifecycleRetryClassification::SafeRetry
    );
    let events = events.lock().expect("events");
    let capture = events
        .iter()
        .position(|event| event == "capture")
        .expect("capture");
    let readiness = events
        .iter()
        .position(|event| event == "stage:UpdateContent")
        .expect("readiness stage");
    let recapture = events
        .iter()
        .position(|event| event == "recapture")
        .expect("recapture");
    assert!(capture < readiness && readiness < recapture);
    assert!(events.ends_with(&["recapture".to_owned(), "forwarding".to_owned()]));
}

#[tokio::test]
async fn cancellation_before_mutation_is_safe_and_returns_no_readiness_result() {
    let plan = plan("updateContentCommand");
    let (events, orchestrator) = fixture(LifecycleMutationOutcome {
        new_generation: true,
        actual_start: true,
    });
    let cancellation = CancellationToken::default();
    cancellation.cancel();

    let error = orchestrator
        .orchestrate(
            &LifecycleOrchestrationRequest {
                plan: &plan,
                checkout: Path::new("/checkout/project"),
                operation: LifecycleOperation::Up,
                scenario: LifecycleScenario::Image,
                input: LifecycleInput::Closed,
            },
            &cancellation,
        )
        .await
        .expect_err("cancelled readiness");

    assert_eq!(
        error.retry_classification(),
        LifecycleRetryClassification::SafeRetry
    );
    assert_eq!(events.lock().expect("events").as_slice(), ["initialize"]);
}

#[tokio::test]
async fn failed_existing_runner_is_classified_as_background_failure() {
    let plan = plan("onCreateCommand");
    let events = Arc::new(Mutex::new(Vec::new()));
    let orchestrator = LifecycleOrchestrator::new(
        FakeHost {
            events: events.clone(),
        },
        FakeMutation {
            events: events.clone(),
            outcome: LifecycleMutationOutcome {
                new_generation: true,
                actual_start: true,
            },
        },
        FakeRuntime {
            events,
            runner_active: Arc::new(Mutex::new(true)),
            stage_failure: None,
            forwarding_failure: false,
            runner_outcome: Some(BackgroundRunnerOutcome::Failed),
        },
    );

    let error = orchestrator
        .orchestrate(
            &LifecycleOrchestrationRequest {
                plan: &plan,
                checkout: Path::new("/checkout/project"),
                operation: LifecycleOperation::Up,
                scenario: LifecycleScenario::Compose,
                input: LifecycleInput::Closed,
            },
            &CancellationToken::default(),
        )
        .await
        .expect_err("background failure");

    assert_eq!(
        error.retry_classification(),
        LifecycleRetryClassification::BackgroundFailure
    );
}

#[tokio::test]
async fn production_host_cancellation_terminates_the_owned_process_group() {
    let directory = TempDir::new().expect("temporary directory");
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
    let document = parse_jsonc(
        &path,
        br#"{"image":"x","initializeCommand":"sleep 30; touch survived"}"#,
        ParseLimits::default(),
    )
    .expect("parse");
    let profile = validate_profile(&document).expect("profile");
    let effective = merge_image_metadata(&[], &profile).expect("metadata");
    let labels = StableIdentityLabels::new("installation", "workspace");
    let lifecycle = plan_lifecycle(
        &effective,
        &HostSubstitutionInputs {
            local_workspace_folder: directory.path().to_str().expect("UTF-8 path"),
            container_workspace_folder: "/workspaces/project",
            local_env: &BTreeMap::new(),
            identity_labels: &labels,
        },
    )
    .expect("lifecycle");
    let cancellation = CancellationToken::default();
    let trigger = cancellation.clone();
    let cancellation_task = tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        trigger.cancel();
    });

    let result = HostLifecycleExecutor
        .initialize(
            &lifecycle.initialize,
            directory.path(),
            LifecycleInput::Closed,
            &cancellation,
        )
        .await;
    cancellation_task.await.expect("cancellation task");

    assert!(result.is_err());
    assert!(!directory.path().join("survived").exists());
}

#[tokio::test]
async fn production_host_executor_runs_all_forms_and_closes_parallel_stdin() {
    let directory = TempDir::new().expect("temporary directory");
    let path = ConfigPath::parse(".devcontainer/devcontainer.json").expect("path");
    let labels = StableIdentityLabels::new("installation", "workspace");
    for source in [
        r#"{"image":"x","initializeCommand":"printf scalar- >> order"}"#,
        r#"{"image":"x","initializeCommand":{"stdin":"if read value; then exit 9; fi; printf repository >> order","argv":["/bin/sh","-c","printf exec >> order"]}}"#,
    ] {
        let document =
            parse_jsonc(&path, source.as_bytes(), ParseLimits::default()).expect("parse");
        let profile = validate_profile(&document).expect("profile");
        let effective = merge_image_metadata(&[], &profile).expect("metadata");
        let lifecycle = plan_lifecycle(
            &effective,
            &HostSubstitutionInputs {
                local_workspace_folder: directory.path().to_str().expect("UTF-8 path"),
                container_workspace_folder: "/workspaces/project",
                local_env: &BTreeMap::new(),
                identity_labels: &labels,
            },
        )
        .expect("lifecycle");
        HostLifecycleExecutor
            .initialize(
                &lifecycle.initialize,
                directory.path(),
                LifecycleInput::Inherit,
                &CancellationToken::default(),
            )
            .await
            .expect("host lifecycle");
    }

    let output = std::fs::read_to_string(directory.path().join("order")).expect("order");
    assert!(output.starts_with("scalar-"));
    assert!(output.contains("repository"));
    assert!(output.contains("exec"));
}
