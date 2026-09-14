//! Lookup-only delegation to trusted host Git credential helpers.
//!
//! This adapter is intentionally separate from ordinary subprocess logging: credential request,
//! response, and helper diagnostics remain only in bounded private pipes and memory. A successful
//! lookup is data, not authority: the broker must still authorize dispatch and recheck the grant
//! revision immediately before releasing the response.

use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::sync::Arc;
use std::time::Duration;

use cdenv_core::credential_protocol::{
    GitCredentialRequest, GitCredentialResponse, MAX_CREDENTIAL_BYTES,
};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

use crate::CancellationToken;

/// Maximum concurrently executing host Git credential helpers.
pub const MAX_HOST_GIT_HELPERS: usize = 4;
/// Maximum bytes read from helper stderr before the lookup is rejected.
pub const MAX_HOST_GIT_STDERR_BYTES: usize = 8 * 1024;
/// Maximum duration of one host Git lookup, including process cleanup.
pub const HOST_GIT_LOOKUP_TIMEOUT: Duration = Duration::from_secs(30);
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const TERMINATION_GRACE: Duration = Duration::from_secs(2);

const TRUSTED_ENVIRONMENT_NAMES: &[&str] = &[
    "DBUS_SESSION_BUS_ADDRESS",
    "GH_CONFIG_DIR",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GCM_CREDENTIAL_STORE",
    "HOME",
    "LANG",
    "LC_ALL",
    "PATH",
    "XDG_CONFIG_DIRS",
    "XDG_CONFIG_HOME",
];

/// Read-only environment captured from the explicit host invocation.
pub trait HostGitLaunchEnvironment {
    /// Returns one host launch variable. Only the adapter's closed allowlist is queried.
    fn variable(&self, name: &str) -> Option<OsString>;
}

/// The current cdenv host process environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessHostGitLaunchEnvironment;

impl HostGitLaunchEnvironment for ProcessHostGitLaunchEnvironment {
    fn variable(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }
}

/// Resolved, trusted execution context for host Git.
///
/// The neutral directory is canonicalized and must not itself be a Git worktree. Repository
/// discovery is additionally stopped at that directory when a lookup executes.
#[derive(Clone)]
pub struct HostGitCredentialContext {
    executable: PathBuf,
    neutral_directory: PathBuf,
    environment: Vec<(OsString, OsString)>,
    askpass_suppressor: PathBuf,
}

impl std::fmt::Debug for HostGitCredentialContext {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostGitCredentialContext([TRUSTED HOST CONTEXT])")
    }
}

impl HostGitCredentialContext {
    /// Resolves a context from explicit host-owned inputs.
    ///
    /// Git execution-control variables (`GIT_DIR`, `GIT_CONFIG_COUNT`, `GIT_CONFIG_KEY_*`, and
    /// similar values) are never imported. Trusted user/system configuration is consequently
    /// selected by `HOME`/XDG and Git's normal platform rules, not by the container request.
    ///
    /// # Errors
    ///
    /// Rejects a missing, non-directory, or repository-bearing neutral directory.
    pub fn resolve(
        executable: PathBuf,
        neutral_directory: &Path,
        environment: &impl HostGitLaunchEnvironment,
    ) -> Result<Self, HostGitContextError> {
        let neutral_directory = neutral_directory
            .canonicalize()
            .map_err(|source| HostGitContextError::NeutralDirectory { source })?;
        if !neutral_directory.is_dir()
            || neutral_directory.join(".git").try_exists().unwrap_or(true)
        {
            return Err(HostGitContextError::UnsafeNeutralDirectory);
        }
        let variables = TRUSTED_ENVIRONMENT_NAMES
            .iter()
            .filter_map(|name| {
                environment
                    .variable(name)
                    .map(|value| (OsString::from(name), value))
            })
            .collect();
        Ok(Self {
            executable,
            neutral_directory,
            environment: variables,
            askpass_suppressor: false_executable(),
        })
    }
}

