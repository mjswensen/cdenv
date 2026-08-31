//! Safe system-Git subprocess execution with cancellation and restricted logs.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;
use url::Url;

use crate::process::CancellationToken;

const POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);
const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_CAPTURE_BYTES: usize = 1024 * 1024;

/// A detected system-Git semantic version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitVersion {
    major: u32,
    minor: u32,
    patch: Option<u32>,
}

impl GitVersion {
    /// Returns the major component.
    #[must_use]
    pub const fn major(&self) -> u32 {
        self.major
    }

    /// Returns the minor component.
    #[must_use]
    pub const fn minor(&self) -> u32 {
        self.minor
    }

    /// Returns the optional patch component.
    #[must_use]
    pub const fn patch(&self) -> Option<u32> {
        self.patch
    }
}

/// System-Git adapter configured with one executable path.
#[derive(Clone, Debug)]
pub struct GitAdapter {
    executable: PathBuf,
}

impl GitAdapter {
    /// Uses `git` from the process search path.
    #[must_use]
    pub fn system() -> Self {
        Self::new(PathBuf::from("git"))
    }

    /// Uses an explicit executable, primarily for deterministic adapters and tests.
    #[must_use]
    pub const fn new(executable: PathBuf) -> Self {
        Self { executable }
    }

    /// Returns the configured executable path without resolving it.
    #[must_use]
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    pub(crate) fn detect(
        &self,
        cancellation: &CancellationToken,
        log: &OperationLog,
    ) -> Result<GitVersion, GitError> {
        let output = self.run(
            &[OsString::from("--version")],
            None,
            "version detection",
            cancellation,
            log,
        )?;
        parse_version(&output.stdout)
    }

    /// Runs `git status --porcelain` and returns only whether checkout changes exist.
    ///
    /// Porcelain bytes are deliberately discarded and never included in logs or errors, so dirty
    /// tracked and untracked filenames cannot escape this boundary.
    ///
    /// # Errors
    ///
    /// Returns a spawn or non-zero-exit failure without command output.
    pub fn checkout_has_changes(&self, checkout: &Path) -> Result<bool, GitError> {
        let output = Command::new(&self.executable)
            .args(["status", "--porcelain"])
            .current_dir(checkout)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .map_err(|source| GitError::Spawn {
                executable: self.executable.clone(),
                operation: "checkout status",
                source,
            })?;
        if !output.status.success() {
            return Err(GitError::StatusExited {
                code: output.status.code(),
            });
        }
        Ok(!output.stdout.is_empty())
    }

    pub(crate) fn clone_repository(
        &self,
        source: &str,
        destination: &Path,
        cancellation: &CancellationToken,
        log: &OperationLog,
    ) -> Result<(), GitError> {
        if source.len() > MAX_SOURCE_BYTES {
            return Err(GitError::SourceTooLong {
                maximum: MAX_SOURCE_BYTES,
            });
        }
        let arguments = [
            OsString::from("clone"),
            OsString::from("--"),
            OsString::from(source),
            destination.as_os_str().to_owned(),
        ];
        let redactions = clone_redactions(source);
        self.run(&arguments, Some(&redactions), "clone", cancellation, log)?;
        Ok(())
    }

    fn run(
        &self,
        arguments: &[OsString],
        secrets: Option<&[Vec<u8>]>,
        operation: &'static str,
        cancellation: &CancellationToken,
        log: &OperationLog,
    ) -> Result<CommandOutput, GitError> {
        if cancellation.is_cancelled() {
            return Err(GitError::Cancelled { operation });
        }
        log.write_line(format_args!("git {operation} started"))?;
        let mut command = Command::new(&self.executable);
        command
            .args(arguments)
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let owns_process_group = configure_process_group(&mut command);
        let mut child = command.spawn().map_err(|source| GitError::Spawn {
            executable: self.executable.clone(),
            operation,
            source,
        })?;
        let stdout = child.stdout.take().ok_or(GitError::MissingPipe {
            operation,
            stream: "stdout",
        })?;
        let stderr = child.stderr.take().ok_or(GitError::MissingPipe {
            operation,
            stream: "stderr",
        })?;
        let stdout_reader =
            spawn_reader(stdout, log.clone(), secrets.map(<[Vec<u8>]>::to_vec), false);
        let stderr_reader =
            spawn_reader(stderr, log.clone(), secrets.map(<[Vec<u8>]>::to_vec), true);
        let (status, cancelled) =
            wait_for_child(&mut child, cancellation, operation, owns_process_group)?;
        let stdout = join_reader(stdout_reader, operation, "stdout")?;
        let stderr = join_reader(stderr_reader, operation, "stderr")?;
        if cancelled {
            log.write_line(format_args!("git {operation} cancelled"))?;
            return Err(GitError::Cancelled { operation });
        }
        if !status.success() {
            log.write_line(format_args!("git {operation} exited unsuccessfully"))?;
            return Err(GitError::Exited {
                operation,
                code: status.code(),
                stderr: bounded_summary(&stderr),
            });
        }
        log.write_line(format_args!("git {operation} completed"))?;
        Ok(CommandOutput { stdout })
    }
}

