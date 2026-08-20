//! Binary-clean Docker Exec creation, inspection, and attached stream routing.

use std::fmt;
use std::pin::Pin;
use std::time::Duration;

use bollard::exec::CreateExecOptions;
use futures_util::{Stream, StreamExt};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::time::sleep;

use cdenv_core::ContainerId;

use super::{
    BollardAdapter, BollardAdapterError, BollardApi, BollardApiError, BollardApiRequest,
    BollardApiResponse,
};
use crate::CancellationToken;

/// Maximum payload accepted in one Docker multiplexed frame.
pub const MAXIMUM_EXEC_FRAME_BYTES: usize = 64 * 1024;
const STREAM_COPY_BYTES: usize = 16 * 1024;
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// A validated full Docker Exec identity.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ExecId(String);

impl ExecId {
    /// Parses a full lowercase hexadecimal Docker Exec ID.
    ///
    /// # Errors
    ///
    /// Rejects values other than exactly 64 lowercase hexadecimal characters.
    pub fn parse(value: &str) -> Result<Self, ExecStreamError> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(ExecStreamError::InvalidExecId);
        }
        Ok(Self(value.to_owned()))
    }

    /// Borrows the full Exec ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ExecId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Borrowed command inputs shared by attached and detached Exec creation.
pub struct ExecCommand<'a> {
    /// Exact verified container ID.
    pub container: &'a ContainerId,
    /// Ordered command argv; argv zero is required.
    pub command: &'a [String],
    /// Optional Docker user or `user:group` value.
    pub user: Option<&'a str>,
    /// Optional absolute container working directory.
    pub working_directory: Option<&'a str>,
    /// Exact `NAME=value` entries supplied to Docker.
    pub environment: &'a [String],
}

impl ExecCommand<'_> {
    fn validate(&self) -> Result<(), ExecStreamError> {
        if self.command.is_empty() {
            return Err(ExecStreamError::EmptyCommand);
        }
        if self
            .command
            .iter()
            .chain(self.environment)
            .any(|value| value.contains('\0'))
            || self.user.is_some_and(|value| value.contains('\0'))
            || self
                .working_directory
                .is_some_and(|value| value.contains('\0'))
        {
            return Err(ExecStreamError::InvalidExecValue);
        }
        if self
            .working_directory
            .is_some_and(|directory| !directory.starts_with('/'))
        {
            return Err(ExecStreamError::RelativeWorkingDirectory);
        }
        Ok(())
    }

    fn configuration(&self, attached: bool, attach_stdin: bool) -> ExecApiConfiguration {
        ExecApiConfiguration {
            attach_stdin: attached && attach_stdin,
            attach_stdout: attached,
            attach_stderr: attached,
            command: self.command.to_vec(),
            user: self.user.map(str::to_owned),
            working_directory: self.working_directory.map(str::to_owned),
            environment: self.environment.to_vec(),
        }
    }
}

/// A created Exec process that must be started with attached stream routing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttachedExec {
    id: ExecId,
    attach_stdin: bool,
}

impl AttachedExec {
    /// Borrows the Docker Exec ID.
    #[must_use]
    pub const fn id(&self) -> &ExecId {
        &self.id
    }
}

/// A created Exec process that must be started detached.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DetachedExec {
    id: ExecId,
}

impl DetachedExec {
    /// Borrows the Docker Exec ID.
    #[must_use]
    pub const fn id(&self) -> &ExecId {
        &self.id
    }
}

/// Authoritative state returned by Docker's Exec inspect endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecInspect {
    /// Exact Exec identity.
    pub id: ExecId,
    /// Container in which the process was created.
    pub container: ContainerId,
    /// Whether Docker reports the process still running.
    pub running: bool,
    /// Final exit code, when Docker has one.
    pub exit_code: Option<i64>,
}