/// Failure to establish a controlled host Git context.
#[derive(Debug, Error)]
pub enum HostGitContextError {
    /// The neutral directory could not be resolved.
    #[error("cannot resolve the host Git neutral directory")]
    NeutralDirectory {
        /// Underlying filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The directory is not neutral or cannot be inspected safely.
    #[error("host Git requires an existing neutral directory without repository metadata")]
    UnsafeNeutralDirectory,
}

/// Current lookup availability. It contains no credential or helper output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostGitCredentialUnavailable {
    /// The configured Git executable could not be started.
    GitMissing,
    /// Helper admission is currently full.
    Saturated,
    /// Git or a helper failed, including login-required states.
    LookupFailed,
    /// The helper exceeded its output bounds.
    OutputLimit,
    /// The helper exceeded the lookup deadline.
    TimedOut,
    /// No complete, unexpired credential was returned.
    NoCredential,
    /// The helper returned malformed, conflicting, or unsupported fields.
    UnsupportedResult,
}

impl HostGitCredentialUnavailable {
    /// Concise value-free remediation suitable for a host diagnostic.
    #[must_use]
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::GitMissing => "install or configure host Git, then retry",
            Self::Saturated => "retry after another host credential lookup finishes",
            Self::OutputLimit => "repair the host credential helper output, then retry",
            Self::TimedOut => "authenticate or repair the host credential helper, then retry",
            Self::LookupFailed | Self::NoCredential | Self::UnsupportedResult => {
                "authenticate with the configured helper on the host, then retry"
            }
        }
    }
}

/// Result of one uncached lookup.
pub enum HostGitCredentialOutcome {
    /// A validated credential, retained only in zeroizing memory.
    Available(GitCredentialResponse),
    /// A typed backend-health outcome.
    Unavailable(HostGitCredentialUnavailable),
}

impl std::fmt::Debug for HostGitCredentialOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Available(_) => formatter.write_str("Available([REDACTED])"),
            Self::Unavailable(reason) => {
                formatter.debug_tuple("Unavailable").field(reason).finish()
            }
        }
    }
}

/// Secret-safe, bounded host Git credential lookup adapter.
#[derive(Clone, Debug)]
pub struct HostGitCredentialAdapter {
    context: HostGitCredentialContext,
    admission: Arc<Semaphore>,
    timeout: Duration,
}

impl HostGitCredentialAdapter {
    /// Creates an adapter with the published concurrency and deadline limits.
    #[must_use]
    pub fn new(context: HostGitCredentialContext) -> Self {
        Self {
            context,
            admission: Arc::new(Semaphore::new(MAX_HOST_GIT_HELPERS)),
            timeout: HOST_GIT_LOOKUP_TIMEOUT,
        }
    }

    #[cfg(test)]
    fn with_limits(context: HostGitCredentialContext, helpers: usize, timeout: Duration) -> Self {
        Self {
            context,
            admission: Arc::new(Semaphore::new(helpers)),
            timeout,
        }
    }

    /// Consults current host helpers using `git credential fill` and no cdenv cache.
    ///
    /// The request is sent only on stdin. Fixed arguments and a closed environment prevent a
    /// container caller from selecting a command, configuration, checkout, or helper context.
    /// Stderr is discarded after a small bound and is never logged or included in errors.
    ///
    /// # Errors
    ///
    /// Returns only cancellation or an internal private-pipe/process-cleanup failure. Expected
    /// backend failures are represented by [`HostGitCredentialOutcome::Unavailable`].
    pub async fn lookup(
        &self,
        request: &GitCredentialRequest,
        now_unix_seconds: u64,
        cancellation: &CancellationToken,
    ) -> Result<HostGitCredentialOutcome, HostGitCredentialError> {
        let Ok(_permit) = self.admission.clone().try_acquire_owned() else {
            return Ok(HostGitCredentialOutcome::Unavailable(
                HostGitCredentialUnavailable::Saturated,
            ));
        };
        if cancellation.is_cancelled() {
            return Err(HostGitCredentialError::Cancelled);
        }
        let mut input = Zeroizing::new(Vec::new());
        request
            .write_private(&mut *input)
            .map_err(|_| HostGitCredentialError::PrivateIo)?;
        let execution = self.execute(input, cancellation).await?;
        let output = match execution {
            ExecutionOutcome::Unavailable(reason) => {
                return Ok(HostGitCredentialOutcome::Unavailable(reason));
            }
            ExecutionOutcome::Completed { status, stdout } => {
                if !status.success() {
                    return Ok(HostGitCredentialOutcome::Unavailable(
                        HostGitCredentialUnavailable::LookupFailed,
                    ));
                }
                stdout
            }
        };
        match GitCredentialResponse::parse(&output, request, now_unix_seconds) {
            Ok(Some(response)) => Ok(HostGitCredentialOutcome::Available(response)),
            Ok(None) => Ok(HostGitCredentialOutcome::Unavailable(
                HostGitCredentialUnavailable::NoCredential,
            )),
            Err(_) => Ok(HostGitCredentialOutcome::Unavailable(
                HostGitCredentialUnavailable::UnsupportedResult,
            )),
        }
    }