struct CommandOutput {
    stdout: Vec<u8>,
}

/// A system-Git dependency or subprocess failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GitError {
    /// The source is too large to redact safely from bounded streams.
    #[error("Git source exceeds the supported {maximum}-byte argument limit")]
    SourceTooLong {
        /// Maximum accepted source length.
        maximum: usize,
    },
    /// The executable could not be started.
    #[error("cannot start Git {operation} with executable {executable:?}: {source}")]
    Spawn {
        /// Configured executable.
        executable: PathBuf,
        /// Safe operation label.
        operation: &'static str,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A configured capture pipe was unexpectedly unavailable.
    #[error("Git {operation} did not provide its configured {stream} pipe")]
    MissingPipe {
        /// Safe operation label.
        operation: &'static str,
        /// Pipe identity.
        stream: &'static str,
    },
    /// Waiting for or terminating Git failed.
    #[error("cannot wait for Git {operation}: {source}")]
    Wait {
        /// Safe operation label.
        operation: &'static str,
        /// Operating-system failure.
        #[source]
        source: io::Error,
    },
    /// A reader thread failed or could not be joined.
    #[error("cannot capture Git {operation} {stream}: {source}")]
    Capture {
        /// Safe operation label.
        operation: &'static str,
        /// Stream identity.
        stream: &'static str,
        /// Capture failure.
        #[source]
        source: io::Error,
    },
    /// The command returned a failing status.
    #[error("Git {operation} failed with exit code {code:?}: {stderr}")]
    Exited {
        /// Safe operation label.
        operation: &'static str,
        /// Portable exit code, absent when signaled.
        code: Option<i32>,
        /// Bounded output with the clone source redacted.
        stderr: String,
    },
    /// Checkout status failed without exposing porcelain or stderr bytes.
    #[error("Git checkout status failed with exit code {code:?}")]
    StatusExited {
        /// Portable exit code, absent when signaled.
        code: Option<i32>,
    },
    /// Cancellation terminated the Git process group.
    #[error("Git {operation} was cancelled")]
    Cancelled {
        /// Safe operation label.
        operation: &'static str,
    },
    /// `git --version` returned an unsupported shape.
    #[error("Git version output was not recognized")]
    InvalidVersion,
    /// Restricted operation-log I/O failed.
    #[error(transparent)]
    Log(#[from] OperationLogError),
}

#[derive(Clone, Debug)]
pub(crate) struct OperationLog {
    inner: Arc<Mutex<BoundedLog>>,
}

impl OperationLog {
    pub(crate) fn create(path: &Path, maximum_bytes: usize) -> Result<Self, OperationLogError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        configure_private_file(&mut options);
        let file = options
            .open(path)
            .map_err(|source| OperationLogError::Create {
                path: path.to_path_buf(),
                source,
            })?;
        enforce_private_file_mode(&file).map_err(|source| OperationLogError::Create {
            path: path.to_path_buf(),
            source,
        })?;
        Ok(Self {
            inner: Arc::new(Mutex::new(BoundedLog {
                file,
                path: path.to_path_buf(),
                maximum_bytes,
                written: 0,
            })),
        })
    }

    pub(crate) fn write_line(
        &self,
        arguments: std::fmt::Arguments<'_>,
    ) -> Result<(), OperationLogError> {
        let mut guard = self.inner.lock().map_err(|_| OperationLogError::Poisoned)?;
        guard.write_fmt(arguments)?;
        guard.write_all(b"\n")
    }

    fn write_bytes(&self, bytes: &[u8]) -> Result<(), OperationLogError> {
        self.inner
            .lock()
            .map_err(|_| OperationLogError::Poisoned)?
            .write_all(bytes)
    }
}

