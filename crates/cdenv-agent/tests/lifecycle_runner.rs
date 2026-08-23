#![cfg(target_os = "linux")]
//! Linux lifecycle execution and recovery contracts.

use std::collections::BTreeMap;
use std::fs;
use std::time::Duration;

use cdenv_agent::{
    BUILD_ID, EnvironmentCaptureRequest, EnvironmentProbe, LifecycleCommand, LifecyclePhase,
    LifecycleProcess, LifecycleRunRequest, LifecycleStage, LifecycleStagePlan, LifecycleValue,
    LifecycleValueSegment, PROTOCOL_VERSION, cancel_lifecycle, capture_environment,
    inspect_lifecycle, run_lifecycle,
};
use tempfile::TempDir;

fn literal(value: &str) -> LifecycleValue {
    LifecycleValue(vec![LifecycleValueSegment::Literal {
        value: value.to_owned(),
    }])
}

fn shell(value: &str) -> LifecycleCommand {
    LifecycleCommand::Process {
        process: LifecycleProcess::Shell {
            command: literal(value),
        },
    }
}

fn exec(arguments: &[&str]) -> LifecycleCommand {
    LifecycleCommand::Process {
        process: LifecycleProcess::Exec {
            arguments: arguments.iter().map(|value| literal(value)).collect(),
        },
    }
}

fn request(
    directory: &TempDir,
    generation: &str,
    stages: Vec<LifecycleStagePlan>,
) -> LifecycleRunRequest {
    let capture = capture_environment(&EnvironmentCaptureRequest {
        generation: generation.to_owned(),
        state_directory: directory.path().display().to_string(),
        probe: EnvironmentProbe::None,
        remote_environment: BTreeMap::new(),
    })
    .expect("environment capture");
    LifecycleRunRequest {
        generation: generation.to_owned(),
        build_id: BUILD_ID.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        state_directory: directory.path().display().to_string(),
        environment_path: capture.snapshot_path,
        workspace_folder: directory.path().display().to_string(),
        uid: nix::unistd::geteuid().as_raw(),
        gid: nix::unistd::getegid().as_raw(),
        stages,
    }
}

#[test]
fn command_forms_preserve_order_environment_cwd_closed_stdin_and_prefixed_logs() {
    let directory = TempDir::new().expect("temporary directory");
    let output = directory.path().join("order");
    let environment_output = directory.path().join("environment");
    let deferred_output = directory.path().join("deferred");
    let deferred = LifecycleCommand::Process {
        process: LifecycleProcess::Exec {
            arguments: vec![
                literal("/bin/sh"),
                literal("-c"),
                literal(&format!(
                    "printf '%s' \"$1\" > {}",
                    deferred_output.display()
                )),
                literal("cdenv-lifecycle"),
                LifecycleValue(vec![LifecycleValueSegment::ContainerEnvironment {
                    name: "HOME".to_owned(),
                    default: "fallback-home".to_owned(),
                }]),
            ],
        },
    };
    let mut parallel = BTreeMap::new();
    parallel.insert(
        "a".to_owned(),
        LifecycleProcess::Shell {
            command: literal("if read value; then exit 9; fi; printf alpha; printf a >> order"),
        },
    );
    parallel.insert(
        "b".to_owned(),
        LifecycleProcess::Exec {
            arguments: ["/bin/sh", "-c", "printf beta; printf b >> order"]
                .iter()
                .map(|value| literal(value))
                .collect(),
        },
    );
    let plan = LifecycleStagePlan {
        stage: LifecycleStage::OnCreate,
        commands: vec![
            shell("printf feature- >> order"),
            exec(&["/bin/sh", "-c", "printf repository- >> order"]),
            shell(&format!(
                "printf '%s' \"$PWD:$HOME\" > {}",
                environment_output.display()
            )),
            deferred,
            LifecycleCommand::Parallel {
                processes: parallel,
            },
        ],
    };
    let request = request(&directory, "forms", vec![plan]);

    let state = run_lifecycle(&request).expect("lifecycle success");

    assert_eq!(state.phase, LifecyclePhase::Complete);
    let order = fs::read_to_string(output).expect("ordered output");
    assert!(order.starts_with("feature-repository-"));
    let environment = fs::read_to_string(environment_output).expect("environment output");
    assert!(environment.starts_with(directory.path().to_str().expect("UTF-8 path")));
    assert!(environment.contains(':'));
    assert!(
        !fs::read_to_string(deferred_output)
            .expect("deferred environment output")
            .is_empty()
    );
    let log = fs::read_to_string(directory.path().join(".cdenv-lifecycle-forms.log"))
        .expect("bounded log");
    assert!(log.contains("[a] alpha"));
    assert!(log.contains("[b] beta"));
}

