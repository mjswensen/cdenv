//! Authenticated SSH over a generic byte stream with non-PTY process execution.

use std::collections::{BTreeMap, HashMap};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::{Pid, geteuid};
use russh::keys::ssh_key::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, MethodKind, Sig};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::mpsc;

use crate::pty_linux::{self, PtyRequest};
use crate::{EnvironmentError, EnvironmentSnapshot};

const SSH_USERNAME: &str = "cdenv";
const CHANNEL_WINDOW_BYTES: u32 = 2 * 1024 * 1024;
const MAXIMUM_PACKET_BYTES: u32 = 32 * 1024;
const CHANNEL_MESSAGE_BUFFER: usize = 64;
const EVENT_BUFFER: usize = 64;
const MAXIMUM_CLIENT_ENVIRONMENT_ENTRIES: usize = 256;
const MAXIMUM_CLIENT_ENVIRONMENT_BYTES: usize = 64 * 1024;
const DISCONNECT_GRACE: Duration = Duration::from_millis(100);

/// Files and active-plan facts required by one SSH stdio transport.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshServerRequest {
    /// Persistent workspace host private key installed with mode `0600`.
    pub host_key: PathBuf,
    /// Exact installation client public key installed with mode `0600`.
    pub authorized_key: PathBuf,
    /// Recaptured effective environment snapshot for this generation.
    pub environment: PathBuf,
    /// Authoritative active remote workspace folder.
    pub workspace: PathBuf,
}

/// Loaded immutable SSH transport configuration.
pub struct SshServerConfig {
    host_key: PrivateKey,
    authorized_key: PublicKey,
    environment: Arc<EnvironmentSnapshot>,
    workspace: Arc<PathBuf>,
    shell: Arc<OsString>,
}

impl std::fmt::Debug for SshServerConfig {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SshServerConfig")
            .field("host_key", &"[REDACTED]")
            .field("authorized_key", &"[PUBLIC KEY]")
            .field("environment", &self.environment)
            .field("workspace", &self.workspace)
            .field("shell", &self.shell)
            .finish()
    }
}

impl SshServerConfig {
    /// Securely loads keys, the effective environment, workspace, and selected shell.
    ///
    /// The host and authorized key files must be regular non-symlink files owned
    /// by the effective user with exact mode `0600`. Both keys must be Ed25519.
    ///
    /// # Errors
    ///
    /// Returns [`SshServerError`] for unsafe key files, malformed or wrong-role
    /// keys, an invalid environment snapshot, or an unavailable workspace.
    pub fn load(request: &SshServerRequest) -> Result<Self, SshServerError> {
        inspect_key_file(&request.host_key, "host private")?;
        inspect_key_file(&request.authorized_key, "authorized client public")?;
        let host_key = russh::keys::load_secret_key(&request.host_key, None)?;
        let authorized_key = russh::keys::load_public_key(&request.authorized_key)?;
        if host_key.algorithm() != Algorithm::Ed25519 {
            return Err(SshServerError::WrongKeyAlgorithm {
                role: "host private",
            });
        }
        if authorized_key.algorithm() != Algorithm::Ed25519 {
            return Err(SshServerError::WrongKeyAlgorithm {
                role: "authorized client public",
            });
        }
        let workspace_metadata = fs::symlink_metadata(&request.workspace).map_err(|source| {
            SshServerError::Workspace {
                path: request.workspace.clone(),
                source,
            }
        })?;
        if workspace_metadata.file_type().is_symlink() || !workspace_metadata.is_dir() {
            return Err(SshServerError::UnsafeWorkspace {
                path: request.workspace.clone(),
            });
        }
        let environment = EnvironmentSnapshot::load(&request.environment)?;
        let shell = resolve_shell(&environment);
        Ok(Self {
            host_key,
            authorized_key,
            environment: Arc::new(environment),
            workspace: Arc::new(request.workspace.clone()),
            shell: Arc::new(shell),
        })
    }

    #[cfg(test)]
    fn from_parts(
        host_key: PrivateKey,
        authorized_key: PublicKey,
        environment: EnvironmentSnapshot,
        workspace: PathBuf,
        shell: OsString,
    ) -> Self {
        Self {
            host_key,
            authorized_key,
            environment: Arc::new(environment),
            workspace: Arc::new(workspace),
            shell: Arc::new(shell),
        }
    }
}