/// Exact attached-stream, framing, cancellation, and exit-status failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ExecStreamError {
    /// Docker returned or the caller supplied a malformed Exec ID.
    #[error("Docker Exec ID must contain exactly 64 lowercase hexadecimal characters")]
    InvalidExecId,
    /// Exec requires a non-empty argv.
    #[error("Docker Exec command argv must not be empty")]
    EmptyCommand,
    /// An Exec string contains a NUL byte.
    #[error("Docker Exec user, directory, argv, and environment values must be NUL-free")]
    InvalidExecValue,
    /// A selected working directory is not absolute inside the container.
    #[error("Docker Exec working directory must be an absolute container path")]
    RelativeWorkingDirectory,
    /// A bounded API operation failed.
    #[error(transparent)]
    Adapter(#[from] BollardAdapterError),
    /// Local generic stream I/O failed.
    #[error("Docker Exec {stream} stream failed: {source}")]
    Io {
        /// Stream being copied.
        stream: &'static str,
        /// Exact I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The attached Docker API stream failed.
    #[error("Docker Exec attached output failed: {source}")]
    ApiOutput {
        /// Lower-level stream failure.
        source: BollardApiError,
    },
    /// Docker emitted an impossible stream kind for a non-TTY Exec.
    #[error("Docker Exec emitted unexpected {stream} bytes on its output stream")]
    UnexpectedOutputStream {
        /// Rejected stream kind.
        stream: &'static str,
    },
    /// Docker framing was truncated, malformed, or exceeded the configured bound.
    #[error("malformed Docker multiplexed stream: {reason}")]
    MalformedFrame {
        /// Static non-sensitive framing reason.
        reason: &'static str,
    },
    /// Cooperative cancellation interrupted attached streaming.
    #[error("Docker Exec streaming was cancelled")]
    Cancelled,
    /// Output ended but Docker still reports the process running.
    #[error("Docker Exec output ended while the process was still running")]
    StillRunning,
    /// Docker omitted a final process exit code.
    #[error("Docker Exec inspection omitted the final exit code")]
    MissingExitCode,
    /// The attached process completed unsuccessfully.
    #[error("Docker Exec process exited with status {code}")]
    NonZeroExit {
        /// Docker process exit status.
        code: i64,
    },
}

#[doc(hidden)]
#[derive(Clone, PartialEq, Eq)]
pub struct ExecApiConfiguration {
    /// Whether Docker attaches stdin.
    pub attach_stdin: bool,
    /// Whether Docker attaches stdout.
    pub attach_stdout: bool,
    /// Whether Docker attaches stderr.
    pub attach_stderr: bool,
    /// Exact command argv.
    pub command: Vec<String>,
    /// Optional Docker user.
    pub user: Option<String>,
    /// Optional container working directory.
    pub working_directory: Option<String>,
    /// Exact environment entries.
    pub environment: Vec<String>,
}

impl fmt::Debug for ExecApiConfiguration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ExecApiConfiguration")
            .field("attach_stdin", &self.attach_stdin)
            .field("attach_stdout", &self.attach_stdout)
            .field("attach_stderr", &self.attach_stderr)
            .field("command_entries", &self.command.len())
            .field("has_user", &self.user.is_some())
            .field("has_working_directory", &self.working_directory.is_some())
            .field("environment_entries", &self.environment.len())
            .finish()
    }
}

impl From<ExecApiConfiguration> for CreateExecOptions<String> {
    fn from(configuration: ExecApiConfiguration) -> Self {
        Self {
            attach_stdin: Some(configuration.attach_stdin),
            attach_stdout: Some(configuration.attach_stdout),
            attach_stderr: Some(configuration.attach_stderr),
            tty: Some(false),
            detach_keys: None,
            env: Some(configuration.environment),
            cmd: Some(configuration.command),
            privileged: Some(false),
            user: configuration.user,
            working_dir: configuration.working_directory,
        }
    }
}

#[doc(hidden)]
pub enum ExecOutput {
    /// Decoded stdout bytes.
    Stdout(Vec<u8>),
    /// Decoded stderr bytes.
    Stderr(Vec<u8>),
    /// Invalid stdin bytes received as output.
    UnexpectedStdin(Vec<u8>),
    /// Invalid unframed console bytes.
    UnexpectedConsole(Vec<u8>),
}

/// Statically injectable attached Docker API streams.
#[doc(hidden)]
pub struct ExecApiIo {
    /// Decoded Docker output frames.
    pub output: Pin<Box<dyn Stream<Item = Result<ExecOutput, BollardApiError>> + Send + 'static>>,
    /// Docker Exec stdin writer.
    pub input: Pin<Box<dyn AsyncWrite + Send + 'static>>,
}

impl<A: BollardApi> BollardAdapter<A> {
    /// Creates a non-TTY Exec configuration for attached binary streaming.
    ///
    /// # Errors
    ///
    /// Returns input validation, timeout, API, response-shape, or ID errors.
    pub async fn create_attached_exec(
        &self,
        command: &ExecCommand<'_>,
        attach_stdin: bool,
    ) -> Result<AttachedExec, ExecStreamError> {
        let id = self
            .create_exec(command, command.configuration(true, attach_stdin))
            .await?;
        Ok(AttachedExec { id, attach_stdin })
    }