#[test]
fn repeated_runner_requests_do_not_duplicate_success_and_failure_skips_later_stages() {
    let directory = TempDir::new().expect("temporary directory");
    let once = LifecycleStagePlan {
        stage: LifecycleStage::OnCreate,
        commands: vec![shell("printf x >> once")],
    };
    let repeat_request = request(&directory, "repeat", vec![once]);
    run_lifecycle(&repeat_request).expect("first run");
    run_lifecycle(&repeat_request).expect("verification run");
    assert_eq!(
        fs::read_to_string(directory.path().join("once")).expect("once"),
        "x"
    );

    let failing = LifecycleStagePlan {
        stage: LifecycleStage::OnCreate,
        commands: vec![shell("exit 7")],
    };
    let skipped = LifecycleStagePlan {
        stage: LifecycleStage::UpdateContent,
        commands: vec![shell("touch should-not-exist")],
    };
    let failure_request = request(&directory, "failure", vec![failing, skipped]);
    assert!(run_lifecycle(&failure_request).is_err());
    assert!(!directory.path().join("should-not-exist").exists());
    assert_eq!(
        inspect_lifecycle(&failure_request)
            .expect("failure checkpoint")
            .state
            .phase,
        LifecyclePhase::Failed
    );
}

#[test]
fn a_running_one_time_checkpoint_becomes_indeterminate_on_restart() {
    let directory = TempDir::new().expect("temporary directory");
    let request = request(
        &directory,
        "crash",
        vec![LifecycleStagePlan {
            stage: LifecycleStage::OnCreate,
            commands: vec![shell("touch must-not-run")],
        }],
    );
    run_lifecycle(&LifecycleRunRequest {
        stages: vec![],
        ..request.clone()
    })
    .expect("create checkpoint");
    let state_path = directory.path().join(".cdenv-lifecycle-crash.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).expect("state")).expect("state JSON");
    state["plan"] = serde_json::to_value(&request).expect("request JSON");
    state["stageIndex"] = serde_json::json!(0);
    state["commandIndex"] = serde_json::json!(0);
    state["phase"] = serde_json::json!("running");
    fs::write(
        &state_path,
        serde_json::to_vec(&state).expect("state encoding"),
    )
    .expect("state write");

    assert!(run_lifecycle(&request).is_err());
    assert_eq!(
        inspect_lifecycle(&request).expect("inspection").state.phase,
        LifecyclePhase::Indeterminate
    );
    assert!(!directory.path().join("must-not-run").exists());
}

#[test]
fn long_running_later_stage_is_reported_active_and_cancels_definitely() {
    let directory = TempDir::new().expect("temporary directory");
    let request = request(
        &directory,
        "long-running",
        vec![LifecycleStagePlan {
            stage: LifecycleStage::PostStart,
            commands: vec![shell("sleep 30")],
        }],
    );
    let runner_request = request.clone();
    let runner = std::thread::spawn(move || run_lifecycle(&runner_request));
    let mut observed_running = false;
    for _ in 0..100 {
        if inspect_lifecycle(&request).is_ok_and(|inspection| {
            inspection.runner_active && inspection.state.phase == LifecyclePhase::Running
        }) {
            observed_running = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(observed_running);

    let inspection = cancel_lifecycle(&request, Duration::from_secs(5)).expect("cancellation");
    let state = runner
        .join()
        .expect("runner thread")
        .expect("definite cancellation");

    assert!(!inspection.runner_active);
    assert_eq!(state.phase, LifecyclePhase::Cancelled);
}

#[test]
fn cancellation_terminates_owned_process_groups_and_marks_one_time_work_indeterminate() {
    let directory = TempDir::new().expect("temporary directory");
    let request = request(
        &directory,
        "cancel",
        vec![LifecycleStagePlan {
            stage: LifecycleStage::PostCreate,
            commands: vec![shell("sleep 30")],
        }],
    );
    let runner_request = request.clone();
    let runner = std::thread::spawn(move || run_lifecycle(&runner_request));
    for _ in 0..100 {
        if inspect_lifecycle(&request).is_ok_and(|inspection| {
            inspection.runner_active && inspection.state.phase == LifecyclePhase::Running
        }) {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let inspection = cancel_lifecycle(&request, Duration::from_secs(5)).expect("cancellation");
    let _ = runner.join().expect("runner thread");

    assert!(!inspection.runner_active);
    assert_eq!(inspection.state.phase, LifecyclePhase::Indeterminate);
}