/// SSH authentication, protocol, configuration, or process failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SshServerError {
    /// A key file failed secure inspection.
    #[error("SSH {role} key file is not a regular, owner-only file: {path:?}")]
    UnsafeKeyFile {
        /// The expected key role.
        role: &'static str,
        /// The rejected path.
        path: PathBuf,
    },
    /// A loaded key used an unsupported algorithm.
    #[error("SSH {role} key must be Ed25519")]
    WrongKeyAlgorithm {
        /// The expected key role.
        role: &'static str,
    },
    /// A key could not be decoded.
    #[error(transparent)]
    Key(#[from] russh::keys::Error),
    /// Effective environment loading failed.
    #[error(transparent)]
    Environment(#[from] EnvironmentError),
    /// The active workspace could not be inspected.
    #[error("cannot inspect SSH workspace {path:?}: {source}")]
    Workspace {
        /// The active workspace path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// The workspace was not a real directory.
    #[error("SSH workspace must be a non-symlink directory: {path:?}")]
    UnsafeWorkspace {
        /// The rejected workspace path.
        path: PathBuf,
    },
    /// SSH protocol processing failed.
    #[error(transparent)]
    Protocol(#[from] russh::Error),
    /// A child process or channel bridge failed.
    #[error("SSH process I/O failed: {0}")]
    ProcessIo(#[from] std::io::Error),
}

/// Serves one authenticated SSH connection over any asynchronous byte stream.
///
/// The stream is the only protocol output sink. This function and its module do
/// not write process stdout, so production can safely pass joined Tokio stdio.
/// Channel flow-control windows, packet sizes, and queues are bounded; channel
/// count remains governed by SSH and operating-system limits.
///
/// # Errors
///
/// Returns [`SshServerError`] when the SSH handshake or connection fails.
pub async fn serve_ssh_stream<S>(stream: S, config: SshServerConfig) -> Result<(), SshServerError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let SshServerConfig {
        host_key,
        authorized_key,
        environment,
        workspace,
        shell,
    } = config;
    let methods = russh::MethodSet::from(&[MethodKind::PublicKey][..]);
    let server = Arc::new(russh::server::Config {
        methods,
        auth_rejection_time: Duration::from_millis(100),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        inactivity_timeout: None,
        max_auth_attempts: 3,
        keepalive_interval: Some(Duration::from_secs(30)),
        keepalive_max: 3,
        window_size: CHANNEL_WINDOW_BYTES,
        maximum_packet_size: MAXIMUM_PACKET_BYTES,
        channel_buffer_size: CHANNEL_MESSAGE_BUFFER,
        event_buffer_size: EVENT_BUFFER,
        ..Default::default()
    });
    let handler = SshHandler {
        authorized_key,
        environment,
        workspace,
        shell,
        channels: Arc::new(Mutex::new(HashMap::new())),
    };
    let running = russh::server::run_stream(server, stream, handler).await?;
    running.await?;
    Ok(())
}

#[derive(Debug)]
enum ChannelState {
    Pending(PendingChannel),
    RunningProcess,
    RunningPty { resize: mpsc::Sender<PtyRequest> },
}

struct PendingChannel {
    channel: Channel<Msg>,
    environment: BTreeMap<String, String>,
    pty: Option<PtyRequest>,
}

impl std::fmt::Debug for PendingChannel {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingChannel")
            .field("channel", &self.channel.id())
            .field("environment", &self.environment.keys())
            .field("pty", &self.pty)
            .finish()
    }
}

struct SshHandler {
    authorized_key: PublicKey,
    environment: Arc<EnvironmentSnapshot>,
    workspace: Arc<PathBuf>,
    shell: Arc<OsString>,
    channels: Arc<Mutex<HashMap<ChannelId, ChannelState>>>,
}

impl russh::server::Handler for SshHandler {
    type Error = SshServerError;

    async fn auth_none(&mut self, _user: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_password(&mut self, _user: &str, _password: &str) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn auth_publickey_offered(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(authenticate(user, public_key, &self.authorized_key))
    }

    async fn auth_publickey(
        &mut self,
        user: &str,
        public_key: &PublicKey,
    ) -> Result<Auth, Self::Error> {
        Ok(authenticate(user, public_key, &self.authorized_key))
    }

    async fn auth_keyboard_interactive<'a>(
        &'a mut self,
        _user: &str,
        _submethods: &str,
        _response: Option<russh::server::Response<'a>>,
    ) -> Result<Auth, Self::Error> {
        Ok(Auth::reject())
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let id = channel.id();
        if channel_states(&self.channels)
            .insert(
                id,
                ChannelState::Pending(PendingChannel {
                    channel,
                    environment: BTreeMap::new(),
                    pty: None,
                }),
            )
            .is_some()
        {
            reply
                .reject(russh::ChannelOpenFailure::ResourceShortage)
                .await;
        } else {
            reply.accept().await;
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Ok(port) = u16::try_from(port_to_connect) else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        let Some(target) = direct_target(host_to_connect, port) else {
            reply.reject(russh::ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        match TcpStream::connect((target.as_str(), port)).await {
            Ok(stream) => {
                reply.accept().await;
                tokio::spawn(async move {
                    if let Err(error) = run_direct_tcpip_channel(channel, stream).await {
                        eprintln!("cdenv-agent: SSH direct-tcpip channel failed: {error}");
                    }
                });
            }
            Err(_) => reply.reject(russh::ChannelOpenFailure::ConnectFailed).await,
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        channel_states(&self.channels).remove(&channel);
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        variable_name: &str,
        variable_value: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let accepted = if allowed_client_environment(variable_name)
            && !variable_value.as_bytes().contains(&0)
        {
            if let Some(ChannelState::Pending(pending)) =
                channel_states(&self.channels).get_mut(&channel)
            {
                insert_client_environment(pending, variable_name, variable_value)
            } else {
                false
            }
        } else {
            false
        };
        if accepted {
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn pty_request(
        &mut self,
        channel: ChannelId,
        term: &str,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        modes: &[(russh::Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let accepted = channel_states(&self.channels)
            .get_mut(&channel)
            .and_then(|state| match state {
                ChannelState::Pending(pending) if pending.pty.is_none() => {
                    pending.pty = Some(PtyRequest {
                        term: term.to_owned(),
                        columns: col_width,
                        rows: row_height,
                        pixels_width: pix_width,
                        pixels_height: pix_height,
                        modes: modes.to_vec(),
                    });
                    Some(())
                }
                _ => None,
            })
            .is_some();
        if accepted {
            session.channel_success(channel)?;
        } else {
            session.channel_failure(channel)?;
        }
        Ok(())
    }

    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(ChannelState::Pending(pending)) = channel_states(&self.channels).remove(&channel)
        else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        let Some(pty) = pending.pty.clone() else {
            channel_states(&self.channels).insert(channel, ChannelState::Pending(pending));
            session.channel_failure(channel)?;
            return Ok(());
        };
        match spawn_pty_process(
            pending,
            &pty,
            None,
            &self.environment,
            &self.workspace,
            &self.shell,
            session.handle(),
            Arc::clone(&self.channels),
        ) {
            Ok(resize) => {
                channel_states(&self.channels).insert(channel, ChannelState::RunningPty { resize });
                session.channel_success(channel)?;
            }
            Err(error) => {
                session.channel_failure(channel)?;
                eprintln!("cdenv-agent: cannot start SSH PTY shell: {error}");
            }
        }
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let Some(ChannelState::Pending(pending)) = channel_states(&self.channels).remove(&channel)
        else {
            session.channel_failure(channel)?;
            return Ok(());
        };
        if let Some(pty) = pending.pty.clone() {
            match spawn_pty_process(
                pending,
                &pty,
                Some(command),
                &self.environment,
                &self.workspace,
                &self.shell,
                session.handle(),
                Arc::clone(&self.channels),
            ) {
                Ok(resize) => {
                    channel_states(&self.channels)
                        .insert(channel, ChannelState::RunningPty { resize });
                    session.channel_success(channel)?;
                }
                Err(error) => {
                    session.channel_failure(channel)?;
                    eprintln!("cdenv-agent: cannot start SSH PTY exec channel: {error}");
                }
            }
        } else {
            channel_states(&self.channels).insert(channel, ChannelState::RunningProcess);
            match spawn_exec_process(
                pending,
                command,
                &self.environment,
                &self.workspace,
                &self.shell,
                session.handle(),
                Arc::clone(&self.channels),
            ) {
                Ok(()) => session.channel_success(channel)?,
                Err(error) => {
                    channel_states(&self.channels).remove(&channel);
                    session.channel_failure(channel)?;
                    eprintln!("cdenv-agent: cannot start SSH exec channel: {error}");
                }
            }
        }
        Ok(())
    }

    async fn window_change_request(
        &mut self,
        channel: ChannelId,
        col_width: u32,
        row_height: u32,
        pix_width: u32,
        pix_height: u32,
        _session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(ChannelState::RunningPty { resize }) =
            channel_states(&self.channels).get(&channel)
        {
            let _ = resize.try_send(PtyRequest {
                term: String::new(),
                columns: col_width,
                rows: row_height,
                pixels_width: pix_width,
                pixels_height: pix_height,
                modes: Vec::new(),
            });
        }
        Ok(())
    }
}

fn direct_target(host: &str, port: u16) -> Option<String> {
    (port != 0
        && !host.is_empty()
        && host.len() <= 253
        && !host
            .bytes()
            .any(|byte| byte == 0 || byte.is_ascii_whitespace()))
    .then(|| host.to_owned())
}

async fn run_direct_tcpip_channel(
    channel: Channel<Msg>,
    stream: TcpStream,
) -> Result<(), SshServerError> {
    let (mut channel_read, channel_write) = channel.split();
    let (mut target_read, mut target_write) = stream.into_split();
    let mut channel_output = channel_write.make_writer();
    let mut output =
        tokio::spawn(async move { tokio::io::copy(&mut target_read, &mut channel_output).await });
    let mut input = tokio::spawn(async move {
        while let Some(message) = channel_read.wait().await {
            match message {
                ChannelMsg::Data { data } => target_write.write_all(&data).await?,
                ChannelMsg::Eof => {
                    target_write.shutdown().await?;
                    break;
                }
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        std::io::Result::Ok(())
    });
    tokio::select! {
        result = &mut input => {
            result.map_err(std::io::Error::other)??;
            output.await.map_err(std::io::Error::other)??;
        }
        result = &mut output => {
            result.map_err(std::io::Error::other)??;
            input.abort();
            let _ = input.await;
        }
    }
    channel_write.eof().await?;
    channel_write.close().await?;
    Ok(())
}

fn channel_states(
    channels: &Mutex<HashMap<ChannelId, ChannelState>>,
) -> MutexGuard<'_, HashMap<ChannelId, ChannelState>> {
    channels
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn authenticate(user: &str, key: &PublicKey, authorized: &PublicKey) -> Auth {
    if user == SSH_USERNAME && key == authorized {
        Auth::Accept
    } else {
        Auth::reject()
    }
}

fn allowed_client_environment(name: &str) -> bool {
    name == "LANG" || name.starts_with("LC_") || matches!(name, "TERM" | "COLORTERM" | "NO_COLOR")
}

fn insert_client_environment(pending: &mut PendingChannel, name: &str, value: &str) -> bool {
    let replaced_bytes = pending
        .environment
        .get(name)
        .map_or(0, |existing| name.len() + existing.len());
    let resulting_entries =
        pending.environment.len() + usize::from(!pending.environment.contains_key(name));
    let resulting_bytes = pending
        .environment
        .iter()
        .map(|(name, value)| name.len() + value.len())
        .sum::<usize>()
        .saturating_sub(replaced_bytes)
        .saturating_add(name.len())
        .saturating_add(value.len());
    if resulting_entries > MAXIMUM_CLIENT_ENVIRONMENT_ENTRIES
        || resulting_bytes > MAXIMUM_CLIENT_ENVIRONMENT_BYTES
    {
        return false;
    }
    pending
        .environment
        .insert(name.to_owned(), value.to_owned());
    true
}

fn spawn_exec_process(
    pending: PendingChannel,
    command_bytes: &[u8],
    environment: &EnvironmentSnapshot,
    workspace: &Path,
    shell: &OsStr,
    handle: russh::server::Handle,
    channels: Arc<Mutex<HashMap<ChannelId, ChannelState>>>,
) -> Result<(), SshServerError> {
    let original_command = OsString::from_vec(command_bytes.to_vec());
    let mut process = tokio::process::Command::new(shell);
    process
        .arg("-c")
        .arg(&original_command)
        .current_dir(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    process.process_group(0);
    environment.apply_to_tokio(&mut process);
    process.envs(&pending.environment);
    process.env("SSH_CONNECTION", "127.0.0.1 0 127.0.0.1 22");
    process.env("SSH_CLIENT", "127.0.0.1 0 22");
    process.env("SSH_ORIGINAL_COMMAND", &original_command);
    let mut child = process.spawn()?;
    let process_group = child
        .id()
        .map(|id| Pid::from_raw(id.cast_signed()))
        .ok_or_else(|| std::io::Error::other("spawned SSH command has no process id"))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| std::io::Error::other("spawned SSH command has no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| std::io::Error::other("spawned SSH command has no stdout"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| std::io::Error::other("spawned SSH command has no stderr"))?;
    let channel_id = pending.channel.id();
    tokio::spawn(async move {
        let result = run_exec_channel(
            pending.channel,
            child,
            stdin,
            stdout,
            stderr,
            process_group,
            handle,
        )
        .await;
        channel_states(&channels).remove(&channel_id);
        if let Err(error) = result {
            eprintln!("cdenv-agent: SSH exec channel failed: {error}");
        }
    });
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "SSH channel launch dependencies are explicit"
)]
fn spawn_pty_process(
    pending: PendingChannel,
    request: &PtyRequest,
    command: Option<&[u8]>,
    environment: &EnvironmentSnapshot,
    workspace: &Path,
    shell: &OsStr,
    handle: russh::server::Handle,
    channels: Arc<Mutex<HashMap<ChannelId, ChannelState>>>,
) -> Result<mpsc::Sender<PtyRequest>, SshServerError> {
    let (arguments, login_argv0, original_command) = if let Some(command) = command {
        let command = OsString::from_vec(command.to_vec());
        (
            vec![OsString::from("-c"), command.clone()],
            None,
            Some(command),
        )
    } else {
        let name = Path::new(shell).file_name().unwrap_or(shell);
        let mut login = OsString::from("-");
        login.push(name);
        (Vec::new(), Some(login), None)
    };
    let mut client_environment = pending.environment;
    client_environment.insert("TERM".to_owned(), request.term.clone());
    let process = pty_linux::spawn(
        shell,
        &arguments,
        login_argv0,
        environment.entries(),
        &client_environment,
        original_command.as_deref(),
        workspace,
        request,
    )?;
    let channel_id = pending.channel.id();
    let (resize, resize_receive) = mpsc::channel(8);
    tokio::spawn(async move {
        let result = run_pty_channel(pending.channel, process, resize_receive, handle).await;
        channel_states(&channels).remove(&channel_id);
        if let Err(error) = result {
            eprintln!("cdenv-agent: SSH PTY channel failed: {error}");
        }
    });
    Ok(resize)
}

async fn run_pty_channel(
    channel: Channel<Msg>,
    mut process: pty_linux::PtyProcess,
    mut resize: mpsc::Receiver<PtyRequest>,
    handle: russh::server::Handle,
) -> Result<(), SshServerError> {
    let channel_id = channel.id();
    let process_group = process
        .child
        .id()
        .map(|id| Pid::from_raw(id.cast_signed()))
        .ok_or_else(|| std::io::Error::other("spawned SSH PTY has no process id"))?;
    let (mut channel_read, channel_write) = channel.split();
    let (mut pty_read, mut pty_write) = process.pty.into_split();
    let mut output = channel_write.make_writer();
    let output_task =
        tokio::spawn(async move { tokio::io::copy(&mut pty_read, &mut output).await });
    let (disconnect_send, mut disconnect_receive) = tokio::sync::oneshot::channel();
    let input_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                message = channel_read.wait() => match message {
                    Some(ChannelMsg::Data { data }) => if pty_write.write_all(&data).await.is_err() { break; },
                    Some(ChannelMsg::Close) | None => { let _ = disconnect_send.send(()); break; },
                    Some(ChannelMsg::Signal { signal }) => if let Some(signal) = client_signal(&signal) { let _ = killpg(process_group, signal); },
                    _ => {}
                },
                Some(next) = resize.recv() => { let _ = pty_write.resize(pty_linux::size(&next)); },
            }
        }
    });
    let status = tokio::select! {
        status = process.child.wait() => status?,
        disconnected = &mut disconnect_receive => { if disconnected.is_ok() { terminate_process_group(process_group).await; } process.child.wait().await? }
    };
    input_task.abort();
    let _ = input_task.await;
    let _ = output_task.await;
    report_exit(&handle, channel_id, &channel_write, status).await?;
    channel_write.eof().await?;
    channel_write.close().await?;
    Ok(())
}

async fn run_exec_channel(
    channel: Channel<Msg>,
    mut child: Child,
    stdin: ChildStdin,
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    process_group: Pid,
    handle: russh::server::Handle,
) -> Result<(), SshServerError> {
    let channel_id = channel.id();
    let (mut channel_read, channel_write) = channel.split();
    let mut stdout_writer = channel_write.make_writer();
    let mut stderr_writer = channel_write.make_writer_ext(Some(1));
    let stdout_task = tokio::spawn(async move {
        tokio::io::copy(&mut stdout, &mut stdout_writer).await?;
        std::io::Result::Ok(())
    });
    let stderr_task = tokio::spawn(async move {
        tokio::io::copy(&mut stderr, &mut stderr_writer).await?;
        std::io::Result::Ok(())
    });
    let (disconnect_send, mut disconnect_receive) = tokio::sync::oneshot::channel();
    let input_task = tokio::spawn(async move {
        if copy_channel_input(&mut channel_read, stdin, process_group).await {
            let _ = disconnect_send.send(());
        }
    });
    let status = tokio::select! {
        status = child.wait() => status?,
        disconnected = &mut disconnect_receive => {
            if disconnected.is_ok() {
                terminate_process_group(process_group).await;
            }
            child.wait().await?
        }
    };
    input_task.abort();
    let _ = input_task.await;
    stdout_task.await.map_err(std::io::Error::other)??;
    stderr_task.await.map_err(std::io::Error::other)??;
    report_exit(&handle, channel_id, &channel_write, status).await?;
    channel_write.eof().await?;
    channel_write.close().await?;
    Ok(())
}

async fn copy_channel_input(
    channel: &mut russh::ChannelReadHalf,
    stdin: ChildStdin,
    process_group: Pid,
) -> bool {
    let mut stdin = Some(stdin);
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                let Some(input) = stdin.as_mut() else {
                    return false;
                };
                if input.write_all(&data).await.is_err() {
                    return false;
                }
            }
            ChannelMsg::Eof => {
                if let Some(mut input) = stdin.take() {
                    let _ = input.shutdown().await;
                }
            }
            ChannelMsg::Close => return true,
            ChannelMsg::Signal { signal } => {
                if let Some(signal) = client_signal(&signal) {
                    let _ = killpg(process_group, signal);
                }
            }
            _ => {}
        }
    }
    true
}

async fn report_exit(
    handle: &russh::server::Handle,
    channel: ChannelId,
    writer: &russh::ChannelWriteHalf<Msg>,
    status: std::process::ExitStatus,
) -> Result<(), SshServerError> {
    if let Some(code) = status.code() {
        writer.exit_status(code.cast_unsigned()).await?;
    } else {
        let signal = status
            .signal()
            .map_or(Sig::Custom("UNKNOWN".to_owned()), process_exit_signal);
        handle
            .exit_signal_request(channel, signal, false, String::new(), String::new())
            .await
            .map_err(|()| russh::Error::SendError)?;
    }
    Ok(())
}

fn client_signal(signal: &Sig) -> Option<Signal> {
    match signal {
        Sig::INT => Some(Signal::SIGINT),
        Sig::TERM => Some(Signal::SIGTERM),
        Sig::HUP => Some(Signal::SIGHUP),
        Sig::QUIT => Some(Signal::SIGQUIT),
        Sig::KILL => Some(Signal::SIGKILL),
        Sig::USR1 => Some(Signal::SIGUSR1),
        Sig::Custom(name) if name == "USR2" => Some(Signal::SIGUSR2),
        _ => None,
    }
}

fn process_exit_signal(signal: i32) -> Sig {
    match Signal::try_from(signal) {
        Ok(Signal::SIGABRT) => Sig::ABRT,
        Ok(Signal::SIGALRM) => Sig::ALRM,
        Ok(Signal::SIGFPE) => Sig::FPE,
        Ok(Signal::SIGHUP) => Sig::HUP,
        Ok(Signal::SIGILL) => Sig::ILL,
        Ok(Signal::SIGINT) => Sig::INT,
        Ok(Signal::SIGKILL) => Sig::KILL,
        Ok(Signal::SIGPIPE) => Sig::PIPE,
        Ok(Signal::SIGQUIT) => Sig::QUIT,
        Ok(Signal::SIGSEGV) => Sig::SEGV,
        Ok(Signal::SIGTERM) => Sig::TERM,
        Ok(Signal::SIGUSR1) => Sig::USR1,
        Ok(Signal::SIGUSR2) => Sig::Custom("USR2".to_owned()),
        Ok(other) => Sig::Custom(format!("SIG{}", other as i32)),
        Err(_) => Sig::Custom("UNKNOWN".to_owned()),
    }
}

async fn terminate_process_group(group: Pid) {
    let _ = killpg(group, Signal::SIGHUP);
    tokio::time::sleep(DISCONNECT_GRACE).await;
    let _ = killpg(group, Signal::SIGTERM);
    tokio::time::sleep(DISCONNECT_GRACE).await;
    let _ = killpg(group, Signal::SIGKILL);
}

fn resolve_shell(environment: &EnvironmentSnapshot) -> OsString {
    crate::identity()
        .ok()
        .map(|identity| identity.shell)
        .filter(|shell| !shell.is_empty())
        .map(OsString::from)
        .or_else(|| environment.value(OsStr::new("SHELL")).map(OsString::from))
        .filter(|shell| !shell.is_empty())
        .unwrap_or_else(|| OsString::from("/bin/sh"))
}

fn inspect_key_file(path: &Path, role: &'static str) -> Result<(), SshServerError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| SshServerError::UnsafeKeyFile {
        role,
        path: path.to_path_buf(),
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_file()
        || metadata.uid() != geteuid().as_raw()
        || metadata.permissions().mode() & 0o777 != 0o600
    {
        return Err(SshServerError::UnsafeKeyFile {
            role,
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};

    use russh::client;
    use russh::keys::PrivateKeyWithHashAlg;
    use russh::keys::ssh_key::private::Ed25519Keypair;
    use russh::{ChannelMsg, Disconnect};

    use super::*;

    #[derive(Clone)]
    struct ClientHandler {
        expected_host: PublicKey,
        observed_host: Arc<Mutex<Option<PublicKey>>>,
    }

    impl client::Handler for ClientHandler {
        type Error = russh::Error;

        async fn check_server_key(
            &mut self,
            server_public_key: &PublicKey,
        ) -> Result<bool, Self::Error> {
            *self.observed_host.lock().expect("host observation lock") =
                Some(server_public_key.clone());
            Ok(server_public_key == &self.expected_host)
        }
    }

    struct TestConnection {
        client: client::Handle<ClientHandler>,
        server: tokio::task::JoinHandle<Result<(), SshServerError>>,
        observed_host: Arc<Mutex<Option<PublicKey>>>,
    }

    impl TestConnection {
        async fn disconnect(self) {
            self.client
                .disconnect(Disconnect::ByApplication, "test complete", "")
                .await
                .expect("client disconnect");
            self.server
                .await
                .expect("server task")
                .expect("server connection");
        }
    }

    #[derive(Debug, Default)]
    struct ExecResult {
        stdout: Vec<u8>,
        stderr: Vec<u8>,
        status: Option<u32>,
        signal: Option<Sig>,
    }

    fn key(seed: u8) -> PrivateKey {
        PrivateKey::from(Ed25519Keypair::from_seed(&[seed; 32]))
    }

    fn test_config(workspace: &Path, host: &PrivateKey, client: &PrivateKey) -> SshServerConfig {
        let environment = EnvironmentSnapshot::from_entries(BTreeMap::from([
            (OsString::from("BASE"), OsString::from("captured")),
            (OsString::from("SHELL"), OsString::from("/must/not/be/used")),
            (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        ]));
        SshServerConfig::from_parts(
            host.clone(),
            client.public_key().clone(),
            environment,
            workspace.to_path_buf(),
            OsString::from("/bin/sh"),
        )
    }

    async fn connect(config: SshServerConfig, expected_host: PublicKey) -> TestConnection {
        let (client_stream, server_stream) = tokio::io::duplex(CHANNEL_WINDOW_BYTES as usize * 2);
        let server = tokio::spawn(serve_ssh_stream(server_stream, config));
        let observed_host = Arc::new(Mutex::new(None));
        let handler = ClientHandler {
            expected_host,
            observed_host: Arc::clone(&observed_host),
        };
        let client = client::connect_stream(
            Arc::new(client::Config {
                inactivity_timeout: None,
                ..Default::default()
            }),
            client_stream,
            handler,
        )
        .await
        .expect("in-memory SSH handshake");
        TestConnection {
            client,
            server,
            observed_host,
        }
    }

    async fn authenticate(
        connection: &mut TestConnection,
        user: &str,
        private_key: &PrivateKey,
    ) -> bool {
        connection
            .client
            .authenticate_publickey(
                user,
                PrivateKeyWithHashAlg::new(Arc::new(private_key.clone()), None),
            )
            .await
            .expect("public-key authentication response")
            .success()
    }

    async fn execute(
        connection: &TestConnection,
        command: &[u8],
        input: &[u8],
        environment: &[(&str, &str)],
    ) -> ExecResult {
        let mut channel = connection
            .client
            .channel_open_session()
            .await
            .expect("session channel");
        for (name, value) in environment {
            channel
                .set_env(true, *name, *value)
                .await
                .expect("environment request");
            assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        }
        channel
            .exec(true, command.to_vec())
            .await
            .expect("exec request");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel
            .data_bytes(input.to_vec())
            .await
            .expect("channel stdin");
        channel.eof().await.expect("channel EOF");
        let mut result = ExecResult::default();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => result.stdout.extend_from_slice(&data),
                ChannelMsg::ExtendedData { data, ext: 1 } => {
                    result.stderr.extend_from_slice(&data);
                }
                ChannelMsg::ExitStatus { exit_status } => result.status = Some(exit_status),
                ChannelMsg::ExitSignal { signal_name, .. } => result.signal = Some(signal_name),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        result
    }

    #[tokio::test]
    async fn in_memory_handshake_accepts_only_exact_user_and_client_key_and_uses_host_key() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(1);
        let allowed = key(2);
        let wrong = key(3);

        let mut wrong_user = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(!authenticate(&mut wrong_user, "other", &allowed).await);
        wrong_user.disconnect().await;

        let mut wrong_key = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(!authenticate(&mut wrong_key, SSH_USERNAME, &wrong).await);
        wrong_key.disconnect().await;

        let mut valid = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut valid, SSH_USERNAME, &allowed).await);
        assert_eq!(
            valid
                .observed_host
                .lock()
                .expect("host observation")
                .as_ref()
                .map(PublicKey::key_data),
            Some(host.public_key().key_data())
        );
        valid.disconnect().await;
    }

    #[tokio::test]
    async fn in_memory_handshake_rejects_none_and_password_methods() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(4);
        let allowed = key(5);
        let mut none = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        let none_result = none
            .client
            .authenticate_none(SSH_USERNAME)
            .await
            .expect("none authentication response");
        assert!(!none_result.success());
        none.disconnect().await;

        let mut password = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        let password_result = password
            .client
            .authenticate_password(SSH_USERNAME, "secret")
            .await
            .expect("password authentication response");
        assert!(!password_result.success());
        password.disconnect().await;

        let mut keyboard = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        let keyboard_result = keyboard
            .client
            .authenticate_keyboard_interactive_start(SSH_USERNAME, None::<String>)
            .await
            .expect("keyboard-interactive authentication response");
        assert!(matches!(
            keyboard_result,
            client::KeyboardInteractiveAuthResponse::Failure { .. }
        ));
        keyboard.disconnect().await;
    }

    #[tokio::test]
    async fn non_pty_exec_preserves_binary_streams_stderr_eof_and_nonzero_status() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(6);
        let allowed = key(7);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let input = b"binary\0stdin\xff";

        let result = execute(
            &connection,
            b"cat; printf 'separate-stderr' >&2; exit 23",
            input,
            &[],
        )
        .await;

        assert_eq!(result.stdout, input);
        assert_eq!(result.stderr, b"separate-stderr");
        assert_eq!(result.status, Some(23));
        connection.disconnect().await;
    }

    #[tokio::test]
    async fn exec_uses_exact_command_workspace_snapshot_allowlist_and_ssh_variables() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(8);
        let allowed = key(9);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let command = b"printf '%s\\n' \"$PWD\" \"$BASE\" \"$LANG\" \"${LD_PRELOAD-unset}\" \"$SSH_CONNECTION\" \"$SSH_CLIENT\" \"$SSH_ORIGINAL_COMMAND\"";
        let mut channel = connection
            .client
            .channel_open_session()
            .await
            .expect("session channel");
        channel
            .set_env(true, "LANG", "client-lang")
            .await
            .expect("allowed environment");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel
            .set_env(true, "LD_PRELOAD", "forbidden")
            .await
            .expect("rejected environment");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Failure)));
        channel.exec(true, command.to_vec()).await.expect("exec");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel.eof().await.expect("EOF");
        let mut stdout = Vec::new();
        let mut status = None;
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => stdout.extend_from_slice(&data),
                ChannelMsg::ExitStatus { exit_status } => status = Some(exit_status),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        let expected = format!(
            "{}\ncaptured\nclient-lang\nunset\n127.0.0.1 0 127.0.0.1 22\n127.0.0.1 0 22\n{}\n",
            temporary.path().display(),
            String::from_utf8_lossy(command)
        );

        assert_eq!(stdout, expected.as_bytes());
        assert_eq!(status, Some(0));
        connection.disconnect().await;
    }

    #[tokio::test]
    async fn invalid_shell_and_duplicate_exec_transitions_are_rejected() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(16);
        let allowed = key(17);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let mut channel = connection
            .client
            .channel_open_session()
            .await
            .expect("session channel");
        channel.request_shell(true).await.expect("shell request");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Failure)));
        channel
            .exec(true, b"cat".to_vec())
            .await
            .expect("first exec");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel
            .exec(true, b"printf duplicate".to_vec())
            .await
            .expect("duplicate exec request");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Failure)));
        channel.eof().await.expect("EOF");
        while !matches!(channel.wait().await, Some(ChannelMsg::Close) | None) {}

        connection.disconnect().await;
    }

    #[tokio::test]
    async fn pty_exec_exposes_tty_applies_modes_and_resizes() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(30);
        let allowed = key(31);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let mut channel = connection
            .client
            .channel_open_session()
            .await
            .expect("session channel");
        channel
            .request_pty(
                true,
                "xterm-256color",
                80,
                24,
                0,
                0,
                &[(russh::Pty::ECHO, 0)],
            )
            .await
            .expect("PTY request");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel
            .exec(
                true,
                b"printf '%s ' \"$SSH_TTY\"; sleep 0.05; stty size; stty -a | grep -o -- '-echo'"
                    .to_vec(),
            )
            .await
            .expect("exec request");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        channel
            .window_change(100, 40, 0, 0)
            .await
            .expect("window change");
        let mut output = Vec::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => output.extend_from_slice(&data),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        let output = String::from_utf8(output).expect("terminal output");
        assert!(output.contains("/dev/pts/"));
        assert!(output.contains("40 100"));
        assert!(output.contains("-echo"));
        connection.disconnect().await;
    }

    #[tokio::test]
    async fn process_signal_is_reported_as_ssh_exit_signal() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(10);
        let allowed = key(11);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);

        let result = execute(&connection, b"kill -TERM $$", b"", &[]).await;

        assert!(matches!(result.signal, Some(Sig::TERM)));
        assert_eq!(result.status, None);
        connection.disconnect().await;
    }

    #[tokio::test]
    async fn concurrent_non_pty_channels_are_isolated() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(12);
        let allowed = key(13);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);

        let (first, second) = tokio::join!(
            execute(&connection, b"cat", b"channel-one\0", &[("LANG", "one")]),
            execute(&connection, b"cat", b"channel-two\xff", &[("LANG", "two")]),
        );

        assert_eq!(first.stdout, b"channel-one\0");
        assert_eq!(second.stdout, b"channel-two\xff");
        connection.disconnect().await;
    }

    #[tokio::test]
    async fn direct_tcpip_forwards_binary_data_and_eof_over_concurrent_channels() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::net::TcpListener;

        let listener = TcpListener::bind(("127.0.0.1", 0)).await.expect("listener");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut stream, _) = listener.accept().await.expect("connection");
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    stream.read_to_end(&mut bytes).await.expect("input EOF");
                    stream.write_all(&bytes).await.expect("echo output");
                });
            }
        });
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(32);
        let allowed = key(33);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let (first, second) = tokio::join!(
            direct_tcpip(&connection, port, b"first\0channel"),
            direct_tcpip(&connection, port, b"second\xffchannel"),
        );
        assert_eq!(first, b"first\0channel");
        assert_eq!(second, b"second\xffchannel");
        server.await.expect("server");
        connection.disconnect().await;
    }

    async fn direct_tcpip(connection: &TestConnection, port: u16, input: &[u8]) -> Vec<u8> {
        let mut channel = connection
            .client
            .channel_open_direct_tcpip("127.0.0.1", u32::from(port), "127.0.0.1", 1234)
            .await
            .expect("direct channel");
        channel.data_bytes(input.to_vec()).await.expect("input");
        channel.eof().await.expect("input EOF");
        let mut output = Vec::new();
        while let Some(message) = channel.wait().await {
            match message {
                ChannelMsg::Data { data } => output.extend_from_slice(&data),
                ChannelMsg::Close => break,
                _ => {}
            }
        }
        output
    }

    #[tokio::test]
    async fn closing_channel_cancels_the_process_group() {
        let temporary = tempfile::tempdir().expect("temporary workspace");
        let host = key(14);
        let allowed = key(15);
        let mut connection = connect(
            test_config(temporary.path(), &host, &allowed),
            host.public_key().clone(),
        )
        .await;
        assert!(authenticate(&mut connection, SSH_USERNAME, &allowed).await);
        let mut channel = connection
            .client
            .channel_open_session()
            .await
            .expect("session channel");
        channel
            .exec(true, b"printf '%s' $$; exec sleep 30".to_vec())
            .await
            .expect("exec");
        assert!(matches!(channel.wait().await, Some(ChannelMsg::Success)));
        let pid = match channel.wait().await {
            Some(ChannelMsg::Data { data }) => std::str::from_utf8(&data)
                .expect("PID UTF-8")
                .parse::<i32>()
                .expect("numeric PID"),
            message => panic!("expected process PID, found {message:?}"),
        };
        channel.close().await.expect("close channel");
        tokio::time::timeout(Duration::from_secs(3), async {
            while nix::sys::signal::kill(Pid::from_raw(pid), None).is_ok() {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("channel process should be terminated");
        connection.disconnect().await;
    }

    #[cfg(unix)]
    #[test]
    fn server_config_loads_restricted_linux_assets() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary fixture");
        let host_path = temporary.path().join("host");
        let authorized_path = temporary.path().join("authorized");
        let host = key(20);
        let client = key(21);
        fs::write(
            &host_path,
            host.to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .expect("host encoding")
                .as_bytes(),
        )
        .expect("host key");
        fs::write(
            &authorized_path,
            client.public_key().to_openssh().expect("public encoding"),
        )
        .expect("authorized key");
        fs::set_permissions(&host_path, fs::Permissions::from_mode(0o600)).expect("host mode");
        fs::set_permissions(&authorized_path, fs::Permissions::from_mode(0o600))
            .expect("authorized mode");
        let capture = crate::capture_environment(&crate::EnvironmentCaptureRequest {
            generation: "ssh-test".to_owned(),
            state_directory: temporary.path().join("environment").display().to_string(),
            probe: crate::EnvironmentProbe::None,
            remote_environment: BTreeMap::new(),
        })
        .expect("environment capture");
        let request = SshServerRequest {
            host_key: host_path,
            authorized_key: authorized_path,
            environment: PathBuf::from(capture.snapshot_path),
            workspace: temporary.path().to_path_buf(),
        };

        let config = SshServerConfig::load(&request).expect("restricted SSH assets should load");

        assert!(format!("{config:?}").contains("[REDACTED]"));
    }

    #[cfg(unix)]
    #[test]
    fn server_config_rejects_key_files_with_wrong_modes_before_loading_secrets() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary fixture");
        let host_path = temporary.path().join("host");
        let authorized_path = temporary.path().join("authorized");
        let host = key(18);
        let client = key(19);
        fs::write(
            &host_path,
            host.to_openssh(russh::keys::ssh_key::LineEnding::LF)
                .expect("host encoding")
                .as_bytes(),
        )
        .expect("host key");
        fs::write(
            &authorized_path,
            client.public_key().to_openssh().expect("public encoding"),
        )
        .expect("authorized key");
        fs::set_permissions(&host_path, fs::Permissions::from_mode(0o644)).expect("host mode");
        fs::set_permissions(&authorized_path, fs::Permissions::from_mode(0o600))
            .expect("authorized mode");
        let request = SshServerRequest {
            host_key: host_path,
            authorized_key: authorized_path,
            environment: temporary.path().join("missing-environment"),
            workspace: temporary.path().to_path_buf(),
        };

        let error = SshServerConfig::load(&request).expect_err("loose host mode must fail");

        assert!(matches!(error, SshServerError::UnsafeKeyFile { .. }));
    }

    #[test]
    fn server_sources_never_use_stdout_printing() {
        let server_source = include_str!("ssh.rs")
            .rsplit_once("#[cfg(test)]\nmod tests")
            .map(|(production, _tests)| production)
            .expect("production SSH source");
        let main_source = include_str!("main.rs");
        let ssh_entry = main_source
            .split("async fn ssh_server")
            .nth(1)
            .and_then(|source| source.split("async fn forwarding_bridge").next())
            .expect("dedicated SSH entry point source");

        assert!(!server_source.lines().any(stdout_macro));
        assert!(!ssh_entry.lines().any(stdout_macro));
    }

    fn stdout_macro(line: &str) -> bool {
        let line = line.trim_start();
        line.starts_with("print!(") || line.starts_with("println!(")
    }

    #[test]
    fn server_errors_are_send_sync_and_static() {
        fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}
        assert_error::<SshServerError>();
    }
}