    /// Creates a non-TTY Exec configuration intended for detached start.
    ///
    /// # Errors
    ///
    /// Returns input validation, timeout, API, response-shape, or ID errors.
    pub async fn create_detached_exec(
        &self,
        command: &ExecCommand<'_>,
    ) -> Result<DetachedExec, ExecStreamError> {
        let id = self
            .create_exec(command, command.configuration(false, false))
            .await?;
        Ok(DetachedExec { id })
    }

    /// Starts a detached Exec process within the bounded control timeout.
    ///
    /// # Errors
    ///
    /// Returns a timeout, API, or response-shape error.
    pub async fn start_detached_exec(&self, exec: &DetachedExec) -> Result<(), ExecStreamError> {
        self.unit(
            "start detached Exec",
            BollardApiRequest::StartDetachedExec {
                id: exec.id.to_string(),
            },
        )
        .await?;
        Ok(())
    }

    /// Inspects an Exec process through the bounded control API.
    ///
    /// # Errors
    ///
    /// Returns a timeout, API, incomplete response, malformed identity, or identity mismatch.
    pub async fn inspect_exec(&self, id: &ExecId) -> Result<ExecInspect, ExecStreamError> {
        let response = self
            .call(
                "inspect Exec",
                BollardApiRequest::InspectExec { id: id.to_string() },
            )
            .await?;
        let BollardApiResponse::ExecInspect(inspect) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "inspect Exec",
            }
            .into());
        };
        let returned_id = inspect
            .id
            .as_deref()
            .ok_or(BollardAdapterError::MissingField {
                resource: "Exec inspect",
                field: "ID",
            })
            .and_then(|value| {
                ExecId::parse(value).map_err(|_| BollardAdapterError::VerificationMismatch {
                    field: "Exec ID",
                    expected: id.to_string(),
                    actual: Some(value.to_owned()),
                })
            })?;
        if &returned_id != id {
            return Err(BollardAdapterError::VerificationMismatch {
                field: "Exec ID",
                expected: id.to_string(),
                actual: Some(returned_id.to_string()),
            }
            .into());
        }
        let container = inspect
            .container_id
            .ok_or(BollardAdapterError::MissingField {
                resource: "Exec inspect",
                field: "ContainerID",
            })?;
        let container = ContainerId::parse(&container).map_err(|source| {
            BollardAdapterError::InvalidContainerId {
                value: container,
                source,
            }
        })?;
        Ok(ExecInspect {
            id: returned_id,
            container,
            running: inspect.running.unwrap_or(false),
            exit_code: inspect.exit_code,
        })
    }

    /// Starts an attached Exec and copies binary streams with backpressure until final inspection.
    ///
    /// Local EOF half-closes Docker Exec stdin. Remote EOF stops reading local stdin. Docker's
    /// multiplexing bytes and diagnostics are never written to `stdout`.
    ///
    /// # Errors
    ///
    /// Returns start/stream I/O, malformed output, cancellation, inspection, or nonzero-exit errors.
    pub async fn run_attached_exec<R, O, E>(
        &self,
        exec: &AttachedExec,
        stdin: &mut R,
        stdout: &mut O,
        stderr: &mut E,
        cancellation: &CancellationToken,
    ) -> Result<(), ExecStreamError>
    where
        R: AsyncRead + Unpin + Send,
        O: AsyncWrite + Unpin + Send,
        E: AsyncWrite + Unpin + Send,
    {
        if cancellation.is_cancelled() {
            return Err(ExecStreamError::Cancelled);
        }
        let mut io = tokio::time::timeout(
            self.timeout,
            self.api
                .start_attached_exec(exec.id.to_string(), MAXIMUM_EXEC_FRAME_BYTES),
        )
        .await
        .map_err(|_| BollardAdapterError::TimedOut {
            operation: "start attached Exec",
            timeout: self.timeout,
        })?
        .map_err(|source| BollardAdapterError::Api {
            operation: "start attached Exec",
            source,
        })?;

        if exec.attach_stdin {
            let input = copy_stdin(stdin, &mut io.input, cancellation);
            let output = copy_output(&mut io.output, stdout, stderr, cancellation);
            tokio::pin!(input);
            tokio::pin!(output);
            tokio::select! {
                result = &mut output => result?,
                result = &mut input => {
                    result?;
                    output.await?;
                }
            }
        } else {
            copy_output(&mut io.output, stdout, stderr, cancellation).await?;
        }
        stdout.flush().await.map_err(|source| ExecStreamError::Io {
            stream: "stdout",
            source,
        })?;
        stderr.flush().await.map_err(|source| ExecStreamError::Io {
            stream: "stderr",
            source,
        })?;

        let final_state = self.inspect_exec(&exec.id).await?;
        if final_state.running {
            return Err(ExecStreamError::StillRunning);
        }
        let code = final_state
            .exit_code
            .ok_or(ExecStreamError::MissingExitCode)?;
        if code == 0 {
            Ok(())
        } else {
            Err(ExecStreamError::NonZeroExit { code })
        }
    }

    async fn create_exec(
        &self,
        command: &ExecCommand<'_>,
        configuration: ExecApiConfiguration,
    ) -> Result<ExecId, ExecStreamError> {
        command.validate()?;
        let response = self
            .call(
                "create Exec",
                BollardApiRequest::CreateExec {
                    container: command.container.to_string(),
                    configuration,
                },
            )
            .await?;
        let BollardApiResponse::ExecCreated(id) = response else {
            return Err(BollardAdapterError::UnexpectedResponse {
                operation: "create Exec",
            }
            .into());
        };
        ExecId::parse(&id)
    }
}