    fn command(&self) -> Command {
        let arguments: [&OsStr; 8] = [
            OsStr::new("-c"),
            OsStr::new("core.askPass="),
            OsStr::new("-c"),
            OsStr::new("credential.interactive=never"),
            OsStr::new("-c"),
            OsStr::new("credential.useHttpPath=true"),
            OsStr::new("credential"),
            OsStr::new("fill"),
        ];
        let mut command = Command::new(&self.context.executable);
        command
            .args(arguments)
            .current_dir(&self.context.neutral_directory)
            .env_clear()
            .envs(
                self.context
                    .environment
                    .iter()
                    .map(|(name, value)| (name, value)),
            )
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_ASKPASS", &self.context.askpass_suppressor)
            .env("SSH_ASKPASS", &self.context.askpass_suppressor)
            .env("GCM_INTERACTIVE", "Never")
            .env("GCM_GUI_PROMPT", "0")
            .env("GH_PROMPT_DISABLED", "1")
            .env("GIT_DISCOVERY_ACROSS_FILESYSTEM", "0")
            .env("GIT_CEILING_DIRECTORIES", &self.context.neutral_directory)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        configure_process_group(&mut command);
        command
    }

    async fn execute(
        &self,
        input: Zeroizing<Vec<u8>>,
        cancellation: &CancellationToken,
    ) -> Result<ExecutionOutcome, HostGitCredentialError> {
        let mut child = match self.command().spawn() {
            Ok(child) => child,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ExecutionOutcome::Unavailable(
                    HostGitCredentialUnavailable::GitMissing,
                ));
            }
            Err(_) => return Err(HostGitCredentialError::PrivateIo),
        };
        let mut process_group = OwnedProcessGroup::new(&child);
        let mut stdin = child
            .stdin
            .take()
            .ok_or(HostGitCredentialError::PrivateIo)?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or(HostGitCredentialError::PrivateIo)?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or(HostGitCredentialError::PrivateIo)?;

        let result = {
            let operation = async {
                let write = async move {
                    stdin.write_all(&input).await?;
                    stdin.shutdown().await
                };
                let read_stdout = read_bounded(&mut stdout, MAX_CREDENTIAL_BYTES);
                let read_stderr = read_bounded(&mut stderr, MAX_HOST_GIT_STDERR_BYTES);
                let wait = child.wait();
                tokio::try_join!(write, read_stdout, read_stderr, wait)
            };
            tokio::pin!(operation);
            let deadline = tokio::time::sleep(self.timeout);
            tokio::pin!(deadline);
            loop {
                tokio::select! {
                    result = &mut operation => break Some(result),
                    () = &mut deadline => break None,
                    () = tokio::time::sleep(CANCELLATION_POLL_INTERVAL) => {
                        if cancellation.is_cancelled() { break None; }
                    }
                }
            }
        };
        match result {
            Some(Ok(((), stdout, stderr, status))) => {
                process_group.disarm();
                if stdout.exceeded || stderr.exceeded {
                    Ok(ExecutionOutcome::Unavailable(
                        HostGitCredentialUnavailable::OutputLimit,
                    ))
                } else {
                    Ok(ExecutionOutcome::Completed {
                        status,
                        stdout: stdout.bytes,
                    })
                }
            }
            Some(Err(_)) => {
                terminate_and_reap(&mut child).await?;
                process_group.disarm();
                Err(HostGitCredentialError::PrivateIo)
            }
            None => {
                terminate_and_reap(&mut child).await?;
                process_group.disarm();
                if cancellation.is_cancelled() {
                    Err(HostGitCredentialError::Cancelled)
                } else {
                    Ok(ExecutionOutcome::Unavailable(
                        HostGitCredentialUnavailable::TimedOut,
                    ))
                }
            }
        }
    }
}

struct BoundedPrivateOutput {
    bytes: Zeroizing<Vec<u8>>,
    exceeded: bool,
}

