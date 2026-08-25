//! Black-box tests for versioned output envelopes and stream separation.

use std::process::ExitCode;

use cdenv_cli::{
    ApplicationError, CommandKind, ErrorEnvelope, OutputFormat, OutputRenderError, SuccessEnvelope,
    invoke, port_output_warnings, render_application_result, render_json_error,
    render_json_success,
};
use cdenv_devcontainer::{PortPlanningWarning, PortPlanningWarningKind};
use clap::Parser;
use serde::{Serialize, Serializer, ser::Error as _};
use serde_json::Value;

#[derive(Debug, Serialize)]
struct WorkspaceListPayload {
    workspaces: Vec<String>,
}

#[test]
fn json_success_envelope_matches_the_reviewed_snapshot() {
    let envelope = SuccessEnvelope::new(WorkspaceListPayload {
        workspaces: Vec::new(),
    });
    let mut output = Vec::new();

    render_json_success(&mut output, &envelope).expect("the success envelope should render");
    serde_json::from_slice::<Value>(&output).expect("stdout should contain exactly one JSON value");

    assert_eq!(output, include_bytes!("snapshots/json-success.txt"));
}

#[test]
fn json_error_envelope_matches_the_reviewed_snapshot() {
    let error = ApplicationError::CommandUnavailable {
        command: CommandKind::Status,
    };
    let envelope = ErrorEnvelope::from_application_error(&error);
    let mut output = Vec::new();

    render_json_error(&mut output, &envelope).expect("the error envelope should render");
    serde_json::from_slice::<Value>(&output).expect("stdout should contain exactly one JSON value");

    assert_eq!(output, include_bytes!("snapshots/json-error.txt"));
}

#[test]
fn non_loopback_security_warning_is_structured_for_json_and_human_renderers() {
    let warnings = port_output_warnings(&[PortPlanningWarning {
        property_path: "$.appPort".to_owned(),
        kind: PortPlanningWarningKind::NonLoopbackPublication,
    }]);
    let envelope = SuccessEnvelope::with_warnings(
        WorkspaceListPayload {
            workspaces: Vec::new(),
        },
        warnings.clone(),
    );
    let mut json = Vec::new();

    render_json_success(&mut json, &envelope).expect("warning envelope");
    let value: Value = serde_json::from_slice(&json).expect("JSON warning");

    assert_eq!(
        (
            warnings[0].code(),
            warnings[0].message(),
            value["warnings"][0]["code"].as_str()
        ),
        (
            "non_loopback_publication",
            "$.appPort publishes a container port beyond host loopback; the service may be reachable from other hosts",
            Some("non_loopback_publication")
        )
    );
}

#[test]
fn json_application_error_uses_stdout_without_incidental_stderr() {
    let error = ApplicationError::CommandUnavailable {
        command: CommandKind::List,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code =
        render_application_result(OutputFormat::Json, Err(error), &mut stdout, &mut stderr);

    assert_eq!(
        (
            exit_code,
            serde_json::from_slice::<Value>(&stdout).is_ok(),
            stderr
        ),
        (ExitCode::FAILURE, true, Vec::new())
    );
}

#[test]
fn human_application_error_uses_stderr_without_stdout() {
    let error = ApplicationError::CommandUnavailable {
        command: CommandKind::Up,
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();

    let exit_code =
        render_application_result(OutputFormat::Human, Err(error), &mut stdout, &mut stderr);

    assert_eq!(
        (exit_code, stdout, stderr),
        (
            ExitCode::FAILURE,
            Vec::new(),
            b"cdenv: command `up` is not implemented in this build\n".to_vec()
        )
    );
}

#[test]
fn application_error_exposes_stable_message_code_and_exit_status() {
    let error = ApplicationError::CommandUnavailable {
        command: CommandKind::Forward,
    };

    assert_eq!(
        (error.to_string(), error.machine_code(), error.exit_code()),
        (
            "command `forward` is not implemented in this build".to_owned(),
            "command_unavailable",
            ExitCode::FAILURE
        )
    );
}

#[test]
fn invoke_returns_a_typed_error_without_running_a_workflow() {
    let command_line = cdenv_cli::CommandLine::try_parse_from(["cdenv", "down", "project"])
        .expect("the command should parse");

    assert_eq!(
        invoke(&command_line),
        Err(ApplicationError::CommandUnavailable {
            command: CommandKind::Down
        })
    );
}

struct FailingPayload;

impl Serialize for FailingPayload {
    fn serialize<S>(&self, _serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        Err(S::Error::custom("intentional serialization failure"))
    }
}

#[test]
fn serialization_failure_does_not_touch_the_writer() {
    let envelope = SuccessEnvelope::new(FailingPayload);
    let mut output = b"existing bytes".to_vec();

    let error = render_json_success(&mut output, &envelope)
        .expect_err("the intentionally failing payload must not render");

    assert!(
        matches!(error, OutputRenderError::Serialize(_)),
        "unexpected render error: {error}"
    );
    assert_eq!(output, b"existing bytes");
}

#[test]
fn application_and_output_errors_are_send_sync_and_static() {
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}

    assert_send_sync_static::<ApplicationError>();
    assert_send_sync_static::<OutputRenderError>();
}