async fn copy_stdin<R: AsyncRead + Unpin>(
    stdin: &mut R,
    docker_input: &mut Pin<Box<dyn AsyncWrite + Send + 'static>>,
    cancellation: &CancellationToken,
) -> Result<(), ExecStreamError> {
    let copy = async {
        let mut buffer = Box::new([0_u8; STREAM_COPY_BYTES]);
        loop {
            let count =
                stdin
                    .read(buffer.as_mut())
                    .await
                    .map_err(|source| ExecStreamError::Io {
                        stream: "stdin",
                        source,
                    })?;
            if count == 0 {
                docker_input
                    .shutdown()
                    .await
                    .map_err(|source| ExecStreamError::Io {
                        stream: "Docker stdin",
                        source,
                    })?;
                return Ok(());
            }
            docker_input
                .write_all(&buffer[..count])
                .await
                .map_err(|source| ExecStreamError::Io {
                    stream: "Docker stdin",
                    source,
                })?;
        }
    };
    tokio::select! {
        result = copy => result,
        () = cancellation_requested(cancellation) => Err(ExecStreamError::Cancelled),
    }
}

async fn copy_output<O: AsyncWrite + Unpin, E: AsyncWrite + Unpin>(
    output: &mut Pin<Box<dyn Stream<Item = Result<ExecOutput, BollardApiError>> + Send + 'static>>,
    stdout: &mut O,
    stderr: &mut E,
    cancellation: &CancellationToken,
) -> Result<(), ExecStreamError> {
    let copy = async {
        while let Some(frame) = output.next().await {
            match frame.map_err(|source| ExecStreamError::ApiOutput { source })? {
                ExecOutput::Stdout(bytes) => {
                    ensure_output_bound(bytes.len())?;
                    stdout
                        .write_all(&bytes)
                        .await
                        .map_err(|source| ExecStreamError::Io {
                            stream: "stdout",
                            source,
                        })?;
                }
                ExecOutput::Stderr(bytes) => {
                    ensure_output_bound(bytes.len())?;
                    stderr
                        .write_all(&bytes)
                        .await
                        .map_err(|source| ExecStreamError::Io {
                            stream: "stderr",
                            source,
                        })?;
                }
                ExecOutput::UnexpectedStdin(bytes) => {
                    let _ = bytes.len();
                    return Err(ExecStreamError::UnexpectedOutputStream { stream: "stdin" });
                }
                ExecOutput::UnexpectedConsole(bytes) => {
                    let _ = bytes.len();
                    return Err(ExecStreamError::UnexpectedOutputStream { stream: "console" });
                }
            }
        }
        Ok(())
    };
    tokio::select! {
        result = copy => result,
        () = cancellation_requested(cancellation) => Err(ExecStreamError::Cancelled),
    }
}

