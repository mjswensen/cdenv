//! Versioned JSON envelopes and stdout/stderr rendering boundaries.

use std::io::{self, Write};
use std::process::ExitCode;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ApplicationError, OutputFormat};

/// The schema version emitted by V1 machine-readable commands.
pub const JSON_SCHEMA_VERSION: u32 = 1;

/// A structured warning included in a JSON envelope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputWarning {
    code: String,
    message: String,
}

impl OutputWarning {
    /// Creates a warning from a stable machine code and human-readable message.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Returns the stable machine code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable warning message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A versioned successful JSON response.
///
/// The payload is flattened so command-specific fields remain at the top level,
/// as in `{"schemaVersion":1,"workspaces":[],"warnings":[]}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SuccessEnvelope<T> {
    schema_version: u32,
    #[serde(flatten)]
    payload: T,
    warnings: Vec<OutputWarning>,
}

impl<T> SuccessEnvelope<T> {
    /// Creates a V1 success envelope without warnings.
    #[must_use]
    pub const fn new(payload: T) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            payload,
            warnings: Vec::new(),
        }
    }

    /// Creates a V1 success envelope with structured warnings.
    #[must_use]
    pub const fn with_warnings(payload: T, warnings: Vec<OutputWarning>) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            payload,
            warnings,
        }
    }

    /// Returns the machine-output schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the command-specific payload.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }

    /// Returns every warning in deterministic output order.
    #[must_use]
    pub fn warnings(&self) -> &[OutputWarning] {
        &self.warnings
    }

    /// Consumes the envelope and returns its command-specific payload.
    #[must_use]
    pub fn into_payload(self) -> T {
        self.payload
    }
}

/// Structured user-visible details for a machine-readable error.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorDetail {
    code: String,
    message: String,
}

impl ErrorDetail {
    /// Creates error details from a stable machine code and safe message.
    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Creates machine-readable details for an application-boundary error.
    #[must_use]
    pub fn from_application_error(error: &ApplicationError) -> Self {
        Self::new(error.machine_code(), error.to_string())
    }

    /// Returns the stable machine code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable error message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }
}

/// A versioned machine-readable error response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ErrorEnvelope {
    schema_version: u32,
    error: ErrorDetail,
    warnings: Vec<OutputWarning>,
}

impl ErrorEnvelope {
    /// Creates a V1 error envelope without warnings.
    #[must_use]
    pub const fn new(error: ErrorDetail) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            error,
            warnings: Vec::new(),
        }
    }

    /// Creates a V1 error envelope with structured warnings.
    #[must_use]
    pub const fn with_warnings(error: ErrorDetail, warnings: Vec<OutputWarning>) -> Self {
        Self {
            schema_version: JSON_SCHEMA_VERSION,
            error,
            warnings,
        }
    }

    /// Creates a V1 envelope for an application-boundary error.
    #[must_use]
    pub fn from_application_error(error: &ApplicationError) -> Self {
        Self::new(ErrorDetail::from_application_error(error))
    }

    /// Returns the machine-output schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the structured error details.
    #[must_use]
    pub const fn error(&self) -> &ErrorDetail {
        &self.error
    }

    /// Returns every warning in deterministic output order.
    #[must_use]
    pub fn warnings(&self) -> &[OutputWarning] {
        &self.warnings
    }
}

/// An error serializing or writing a machine-readable document.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum OutputRenderError {
    /// The envelope could not be serialized before stdout was touched.
    #[error("failed to serialize JSON output: {0}")]
    Serialize(#[source] serde_json::Error),
    /// The complete serialized document could not be written.
    #[error("failed to write output: {0}")]
    Write(#[source] io::Error),
}

/// Serializes and writes one successful JSON document followed by a newline.
///
/// Serialization completes in memory before the writer is touched. A
/// serialization failure therefore cannot leave a partial JSON document on
/// stdout.
///
/// # Errors
///
/// Returns [`OutputRenderError::Serialize`] when the envelope cannot be
/// serialized and [`OutputRenderError::Write`] when the writer fails.
pub fn render_json_success<T, W>(
    writer: &mut W,
    envelope: &SuccessEnvelope<T>,
) -> Result<(), OutputRenderError>
where
    T: Serialize,
    W: Write + ?Sized,
{
    render_json_document(writer, envelope)
}

/// Serializes and writes one error JSON document followed by a newline.
///
/// # Errors
///
/// Returns [`OutputRenderError::Serialize`] when the envelope cannot be
/// serialized and [`OutputRenderError::Write`] when the writer fails.
pub fn render_json_error<W>(
    writer: &mut W,
    envelope: &ErrorEnvelope,
) -> Result<(), OutputRenderError>
where
    W: Write + ?Sized,
{
    render_json_document(writer, envelope)
}

fn render_json_document<T, W>(writer: &mut W, value: &T) -> Result<(), OutputRenderError>
where
    T: Serialize,
    W: Write + ?Sized,
{
    let mut document = serde_json::to_vec(value).map_err(OutputRenderError::Serialize)?;
    document.push(b'\n');
    writer
        .write_all(&document)
        .map_err(OutputRenderError::Write)
}

#[derive(Serialize)]
struct EmptyPayload {}

/// Renders the current application result to the correct output stream.
///
/// JSON success and error responses go exclusively to stdout. Human errors and
/// renderer diagnostics go exclusively to stderr.
#[must_use]
pub fn render_application_result<Stdout, Stderr>(
    format: OutputFormat,
    result: Result<(), ApplicationError>,
    stdout: &mut Stdout,
    stderr: &mut Stderr,
) -> ExitCode
where
    Stdout: Write + ?Sized,
    Stderr: Write + ?Sized,
{
    match (format, result) {
        (OutputFormat::Human, Ok(())) => ExitCode::SUCCESS,
        (OutputFormat::Json, Ok(())) => {
            let envelope = SuccessEnvelope::new(EmptyPayload {});
            match render_json_success(stdout, &envelope) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => render_failure(stderr, &error),
            }
        }
        (OutputFormat::Human, Err(error)) => match writeln!(stderr, "cdenv: {error}") {
            Ok(()) => error.exit_code(),
            Err(_) => ExitCode::FAILURE,
        },
        (OutputFormat::Json, Err(error)) => {
            let envelope = ErrorEnvelope::from_application_error(&error);
            match render_json_error(stdout, &envelope) {
                Ok(()) => error.exit_code(),
                Err(render_error) => render_failure(stderr, &render_error),
            }
        }
    }
}

fn render_failure(stderr: &mut (impl Write + ?Sized), error: &OutputRenderError) -> ExitCode {
    match writeln!(stderr, "cdenv: {error}") {
        Ok(()) | Err(_) => ExitCode::FAILURE,
    }
}