#[derive(Debug, Error)]
/// A private bounded operation-log failure.
pub enum OperationLogError {
    /// The unique private log file could not be created.
    #[error("cannot create private operation log {path:?}: {source}")]
    Create {
        /// Intended log path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// A bounded log write failed.
    #[error("cannot write private operation log {path:?}: {source}")]
    Write {
        /// Managed log path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// An earlier panic poisoned synchronized log access.
    #[error("operation log lock was poisoned")]
    Poisoned,
}

#[derive(Debug)]
struct BoundedLog {
    file: File,
    path: PathBuf,
    maximum_bytes: usize,
    written: usize,
}

impl Write for BoundedLog {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.maximum_bytes.saturating_sub(self.written);
        let accepted = remaining.min(buffer.len());
        if accepted > 0 {
            self.file.write_all(&buffer[..accepted])?;
            self.written += accepted;
        }
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl BoundedLog {
    fn write_all(&mut self, bytes: &[u8]) -> Result<(), OperationLogError> {
        Write::write_all(self, bytes).map_err(|source| OperationLogError::Write {
            path: self.path.clone(),
            source,
        })
    }

    fn write_fmt(&mut self, arguments: std::fmt::Arguments<'_>) -> Result<(), OperationLogError> {
        Write::write_fmt(self, arguments).map_err(|source| OperationLogError::Write {
            path: self.path.clone(),
            source,
        })
    }
}

fn spawn_reader(
    reader: impl Read + Send + 'static,
    log: OperationLog,
    secrets: Option<Vec<Vec<u8>>>,
    mirror_stderr: bool,
) -> thread::JoinHandle<Result<Vec<u8>, OperationLogError>> {
    thread::spawn(move || capture_stream(reader, &log, secrets.as_deref(), mirror_stderr))
}

fn capture_stream(
    mut reader: impl Read,
    log: &OperationLog,
    secrets: Option<&[Vec<u8>]>,
    mirror_stderr: bool,
) -> Result<Vec<u8>, OperationLogError> {
    let mut captured = Vec::new();
    let mut pending = Vec::new();
    let mut chunk = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut chunk)
            .map_err(|source| OperationLogError::Write {
                path: PathBuf::from("<git-pipe>"),
                source,
            })?;
        if count == 0 {
            break;
        }
        if let Some(secrets) = secrets.filter(|values| !values.is_empty()) {
            let mut ready = Vec::with_capacity(count);
            for byte in &chunk[..count] {
                pending.push(*byte);
                while !secrets.iter().any(|secret| secret.starts_with(&pending)) {
                    ready.push(pending.remove(0));
                }
                if secrets.contains(&pending) {
                    ready.extend_from_slice(b"[REDACTED]");
                    pending.clear();
                }
            }
            emit_redacted(&ready, None, log, mirror_stderr, &mut captured)?;
        } else {
            emit_redacted(&chunk[..count], None, log, mirror_stderr, &mut captured)?;
        }
    }
    emit_redacted(&pending, None, log, mirror_stderr, &mut captured)?;
    Ok(captured)
}

fn emit_redacted(
    bytes: &[u8],
    secret: Option<&[u8]>,
    log: &OperationLog,
    mirror_stderr: bool,
    captured: &mut Vec<u8>,
) -> Result<(), OperationLogError> {
    let redacted = replace_bytes(bytes, secret);
    log.write_bytes(&redacted)?;
    if mirror_stderr {
        let _ = io::stderr().lock().write_all(&redacted);
    }
    let remaining = MAX_CAPTURE_BYTES.saturating_sub(captured.len());
    captured.extend_from_slice(&redacted[..redacted.len().min(remaining)]);
    Ok(())
}

fn replace_bytes(bytes: &[u8], secret: Option<&[u8]>) -> Vec<u8> {
    let Some(secret) = secret.filter(|value| !value.is_empty()) else {
        return bytes.to_vec();
    };
    let mut output = Vec::with_capacity(bytes.len());
    let mut remaining = bytes;
    while let Some(index) = remaining
        .windows(secret.len())
        .position(|window| window == secret)
    {
        output.extend_from_slice(&remaining[..index]);
        output.extend_from_slice(b"[REDACTED]");
        remaining = &remaining[index + secret.len()..];
    }
    output.extend_from_slice(remaining);
    output
}

fn clone_redactions(source: &str) -> Vec<Vec<u8>> {
    let mut values = vec![source.as_bytes().to_vec()];
    if let Ok(url) = Url::parse(source) {
        if matches!(url.scheme(), "http" | "https") && !url.username().is_empty() {
            values.push(url.username().as_bytes().to_vec());
        }
        if let Some(password) = url.password().filter(|value| !value.is_empty()) {
            values.push(password.as_bytes().to_vec());
        }
        if let Some(query) = url.query().filter(|value| !value.is_empty()) {
            values.push(query.as_bytes().to_vec());
            values.extend(
                url.query_pairs()
                    .map(|(_, value)| value.into_owned().into_bytes())
                    .filter(|value| !value.is_empty()),
            );
        }
        if let Some(fragment) = url.fragment().filter(|value| !value.is_empty()) {
            values.push(fragment.as_bytes().to_vec());
        }
    }
    values.sort();
    values.dedup();
    values
}

fn join_reader(
    handle: thread::JoinHandle<Result<Vec<u8>, OperationLogError>>,
    operation: &'static str,
    stream: &'static str,
) -> Result<Vec<u8>, GitError> {
    handle
        .join()
        .map_err(|_| GitError::Capture {
            operation,
            stream,
            source: io::Error::other("Git stream reader panicked"),
        })?
        .map_err(|source| GitError::Capture {
            operation,
            stream,
            source: io::Error::other(source),
        })
}

fn wait_for_child(
    child: &mut Child,
    cancellation: &CancellationToken,
    operation: &'static str,
    owns_process_group: bool,
) -> Result<(ExitStatus, bool), GitError> {
    let mut cancellation_started = None;
    loop {
        if let Some(status) = child
            .try_wait()
            .map_err(|source| GitError::Wait { operation, source })?
        {
            return Ok((status, cancellation_started.is_some()));
        }
        if cancellation.is_cancelled() && cancellation_started.is_none() {
            terminate_child(child, owns_process_group)
                .map_err(|source| GitError::Wait { operation, source })?;
            cancellation_started = Some(Instant::now());
        } else if cancellation_started.is_some_and(|started| started.elapsed() >= TERMINATION_GRACE)
        {
            child
                .kill()
                .map_err(|source| GitError::Wait { operation, source })?;
        }
        thread::sleep(POLL_INTERVAL);
    }
}

fn parse_version(bytes: &[u8]) -> Result<GitVersion, GitError> {
    let text = std::str::from_utf8(bytes).map_err(|_| GitError::InvalidVersion)?;
    let version = text
        .trim()
        .strip_prefix("git version ")
        .ok_or(GitError::InvalidVersion)?;
    let mut components = version.split('.');
    let major = parse_numeric_component(components.next())?;
    let minor = parse_numeric_component(components.next())?;
    let patch = components.next().map(parse_leading_number).transpose()?;
    Ok(GitVersion {
        major,
        minor,
        patch,
    })
}

fn parse_numeric_component(component: Option<&str>) -> Result<u32, GitError> {
    component
        .ok_or(GitError::InvalidVersion)?
        .parse()
        .map_err(|_| GitError::InvalidVersion)
}

fn parse_leading_number(component: &str) -> Result<u32, GitError> {
    let digits = component
        .get(
            ..component
                .find(|character: char| !character.is_ascii_digit())
                .unwrap_or(component.len()),
        )
        .ok_or(GitError::InvalidVersion)?;
    if digits.is_empty() {
        return Err(GitError::InvalidVersion);
    }
    digits.parse().map_err(|_| GitError::InvalidVersion)
}

fn bounded_summary(bytes: &[u8]) -> String {
    const MAXIMUM: usize = 1024;
    let text = String::from_utf8_lossy(bytes);
    let mut summary = text.replace(char::is_control, " ");
    if summary.len() > MAXIMUM {
        let mut boundary = MAXIMUM;
        while !summary.is_char_boundary(boundary) {
            boundary -= 1;
        }
        summary.truncate(boundary);
    }
    summary
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) -> bool {
    use std::io::IsTerminal;
    use std::os::unix::process::CommandExt;

    if io::stdin().is_terminal() {
        return false;
    }
    command.process_group(0);
    true
}

#[cfg(not(unix))]
const fn configure_process_group(_command: &mut Command) -> bool {
    false
}

#[cfg(unix)]
fn terminate_child(child: &mut Child, owns_process_group: bool) -> io::Result<()> {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;

    if !owns_process_group {
        return child.kill();
    }
    let process_group = i32::try_from(child.id())
        .map(Pid::from_raw)
        .map_err(|_| io::Error::other("Git process identifier exceeds platform range"))?;
    killpg(process_group, Signal::SIGTERM).map_err(io::Error::other)
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child, _owns_process_group: bool) -> io::Result<()> {
    child.kill()
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
    fn version_parser_accepts_vendor_suffix() {
        assert_eq!(
            parse_version(b"git version 2.47.1.windows.1\n").expect("version should parse"),
            GitVersion {
                major: 2,
                minor: 47,
                patch: Some(1)
            }
        );
    }

    #[test]
    fn byte_replacement_redacts_every_occurrence() {
        assert_eq!(
            replace_bytes(b"before secret after secret", Some(b"secret")),
            b"before [REDACTED] after [REDACTED]"
        );
    }
}