fn ensure_output_bound(length: usize) -> Result<(), ExecStreamError> {
    if length > MAXIMUM_EXEC_FRAME_BYTES {
        Err(ExecStreamError::MalformedFrame {
            reason: "frame payload exceeds the configured bound",
        })
    } else {
        Ok(())
    }
}

async fn cancellation_requested(cancellation: &CancellationToken) {
    while !cancellation.is_cancelled() {
        sleep(CANCELLATION_POLL_INTERVAL).await;
    }
}

/// Decodes raw Docker multiplexed frames into separate generic async sinks.
///
/// This parser is useful at raw API and in-memory test boundaries. It accepts only non-TTY Docker
/// stdout/stderr frames, waits for complete bounded payloads, and never writes framing bytes.
///
/// # Errors
///
/// Returns malformed/truncated framing or generic reader/writer I/O failures.
pub async fn decode_docker_multiplexed<R, O, E>(
    input: &mut R,
    stdout: &mut O,
    stderr: &mut E,
) -> Result<(), ExecStreamError>
where
    R: AsyncRead + Unpin,
    O: AsyncWrite + Unpin,
    E: AsyncWrite + Unpin,
{
    loop {
        let mut header = [0_u8; 8];
        let header_bytes = read_until_full(input, &mut header, "frame header").await?;
        if header_bytes == 0 {
            return Ok(());
        }
        if header_bytes != header.len() {
            return Err(ExecStreamError::MalformedFrame {
                reason: "truncated frame header",
            });
        }
        if header[1..4] != [0, 0, 0] {
            return Err(ExecStreamError::MalformedFrame {
                reason: "nonzero reserved header bytes",
            });
        }
        if !matches!(header[0], 1 | 2) {
            return Err(ExecStreamError::MalformedFrame {
                reason: "unsupported stream type",
            });
        }
        let length = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length > MAXIMUM_EXEC_FRAME_BYTES {
            return Err(ExecStreamError::MalformedFrame {
                reason: "frame payload exceeds the configured bound",
            });
        }
        let mut payload = vec![0_u8; length];
        if read_until_full(input, &mut payload, "frame payload").await? != length {
            return Err(ExecStreamError::MalformedFrame {
                reason: "truncated frame payload",
            });
        }
        if header[0] == 1 {
            stdout
                .write_all(&payload)
                .await
                .map_err(|source| ExecStreamError::Io {
                    stream: "stdout",
                    source,
                })?;
        } else {
            stderr
                .write_all(&payload)
                .await
                .map_err(|source| ExecStreamError::Io {
                    stream: "stderr",
                    source,
                })?;
        }
    }
}

