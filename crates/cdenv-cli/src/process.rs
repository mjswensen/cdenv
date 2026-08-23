//! Bounded, redacting subprocess execution for owned host commands.

use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use getrandom::fill;
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::sleep;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
const DEFAULT_CAPTURE_BYTES: usize = 1024 * 1024;
const DEFAULT_LOG_BYTES: usize = 1024 * 1024;

/// Cooperative cancellation shared by owned subprocess adapters.
#[derive(Clone, Debug, Default)]
pub struct CancellationToken(Arc<AtomicBool>);

impl CancellationToken {
    /// Requests cancellation of the current command and its process group.
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Reports whether cancellation has been requested.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A unique, non-secret identifier for one subprocess operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OperationId(String);

impl OperationId {
    fn generate() -> Result<Self, ProcessError> {
        let mut bytes = [0_u8; 16];
        fill(&mut bytes).map_err(ProcessError::OperationId)?;
        Ok(Self(hex::encode(bytes)))
    }

    /// Borrows the lowercase hexadecimal identifier.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One explicitly supplied subprocess environment variable.
#[derive(Clone, Copy, Debug)]
pub struct ProcessEnvironmentVariable<'a> {
    /// Variable name.
    pub name: &'a OsStr,
    /// Variable value.
    pub value: &'a OsStr,
    /// Whether occurrences of the value must be redacted from output and logs.
    pub sensitive: bool,
}

/// Whether a command has an intentionally bounded duration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessDeadline {
    /// Builds, lifecycle commands, sessions, and background work have no guessed deadline.
    Unbounded,
    /// A control or capability probe must complete within this duration.
    Control(Duration),
}

/// Borrowed direct-exec command input.
#[derive(Debug)]
pub struct ProcessRequest<'a> {
    /// Safe, non-user-controlled operation label written to the log.
    pub operation: &'static str,
    /// Executable invoked directly, without a shell.
    pub executable: &'a Path,
    /// Exact argument vector excluding argv zero.
    pub arguments: &'a [&'a OsStr],
    /// Explicit working directory.
    pub cwd: &'a Path,
    /// Complete explicit environment. The inherited environment is cleared.
    pub environment: &'a [ProcessEnvironmentVariable<'a>],
    /// Additional recognized byte values, such as authorization header values, to redact.
    pub redactions: &'a [&'a [u8]],
    /// Optional bounded control/probe deadline.
    pub deadline: ProcessDeadline,
}

/// Bounded bytes captured from one process stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl CapturedOutput {
    /// Borrows captured, already-redacted bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Reports whether bytes beyond the configured bound were discarded.
    #[must_use]
    pub const fn is_truncated(&self) -> bool {
        self.truncated
    }
}

/// Typed raw result consumed only by an adapter parser.
#[derive(Clone, Debug)]
pub struct ProcessResult {
    /// Unique operation identifier.
    pub operation_id: OperationId,
    /// Portable process status.
    pub status: ExitStatus,
    /// Bounded, redacted standard output.
    pub stdout: CapturedOutput,
    /// Bounded, redacted standard error.
    pub stderr: CapturedOutput,
    /// Private restricted operation-log path.
    pub log_path: PathBuf,
}

/// Reusable direct subprocess runner.
#[derive(Clone, Debug)]
pub struct ProcessRunner {
    log_directory: PathBuf,
    maximum_capture_bytes: usize,
    maximum_log_bytes: usize,
}

impl ProcessRunner {
    /// Creates a runner using one private log directory and default one-MiB bounds.
    #[must_use]
    pub fn new(log_directory: PathBuf) -> Self {
        Self {
            log_directory,
            maximum_capture_bytes: DEFAULT_CAPTURE_BYTES,
            maximum_log_bytes: DEFAULT_LOG_BYTES,
        }
    }

    /// Overrides output and operation-log bounds, primarily for constrained operations and tests.
    #[must_use]
    pub const fn with_bounds(mut self, capture_bytes: usize, log_bytes: usize) -> Self {
        self.maximum_capture_bytes = capture_bytes;
        self.maximum_log_bytes = log_bytes;
        self
    }