async fn read_bounded(
    reader: &mut (impl AsyncRead + Unpin),
    maximum: usize,
) -> io::Result<BoundedPrivateOutput> {
    let mut bytes = Zeroizing::new(Vec::new());
    let mut exceeded = false;
    let mut buffer = [0_u8; 4096];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            break;
        }
        let remaining = maximum.saturating_sub(bytes.len());
        bytes.extend_from_slice(&buffer[..count.min(remaining)]);
        exceeded |= count > remaining;
    }
    Ok(BoundedPrivateOutput { bytes, exceeded })
}

struct OwnedProcessGroup(Option<u32>);

impl OwnedProcessGroup {
    fn new(child: &Child) -> Self {
        Self(child.id())
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

#[cfg(unix)]
impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {
        use nix::sys::signal::{Signal, killpg};
        use nix::unistd::Pid;

        if let Some(pid) = self.0.and_then(|pid| i32::try_from(pid).ok()) {
            let _ = killpg(Pid::from_raw(pid), Signal::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
impl Drop for OwnedProcessGroup {
    fn drop(&mut self) {}
}

async fn terminate_and_reap(child: &mut Child) -> Result<(), HostGitCredentialError> {
    if child
        .try_wait()
        .map_err(|_| HostGitCredentialError::Cleanup)?
        .is_some()
    {
        return Ok(());
    }
    terminate_process_group(child, false).map_err(|_| HostGitCredentialError::Cleanup)?;
    if tokio::time::timeout(TERMINATION_GRACE, child.wait())
        .await
        .is_err()
    {
        terminate_process_group(child, true).map_err(|_| HostGitCredentialError::Cleanup)?;
        child
            .wait()
            .await
            .map_err(|_| HostGitCredentialError::Cleanup)?;
    }
    Ok(())
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

    let pid = i32::try_from(
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
    match killpg(pid, signal) {
        Ok(()) | Err(nix::errno::Errno::ESRCH) => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child, _force: bool) -> io::Result<()> {
    child.start_kill()
}

#[cfg(unix)]
fn false_executable() -> PathBuf {
    PathBuf::from("/usr/bin/false")
}

#[cfg(not(unix))]
fn false_executable() -> PathBuf {
    PathBuf::from("false")
}

enum ExecutionOutcome {
    Completed {
        status: ExitStatus,
        stdout: Zeroizing<Vec<u8>>,
    },
    Unavailable(HostGitCredentialUnavailable),
}

/// Internal lookup interruption. It never contains request, response, or helper output.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HostGitCredentialError {
    /// Revocation, broker disconnect, or caller cancellation stopped the lookup.
    #[error("host Git credential lookup was cancelled")]
    Cancelled,
    /// A private pipe or subprocess operation failed.
    #[error("host Git credential private subprocess failed")]
    PrivateIo,
    /// Owned helper subprocess work could not be reaped.
    #[error("host Git credential subprocess cleanup failed")]
    Cleanup,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[derive(Default)]
    struct Environment(BTreeMap<String, OsString>);
    impl HostGitLaunchEnvironment for Environment {
        fn variable(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    fn fixture(script: &str) -> (tempfile::TempDir, HostGitCredentialContext) {
        let temporary = tempfile::tempdir().expect("temporary");
        let neutral = temporary.path().join("neutral");
        fs::create_dir(&neutral).expect("neutral");
        let executable = temporary.path().join("git-fixture");
        fs::write(&executable, format!("#!/bin/sh\nset -eu\n{script}\n")).expect("script");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("mode");
        let environment = Environment(BTreeMap::from([
            ("HOME".to_owned(), OsString::from("/trusted/home")),
            ("PATH".to_owned(), OsString::from("/usr/bin:/bin")),
            ("GIT_CONFIG_COUNT".to_owned(), OsString::from("1")),
            ("SECRET_TOKEN".to_owned(), OsString::from("SECRET-MARKER")),
        ]));
        let context =
            HostGitCredentialContext::resolve(executable, &neutral, &environment).expect("context");
        (temporary, context)
    }

    fn request(path: &str, username: &str) -> GitCredentialRequest {
        GitCredentialRequest::parse(
            format!("protocol=https\nhost=git.internal:8443\npath={path}\nusername={username}\n\n")
                .as_bytes(),
        )
        .expect("request")
    }

    #[tokio::test]
    async fn controlled_context_preserves_path_and_username_without_injection() {
        let script = r#"
base=$(dirname "$0")
pwd > "$base/cwd"
printf '%s\n' "$@" > "$base/args"
env | sort > "$base/env"
cat > "$base/stdin"
printf 'username=alice\npassword=ROTATED-TOKEN\n\n'
"#;
        let (temporary, context) = fixture(script);
        let adapter = HostGitCredentialAdapter::new(context);
        let outcome = adapter
            .lookup(
                &request("team/one.git", "alice"),
                100,
                &CancellationToken::default(),
            )
            .await
            .expect("lookup");
        assert!(matches!(outcome, HostGitCredentialOutcome::Available(_)));
        assert_eq!(
            fs::read_to_string(temporary.path().join("stdin")).expect("stdin"),
            "protocol=https\nhost=git.internal:8443\npath=team/one.git\nusername=alice\n\n"
        );
        let arguments = fs::read_to_string(temporary.path().join("args")).expect("args");
        assert_eq!(
            arguments,
            "-c\ncore.askPass=\n-c\ncredential.interactive=never\n-c\ncredential.useHttpPath=true\ncredential\nfill\n"
        );
        let environment = fs::read_to_string(temporary.path().join("env")).expect("env");
        assert!(environment.contains("GIT_TERMINAL_PROMPT=0"));
        assert!(environment.contains("GCM_INTERACTIVE=Never"));
        assert!(environment.contains("GH_PROMPT_DISABLED=1"));
        assert!(!environment.contains("GIT_CONFIG_COUNT"));
        assert!(!environment.contains("SECRET-MARKER"));
        assert_eq!(
            fs::read_to_string(temporary.path().join("cwd")).expect("cwd"),
            temporary.path().join("neutral").display().to_string() + "\n"
        );
    }

    #[tokio::test]
    async fn every_lookup_observes_rotation_and_keeps_context_distinct() {
        let script = r#"
base=$(dirname "$0")
count=$(cat "$base/count" 2>/dev/null || printf 0)
count=$((count + 1)); printf '%s' "$count" > "$base/count"
body=$(cat)
user=$(printf '%s' "$body" | awk -F= '$1 == "username" { print $2 }')
path=$(printf '%s' "$body" | awk -F= '$1 == "path" { print $2 }')
printf 'username=%s\npassword=token-%s-%s-%s\n\n' "$user" "$count" "$path" "$user"
"#;
        let (_temporary, context) = fixture(script);
        let adapter = HostGitCredentialAdapter::new(context);
        for (path, username) in [("one.git", "alice"), ("two.git", "bob")] {
            assert!(matches!(
                adapter
                    .lookup(&request(path, username), 1, &CancellationToken::default())
                    .await,
                Ok(HostGitCredentialOutcome::Available(_))
            ));
        }
    }

    #[tokio::test]
    async fn incomplete_expired_unknown_failed_and_noisy_results_are_unavailable() {
        for (script, expected) in [
            (
                "cat >/dev/null; printf 'username=alice\\n\\n'",
                HostGitCredentialUnavailable::NoCredential,
            ),
            (
                "cat >/dev/null; printf 'username=alice\\npassword=x\\npassword_expiry_utc=10\\n\\n'",
                HostGitCredentialUnavailable::NoCredential,
            ),
            (
                "cat >/dev/null; printf 'username=alice\\npassword=x\\nrefresh_token=SECRET\\n\\n'",
                HostGitCredentialUnavailable::UnsupportedResult,
            ),
            (
                "cat >/dev/null; printf 'SECRET' >&2; exit 1",
                HostGitCredentialUnavailable::LookupFailed,
            ),
            (
                "cat >/dev/null; yes x | head -c 9000 >&2",
                HostGitCredentialUnavailable::OutputLimit,
            ),
        ] {
            let (_temporary, context) = fixture(script);
            let result = HostGitCredentialAdapter::new(context)
                .lookup(
                    &request("repo.git", "alice"),
                    10,
                    &CancellationToken::default(),
                )
                .await
                .expect("typed outcome");
            assert!(
                matches!(result, HostGitCredentialOutcome::Unavailable(reason) if reason == expected)
            );
            assert!(!expected.guidance().contains("SECRET"));
        }
    }

    #[tokio::test]
    async fn deadline_cancels_the_owned_process_group_and_reaps_git() {
        let script = "base=$(dirname \"$0\"); echo $$ > \"$base/pid\"; cat >/dev/null; sleep 30";
        let (temporary, context) = fixture(script);
        let adapter = HostGitCredentialAdapter::with_limits(context, 1, Duration::from_millis(100));
        let result = adapter
            .lookup(
                &request("repo.git", "alice"),
                1,
                &CancellationToken::default(),
            )
            .await
            .expect("outcome");
        assert!(matches!(
            result,
            HostGitCredentialOutcome::Unavailable(HostGitCredentialUnavailable::TimedOut)
        ));
        let pid = fs::read_to_string(temporary.path().join("pid")).expect("pid");
        let status = std::process::Command::new("kill")
            .args(["-0", pid.trim()])
            .status()
            .expect("kill probe");
        assert!(!status.success());
    }

    #[tokio::test]
    async fn admission_and_explicit_cancellation_are_bounded() {
        let script =
            "base=$(dirname \"$0\"); echo ready > \"$base/ready\"; cat >/dev/null; sleep 30";
        let (temporary, context) = fixture(script);
        let adapter = HostGitCredentialAdapter::with_limits(context, 1, Duration::from_secs(5));
        let cancellation = CancellationToken::default();
        let task_adapter = adapter.clone();
        let task_cancellation = cancellation.clone();
        let task = tokio::spawn(async move {
            task_adapter
                .lookup(&request("one.git", "alice"), 1, &task_cancellation)
                .await
        });
        for _ in 0..100 {
            if temporary.path().join("ready").exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let second = adapter
            .lookup(&request("two.git", "bob"), 1, &CancellationToken::default())
            .await
            .expect("admission outcome");
        assert!(matches!(
            second,
            HostGitCredentialOutcome::Unavailable(HostGitCredentialUnavailable::Saturated)
        ));
        cancellation.cancel();
        assert_eq!(
            task.await.expect("join").expect_err("cancelled"),
            HostGitCredentialError::Cancelled
        );
    }

    #[tokio::test]
    async fn trusted_host_include_and_url_helper_matching_are_applied_by_git() {
        let temporary = tempfile::tempdir().expect("temporary");
        let neutral = temporary.path().join("neutral");
        let home = temporary.path().join("home");
        fs::create_dir(&neutral).expect("neutral");
        fs::create_dir(&home).expect("home");
        let helper = temporary.path().join("helper");
        fs::write(
            &helper,
            "#!/bin/sh\ncat > \"$(dirname \"$0\")/helper-input\"\nprintf 'password=from-trusted-helper\\n'\n",
        )
        .expect("helper");
        fs::set_permissions(&helper, fs::Permissions::from_mode(0o700)).expect("helper mode");
        let included = temporary.path().join("included.gitconfig");
        fs::write(
            &included,
            format!(
                "[credential \"https://git.internal:8443/team/repo.git\"]\n\tusername = alice\n\thelper = !{}\n",
                helper.display()
            ),
        )
        .expect("included config");
        fs::write(
            home.join(".gitconfig"),
            format!("[include]\n\tpath = {}\n", included.display()),
        )
        .expect("global config");
        let environment = Environment(BTreeMap::from([
            ("HOME".to_owned(), home.into_os_string()),
            ("PATH".to_owned(), OsString::from("/usr/bin:/bin")),
        ]));
        let context =
            HostGitCredentialContext::resolve(PathBuf::from("git"), &neutral, &environment)
                .expect("context");
        let result = HostGitCredentialAdapter::new(context)
            .lookup(
                &request("team/repo.git", "alice"),
                1,
                &CancellationToken::default(),
            )
            .await
            .expect("lookup");
        assert!(matches!(result, HostGitCredentialOutcome::Available(_)));
        let observed = fs::read_to_string(temporary.path().join("helper-input")).expect("input");
        assert!(observed.contains("protocol=https\n"));
        assert!(observed.contains("host=git.internal:8443\n"));
        assert!(observed.contains("path=team/repo.git\n"));
        assert!(observed.contains("username=alice\n"));
    }

    #[test]
    fn repository_directory_is_not_accepted_as_neutral_context() {
        let temporary = tempfile::tempdir().expect("temporary");
        fs::create_dir(temporary.path().join(".git")).expect("git marker");
        assert!(matches!(
            HostGitCredentialContext::resolve(
                PathBuf::from("git"),
                temporary.path(),
                &Environment::default()
            ),
            Err(HostGitContextError::UnsafeNeutralDirectory)
        ));
    }
}