async fn read_until_full<R: AsyncRead + Unpin>(
    input: &mut R,
    buffer: &mut [u8],
    stream: &'static str,
) -> Result<usize, ExecStreamError> {
    let mut offset = 0;
    while offset < buffer.len() {
        let count = input
            .read(&mut buffer[offset..])
            .await
            .map_err(|source| ExecStreamError::Io { stream, source })?;
        if count == 0 {
            break;
        }
        offset += count;
    }
    Ok(offset)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use std::task::{Context, Poll};

    use bollard::models::ExecInspectResponse;
    use futures_util::stream;
    use tokio::io::{AsyncReadExt, AsyncWrite, AsyncWriteExt};

    use super::*;
    use crate::{DockerEndpoint, DockerEnvironment, DockerSocketProbe};

    const CONTAINER_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const EXEC_ID: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[derive(Clone)]
    struct FakeApi {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        requests: Vec<BollardApiRequest>,
        responses: VecDeque<Result<BollardApiResponse, BollardApiError>>,
        attached: VecDeque<Result<ExecApiIo, BollardApiError>>,
        attached_delay: Duration,
    }

    impl FakeApi {
        fn new(
            responses: impl IntoIterator<Item = BollardApiResponse>,
            attached: impl IntoIterator<Item = ExecApiIo>,
        ) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    requests: Vec::new(),
                    responses: responses.into_iter().map(Ok).collect(),
                    attached: attached.into_iter().map(Ok).collect(),
                    attached_delay: Duration::ZERO,
                })),
            }
        }

        fn requests(&self) -> Vec<BollardApiRequest> {
            self.state.lock().expect("fake lock").requests.clone()
        }

        fn with_attached_delay(self, delay: Duration) -> Self {
            self.state.lock().expect("fake lock").attached_delay = delay;
            self
        }
    }

    impl BollardApi for FakeApi {
        async fn execute(
            &self,
            request: BollardApiRequest,
        ) -> Result<BollardApiResponse, BollardApiError> {
            let mut state = self.state.lock().expect("fake lock");
            state.requests.push(request);
            state.responses.pop_front().expect("fake control response")
        }

        async fn start_attached_exec(
            &self,
            _id: String,
            _output_capacity: usize,
        ) -> Result<ExecApiIo, BollardApiError> {
            let delay = self.state.lock().expect("fake lock").attached_delay;
            if !delay.is_zero() {
                sleep(delay).await;
            }
            self.state
                .lock()
                .expect("fake lock")
                .attached
                .pop_front()
                .expect("fake attached response")
        }
    }

    #[derive(Clone, Default)]
    struct CaptureWriter {
        state: Arc<Mutex<CaptureState>>,
    }

    #[derive(Default)]
    struct CaptureState {
        bytes: Vec<u8>,
        shutdown: bool,
    }

    impl CaptureWriter {
        fn bytes(&self) -> Vec<u8> {
            self.state.lock().expect("capture lock").bytes.clone()
        }

        fn is_shutdown(&self) -> bool {
            self.state.lock().expect("capture lock").shutdown
        }
    }

    impl AsyncWrite for CaptureWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            self.state
                .lock()
                .expect("capture lock")
                .bytes
                .extend_from_slice(buffer);
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            self.state.lock().expect("capture lock").shutdown = true;
            Poll::Ready(Ok(()))
        }
    }

    struct Environment(OsString);

    impl DockerEnvironment for Environment {
        fn docker_host(&self) -> Option<OsString> {
            Some(self.0.clone())
        }
        fn docker_context(&self) -> Option<OsString> {
            None
        }
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
        fn runtime_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    struct Socket(PathBuf);

    impl DockerSocketProbe for Socket {
        fn is_unix_socket(&self, path: &Path) -> bool {
            path == self.0
        }
    }

    fn endpoint() -> DockerEndpoint {
        let path = PathBuf::from("/tmp/cdenv-exec-test.sock");
        DockerEndpoint::resolve_with_probe(
            &Environment(OsString::from("unix:///tmp/cdenv-exec-test.sock")),
            &Socket(path),
        )
        .expect("test endpoint")
    }

    fn inspect(exit_code: Option<i64>, running: bool) -> BollardApiResponse {
        BollardApiResponse::ExecInspect(Box::new(ExecInspectResponse {
            id: Some(EXEC_ID.to_owned()),
            container_id: Some(CONTAINER_ID.to_owned()),
            running: Some(running),
            exit_code,
            ..ExecInspectResponse::default()
        }))
    }

    fn command<'a>(
        container: &'a ContainerId,
        argv: &'a [String],
        environment: &'a [String],
    ) -> ExecCommand<'a> {
        ExecCommand {
            container,
            command: argv,
            user: Some("developer"),
            working_directory: Some("/workspaces/project"),
            environment,
        }
    }

    fn attached_io<I>(frames: I, input: CaptureWriter) -> ExecApiIo
    where
        I: IntoIterator<Item = ExecOutput>,
        I::IntoIter: Send + 'static,
    {
        ExecApiIo {
            output: Box::pin(stream::iter(frames.into_iter().map(Ok))),
            input: Box::pin(input),
        }
    }

    fn frame(stream: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![stream, 0, 0, 0];
        frame.extend_from_slice(
            &u32::try_from(payload.len())
                .expect("frame length")
                .to_be_bytes(),
        );
        frame.extend_from_slice(payload);
        frame
    }

    #[test]
    fn exec_errors_are_send_sync_and_static() {
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}

        assert_error::<ExecStreamError>();
    }

    #[tokio::test]
    async fn detached_create_start_and_inspect_use_exact_non_tty_configuration() {
        let api = FakeApi::new(
            [
                BollardApiResponse::ExecCreated(EXEC_ID.to_owned()),
                BollardApiResponse::Unit,
                inspect(Some(0), false),
            ],
            [],
        );
        let adapter = BollardAdapter::with_api(endpoint(), api.clone(), Duration::from_secs(1));
        let container = ContainerId::parse(CONTAINER_ID).expect("container ID");
        let argv = ["printf".to_owned(), "hello".to_owned()];
        let environment = ["MODE=test".to_owned()];

        let exec = adapter
            .create_detached_exec(&command(&container, &argv, &environment))
            .await
            .expect("create detached");
        adapter
            .start_detached_exec(&exec)
            .await
            .expect("start detached");
        let state = adapter
            .inspect_exec(exec.id())
            .await
            .expect("inspect detached");

        assert_eq!((state.running, state.exit_code), (false, Some(0)));
        assert_eq!(
            api.requests(),
            [
                BollardApiRequest::CreateExec {
                    container: CONTAINER_ID.to_owned(),
                    configuration: ExecApiConfiguration {
                        attach_stdin: false,
                        attach_stdout: false,
                        attach_stderr: false,
                        command: argv.to_vec(),
                        user: Some("developer".to_owned()),
                        working_directory: Some("/workspaces/project".to_owned()),
                        environment: environment.to_vec(),
                    },
                },
                BollardApiRequest::StartDetachedExec {
                    id: EXEC_ID.to_owned()
                },
                BollardApiRequest::InspectExec {
                    id: EXEC_ID.to_owned()
                },
            ]
        );
    }

    #[tokio::test]
    async fn attached_exec_routes_binary_bytes_exactly_and_half_closes_local_eof() {
        let docker_input = CaptureWriter::default();
        let api = FakeApi::new(
            [
                BollardApiResponse::ExecCreated(EXEC_ID.to_owned()),
                inspect(Some(0), false),
            ],
            [ExecApiIo {
                output: Box::pin(
                    stream::once(async {
                        sleep(Duration::from_millis(5)).await;
                        Ok(ExecOutput::Stdout(vec![0, 1, 0, 255]))
                    })
                    .chain(stream::iter([
                        Ok(ExecOutput::Stderr(vec![9, 0, 8])),
                        Ok(ExecOutput::Stdout(b"tail".to_vec())),
                    ])),
                ),
                input: Box::pin(docker_input.clone()),
            }],
        );
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let container = ContainerId::parse(CONTAINER_ID).expect("container ID");
        let argv = ["agent".to_owned()];
        let mut stdin = b"protocol\0input".as_slice();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let exec = adapter
            .create_attached_exec(&command(&container, &argv, &[]), true)
            .await
            .expect("create attached");

        adapter
            .run_attached_exec(
                &exec,
                &mut stdin,
                &mut stdout,
                &mut stderr,
                &CancellationToken::default(),
            )
            .await
            .expect("attached run");

        assert_eq!(stdout, [0, 1, 0, 255, b't', b'a', b'i', b'l']);
        assert_eq!(stderr, [9, 0, 8]);
        assert_eq!(docker_input.bytes(), b"protocol\0input");
        assert!(docker_input.is_shutdown());
    }

    #[tokio::test]
    async fn remote_eof_stops_a_pending_local_input_read() {
        let docker_input = CaptureWriter::default();
        let api = FakeApi::new([inspect(Some(0), false)], [attached_io([], docker_input)]);
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let exec = AttachedExec {
            id: ExecId::parse(EXEC_ID).expect("Exec ID"),
            attach_stdin: true,
        };
        let (mut stdin, _peer) = tokio::io::duplex(8);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        tokio::time::timeout(
            Duration::from_millis(100),
            adapter.run_attached_exec(
                &exec,
                &mut stdin,
                &mut stdout,
                &mut stderr,
                &CancellationToken::default(),
            ),
        )
        .await
        .expect("remote EOF must finish")
        .expect("successful run");
    }

    #[tokio::test]
    async fn attached_stream_cancellation_interrupts_pending_duplex_io() {
        let pending = ExecApiIo {
            output: Box::pin(stream::pending()),
            input: Box::pin(CaptureWriter::default()),
        };
        let api = FakeApi::new([], [pending]);
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let exec = AttachedExec {
            id: ExecId::parse(EXEC_ID).expect("Exec ID"),
            attach_stdin: true,
        };
        let (mut stdin, _peer) = tokio::io::duplex(8);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let cancellation = CancellationToken::default();
        let trigger = cancellation.clone();
        tokio::spawn(async move {
            sleep(Duration::from_millis(5)).await;
            trigger.cancel();
        });

        let error = adapter
            .run_attached_exec(&exec, &mut stdin, &mut stdout, &mut stderr, &cancellation)
            .await
            .expect_err("cancelled run");

        assert!(matches!(error, ExecStreamError::Cancelled));
    }

    #[tokio::test]
    async fn attached_start_obeys_the_bounded_control_timeout() {
        let api = FakeApi::new([], [attached_io([], CaptureWriter::default())])
            .with_attached_delay(Duration::from_millis(50));
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_millis(1));
        let exec = AttachedExec {
            id: ExecId::parse(EXEC_ID).expect("Exec ID"),
            attach_stdin: false,
        };
        let mut stdin = b"".as_slice();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let error = adapter
            .run_attached_exec(
                &exec,
                &mut stdin,
                &mut stdout,
                &mut stderr,
                &CancellationToken::default(),
            )
            .await
            .expect_err("start timeout");

        assert!(matches!(
            error,
            ExecStreamError::Adapter(BollardAdapterError::TimedOut {
                operation: "start attached Exec",
                ..
            })
        ));
    }

    #[tokio::test]
    async fn attached_stream_reports_final_nonzero_exit_status() {
        let api = FakeApi::new(
            [inspect(Some(17), false)],
            [attached_io([], CaptureWriter::default())],
        );
        let adapter = BollardAdapter::with_api(endpoint(), api, Duration::from_secs(1));
        let exec = AttachedExec {
            id: ExecId::parse(EXEC_ID).expect("Exec ID"),
            attach_stdin: false,
        };
        let mut stdin = b"".as_slice();
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        let error = adapter
            .run_attached_exec(
                &exec,
                &mut stdin,
                &mut stdout,
                &mut stderr,
                &CancellationToken::default(),
            )
            .await
            .expect_err("nonzero exit");

        assert!(matches!(error, ExecStreamError::NonZeroExit { code: 17 }));
    }

    #[tokio::test]
    async fn frame_decoder_handles_fragmented_interleaved_binary_frames_without_leaking_headers() {
        let mut bytes = frame(1, &[0, 255, 1]);
        bytes.extend(frame(2, b"error\0bytes"));
        bytes.extend(frame(1, b"done"));
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let producer = tokio::spawn(async move {
            for byte in bytes {
                writer.write_all(&[byte]).await.expect("fragment write");
            }
        });
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();

        decode_docker_multiplexed(&mut reader, &mut stdout, &mut stderr)
            .await
            .expect("decode");
        producer.await.expect("producer");

        assert_eq!(stdout, [0, 255, 1, b'd', b'o', b'n', b'e']);
        assert_eq!(stderr, b"error\0bytes");
    }

    #[tokio::test]
    async fn frame_decoder_applies_backpressure_across_large_streams() {
        let payload = vec![0x5a; MAXIMUM_EXEC_FRAME_BYTES];
        let mut bytes = Vec::new();
        for _ in 0..16 {
            bytes.extend(frame(1, &payload));
        }
        let expected = payload.len() * 16;
        let (mut source_writer, mut source_reader) = tokio::io::duplex(127);
        let source = tokio::spawn(async move {
            source_writer.write_all(&bytes).await.expect("source write");
        });
        let (mut sink_writer, mut sink_reader) = tokio::io::duplex(31);
        let sink = tokio::spawn(async move {
            let mut received = Vec::new();
            sink_reader
                .read_to_end(&mut received)
                .await
                .expect("sink read");
            received
        });
        let mut stderr = Vec::new();

        decode_docker_multiplexed(&mut source_reader, &mut sink_writer, &mut stderr)
            .await
            .expect("large decode");
        sink_writer.shutdown().await.expect("sink EOF");
        source.await.expect("source");
        let received = sink.await.expect("sink");

        assert_eq!(received.len(), expected);
        assert!(received.iter().all(|byte| *byte == 0x5a));
    }

    #[tokio::test]
    async fn frame_decoder_rejects_malformed_and_truncated_frames_without_partial_payload() {
        let malformed = [
            vec![1, 0, 0],
            vec![3, 0, 0, 0, 0, 0, 0, 0],
            vec![1, 1, 0, 0, 0, 0, 0, 0],
            frame(1, &vec![0; MAXIMUM_EXEC_FRAME_BYTES + 1])[..8].to_vec(),
            vec![1, 0, 0, 0, 0, 0, 0, 4, 1, 2],
        ];

        for bytes in malformed {
            let mut input = bytes.as_slice();
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let error = decode_docker_multiplexed(&mut input, &mut stdout, &mut stderr)
                .await
                .expect_err("malformed frame");
            assert!(matches!(error, ExecStreamError::MalformedFrame { .. }));
            assert!(stdout.is_empty(), "partial frame bytes must not escape");
            assert!(stderr.is_empty(), "partial frame bytes must not escape");
        }
    }
}