    /// Runs a directly invoked process with bounded capture, redaction, cancellation, and logging.
    ///
    /// # Errors
    ///
    /// Returns a typed setup, spawn, capture, cancellation, timeout, or wait failure. A non-zero
    /// status is returned in [`ProcessResult`] for the adapter to classify.
    pub async fn run(
        &self,
        request: &ProcessRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessResult, ProcessError> {
        self.run_with_stdout_logging(request, cancellation, true)
            .await
    }

    /// Runs a process while keeping stdout only in bounded memory and out of the operation log.
    ///
    /// This is intended for typed parsing of interpolated configuration that may contain values
    /// unknown to cdenv's redactor. Stderr and operation metadata remain logged.
    ///
    /// # Errors
    ///
    /// Returns the same failures as [`Self::run`].
    pub async fn run_secret_stdout(
        &self,
        request: &ProcessRequest<'_>,
        cancellation: &CancellationToken,
    ) -> Result<ProcessResult, ProcessError> {
        self.run_with_stdout_logging(request, cancellation, false)
            .await
    }

    async fn run_with_stdout_logging(
        &self,
        request: &ProcessRequest<'_>,
        cancellation: &CancellationToken,
        log_stdout: bool,
    ) -> Result<ProcessResult, ProcessError> {
        if request.operation.is_empty()
            || !request
                .operation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        {
            return Err(ProcessError::InvalidOperationLabel {
                operation: request.operation,
            });
        }
        if cancellation.is_cancelled() {
            return Err(ProcessError::Cancelled {
                operation: request.operation,
            });
        }
        fs::create_dir_all(&self.log_directory).map_err(|source| ProcessError::LogDirectory {
            path: self.log_directory.clone(),
            source,
        })?;
        let operation_id = OperationId::generate()?;
        let log_path = self.log_directory.join(format!(
            "{}-{}.log",
            request.operation,
            operation_id.as_str()
        ));
        let log = RestrictedLog::create(&log_path, self.maximum_log_bytes)?;
        log.write_line(format_args!(
            "operation {} ({}) started",
            request.operation,
            operation_id.as_str()
        ))?;

        let mut command = Command::new(request.executable);
        command
            .args(request.arguments)
            .current_dir(request.cwd)
            .env_clear()
            .envs(
                request
                    .environment
                    .iter()
                    .map(|variable| (variable.name, variable.value)),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        configure_process_group(&mut command);
        let mut child = command.spawn().map_err(|source| ProcessError::Spawn {
            executable: request.executable.to_path_buf(),
            operation: request.operation,
            source,
        })?;
        let stdout = child.stdout.take().ok_or(ProcessError::MissingPipe {
            operation: request.operation,
            stream: "stdout",
        })?;
        let stderr = child.stderr.take().ok_or(ProcessError::MissingPipe {
            operation: request.operation,
            stream: "stderr",
        })?;
        let redactions = collect_redactions(request);
        let stdout_reader = tokio::spawn(capture_stream(
            stdout,
            log_stdout.then(|| log.clone()),
            redactions.clone(),
            self.maximum_capture_bytes,
        ));
        let stderr_reader = tokio::spawn(capture_stream(
            stderr,
            Some(log.clone()),
            redactions,
            self.maximum_capture_bytes,
        ));
        let wait_result = wait_for_child(&mut child, request, cancellation).await;
        let stdout = join_reader(stdout_reader, request.operation, "stdout").await?;
        let stderr = join_reader(stderr_reader, request.operation, "stderr").await?;
        let status = wait_result?;
        log.write_line(format_args!(
            "operation {} ({}) completed with {:?}",
            request.operation,
            operation_id.as_str(),
            status.code()
        ))?;
        Ok(ProcessResult {
            operation_id,
            status,
            stdout,
            stderr,
            log_path,
        })
    }
}

/// A subprocess runner failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ProcessError {
    /// An operation label was unsafe for use in a private log filename.
    #[error(
        "invalid subprocess operation label {operation:?}; use ASCII letters, digits, '-' or '_'"
    )]
    InvalidOperationLabel {
        /// Rejected label.
        operation: &'static str,
    },
    /// Secure random operation-ID generation failed.
    #[error("cannot generate a subprocess operation ID: {0}")]
    OperationId(getrandom::Error),
    /// The private log directory could not be created.
    #[error("cannot create subprocess log directory {path:?}: {source}")]
    LogDirectory {
        /// Log directory path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The executable could not be started.
    #[error("cannot start {operation} with executable {executable:?}: {source}")]
    Spawn {
        /// Configured executable.
        executable: PathBuf,
        /// Safe operation label.
        operation: &'static str,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A configured output pipe was unexpectedly unavailable.
    #[error("{operation} did not provide its configured {stream} pipe")]
    MissingPipe {
        /// Safe operation label.
        operation: &'static str,
        /// Pipe identity.
        stream: &'static str,
    },
    /// Waiting for or terminating a process failed.
    #[error("cannot wait for {operation}: {source}")]
    Wait {
        /// Safe operation label.
        operation: &'static str,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// Capturing a stream failed.
    #[error("cannot capture {operation} {stream}: {source}")]
    Capture {
        /// Safe operation label.
        operation: &'static str,
        /// Stream identity.
        stream: &'static str,
        /// I/O or task failure.
        #[source]
        source: io::Error,
    },
    /// Cancellation terminated the owned process group.
    #[error("{operation} was cancelled")]
    Cancelled {
        /// Safe operation label.
        operation: &'static str,
    },
    /// A bounded control or capability probe exceeded its explicit deadline.
    #[error("{operation} timed out after {timeout:?}")]
    TimedOut {
        /// Safe operation label.
        operation: &'static str,
        /// Configured probe timeout.
        timeout: Duration,
    },
    /// Restricted operation-log I/O failed.
    #[error("restricted subprocess logging failed: {source}")]
    Log {
        /// Private log failure.
        #[source]
        source: io::Error,
    },
}

impl From<RestrictedLogError> for ProcessError {
    fn from(source: RestrictedLogError) -> Self {
        Self::Log {
            source: io::Error::other(source),
        }
    }
}

#[derive(Clone, Debug)]
struct RestrictedLog(Arc<Mutex<BoundedLog>>);

impl RestrictedLog {
    fn create(path: &Path, maximum_bytes: usize) -> Result<Self, RestrictedLogError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        configure_private_file(&mut options);
        let file = options
            .open(path)
            .map_err(|source| RestrictedLogError::Create {
                path: path.to_path_buf(),
                source,
            })?;
        enforce_private_file_mode(&file).map_err(|source| RestrictedLogError::Create {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self(Arc::new(Mutex::new(BoundedLog {
            file,
            path: path.to_path_buf(),
            maximum_bytes,
            written: 0,
        }))))
    }

    fn write_line(&self, arguments: std::fmt::Arguments<'_>) -> Result<(), RestrictedLogError> {
        let mut log = self.0.lock().map_err(|_| RestrictedLogError::Poisoned)?;
        log.write_fmt(arguments)?;
        log.write_all(b"\n")
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<(), RestrictedLogError> {
        self.0
            .lock()
            .map_err(|_| RestrictedLogError::Poisoned)?
            .write_all(bytes)
    }
}

#[derive(Debug, Error)]
enum RestrictedLogError {
    #[error("cannot create private subprocess log {path:?}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("cannot write private subprocess log {path:?}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("subprocess log lock was poisoned")]
    Poisoned,
}

#[derive(Debug)]
struct BoundedLog {
    file: File,
    path: PathBuf,
    maximum_bytes: usize,
    written: usize,
}

impl BoundedLog {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), RestrictedLogError> {
        let remaining = self.maximum_bytes.saturating_sub(self.written);
        let accepted = remaining.min(bytes.len());
        if accepted > 0 {
            self.file.write_all(&bytes[..accepted]).map_err(|source| {
                RestrictedLogError::Write {
                    path: self.path.clone(),
                    source,
                }
            })?;
            self.written += accepted;
        }
        Ok(())
    }

    fn write_fmt(&mut self, arguments: std::fmt::Arguments<'_>) -> Result<(), RestrictedLogError> {
        let mut bytes = Vec::new();
        bytes
            .write_fmt(arguments)
            .map_err(|source| RestrictedLogError::Write {
                path: self.path.clone(),
                source,
            })?;
        self.write_all(&bytes)
    }
}

fn collect_redactions(request: &ProcessRequest<'_>) -> Vec<Vec<u8>> {
    let mut values = request
        .redactions
        .iter()
        .filter(|value| !value.is_empty())
        .map(|value| value.to_vec())
        .collect::<Vec<_>>();
    values.extend(
        request
            .environment
            .iter()
            .filter(|variable| variable.sensitive)
            .map(|variable| variable.value.to_string_lossy().into_owned().into_bytes())
            .filter(|value| !value.is_empty()),
    );
    values.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    values.dedup();
    values
}

async fn capture_stream(
    mut reader: impl AsyncRead + Unpin,
    log: Option<RestrictedLog>,
    redactions: Vec<Vec<u8>>,
    maximum_bytes: usize,
) -> Result<CapturedOutput, RestrictedLogError> {
    let mut captured = Vec::new();
    let mut pending = Vec::new();
    let mut truncated = false;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .await
            .map_err(|source| RestrictedLogError::Write {
                path: PathBuf::from("<process-pipe>"),
                source,
            })?;
        if count == 0 {
            break;
        }
        let ready = redact_chunk(&chunk[..count], &mut pending, &redactions, false);
        append_output(
            &ready,
            log.as_ref(),
            &mut captured,
            maximum_bytes,
            &mut truncated,
        )?;
    }
    let ready = redact_chunk(&[], &mut pending, &redactions, true);
    append_output(
        &ready,
        log.as_ref(),
        &mut captured,
        maximum_bytes,
        &mut truncated,
    )?;
    Ok(CapturedOutput {
        bytes: captured,
        truncated,
    })
}

fn redact_chunk(bytes: &[u8], pending: &mut Vec<u8>, secrets: &[Vec<u8>], eof: bool) -> Vec<u8> {
    pending.extend_from_slice(bytes);
    if secrets.is_empty() {
        return std::mem::take(pending);
    }
    let mut output = Vec::new();
    loop {
        if let Some(secret) = secrets.iter().find(|secret| pending.starts_with(secret)) {
            output.extend_from_slice(b"[REDACTED]");
            pending.drain(..secret.len());
            continue;
        }
        if pending.is_empty() || (!eof && secrets.iter().any(|secret| secret.starts_with(pending)))
        {
            break;
        }
        output.push(pending.remove(0));
    }
    output
}

fn append_output(
    bytes: &[u8],
    log: Option<&RestrictedLog>,
    captured: &mut Vec<u8>,
    maximum_bytes: usize,
    truncated: &mut bool,
) -> Result<(), RestrictedLogError> {
    if let Some(log) = log {
        log.write_bytes(bytes)?;
    }
    let remaining = maximum_bytes.saturating_sub(captured.len());
    let accepted = remaining.min(bytes.len());
    captured.extend_from_slice(&bytes[..accepted]);
    *truncated |= accepted < bytes.len();
    Ok(())
}

async fn join_reader(
    handle: JoinHandle<Result<CapturedOutput, RestrictedLogError>>,
    operation: &'static str,
    stream: &'static str,
) -> Result<CapturedOutput, ProcessError> {
    handle
        .await
        .map_err(|source| ProcessError::Capture {
            operation,
            stream,
            source: io::Error::other(source),
        })?
        .map_err(|source| ProcessError::Capture {
            operation,
            stream,
            source: io::Error::other(source),
        })
}

async fn wait_for_child(
    child: &mut Child,
    request: &ProcessRequest<'_>,
    cancellation: &CancellationToken,
) -> Result<ExitStatus, ProcessError> {
    let started = Instant::now();
    let mut termination_started = None;
    let mut reason = None;
    loop {
        if let Some(status) = child.try_wait().map_err(|source| ProcessError::Wait {
            operation: request.operation,
            source,
        })? {
            return match reason {
                Some(TerminationReason::Cancelled) => Err(ProcessError::Cancelled {
                    operation: request.operation,
                }),
                Some(TerminationReason::TimedOut(timeout)) => Err(ProcessError::TimedOut {
                    operation: request.operation,
                    timeout,
                }),
                None => Ok(status),
            };
        }
        let timed_out = match request.deadline {
            ProcessDeadline::Control(timeout) if started.elapsed() >= timeout => Some(timeout),
            ProcessDeadline::Unbounded | ProcessDeadline::Control(_) => None,
        };
        if termination_started.is_none() && (cancellation.is_cancelled() || timed_out.is_some()) {
            terminate_process_group(child, false).map_err(|source| ProcessError::Wait {
                operation: request.operation,
                source,
            })?;
            termination_started = Some(Instant::now());
            reason =
                Some(timed_out.map_or(TerminationReason::Cancelled, TerminationReason::TimedOut));
        } else if termination_started.is_some_and(|instant| instant.elapsed() >= TERMINATION_GRACE)
        {
            terminate_process_group(child, true).map_err(|source| ProcessError::Wait {
                operation: request.operation,
                source,
            })?;
        }
        sleep(POLL_INTERVAL).await;
    }
}

#[derive(Clone, Copy, Debug)]
enum TerminationReason {
    Cancelled,
    TimedOut(Duration),
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
const fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_group(child: &mut Child, force: bool) -> io::Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    let process_group = i32::try_from(
        child
            .id()
            .ok_or_else(|| io::Error::other("missing process ID"))?,
    )
    .map(Pid::from_raw)
    .map_err(|_| io::Error::other("process identifier exceeds platform range"))?;
    let signal = if force {
        Signal::SIGKILL
    } else {
        Signal::SIGTERM
    };
    match killpg(process_group, signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child, _force: bool) -> io::Result<()> {
    child.start_kill()
}

#[cfg(unix)]
fn configure_private_file(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
}

#[cfg(not(unix))]
const fn configure_private_file(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn enforce_private_file_mode(file: &File) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
const fn enforce_private_file_mode(_file: &File) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_handles_values_split_between_chunks() {
        let mut pending = Vec::new();
        let secrets = vec![b"secret-marker".to_vec()];
        let first = redact_chunk(b"before secret", &mut pending, &secrets, false);
        let second = redact_chunk(b"-marker after", &mut pending, &secrets, true);

        assert_eq!([first, second].concat(), b"before [REDACTED] after");
    }
}
