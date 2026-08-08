use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use nix::sys::signal::{Signal, killpg};
use nix::unistd::Pid;
use russh::keys::ssh_key::PublicKey;
use russh::server::{Auth, Msg, Session};
use russh::{Channel, ChannelId, ChannelMsg, ChannelOpenFailure, MethodKind, Pty, Sig};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, ChildStderr, ChildStdin, ChildStdout};
use tokio::sync::Mutex;

#[derive(Debug, Error)]
enum AgentError {
    #[error("invalid agent invocation: {0}")]
    Invocation(String),
    #[error("I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("SSH protocol failed: {0}")]
    Russh(#[from] russh::Error),
    #[error("SSH key failed: {0}")]
    Key(#[from] russh::keys::Error),
    #[error("PTY failed: {0}")]
    Pty(#[from] pty_process::Error),
    #[error("process signal failed: {0}")]
    Signal(#[from] nix::errno::Errno),
    #[error("channel {0} is not pending")]
    MissingChannel(ChannelId),
    #[error("channel {0} already started a process")]
    DuplicateProcess(ChannelId),
    #[error("direct TCP target is invalid: {0}")]
    ForwardTarget(String),
    #[error("lifecycle state `{0}` is indeterminate and requires rebuild")]
    Indeterminate(String),
}

type Result<T> = std::result::Result<T, AgentError>;

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("cdenv feasibility agent: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut arguments = std::env::args_os();
    let _program = arguments.next();
    let command = arguments
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(|| AgentError::Invocation("missing subcommand".to_string()))?;
    let rest = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "ssh-server" => {
            if rest.len() != 3 {
                return Err(AgentError::Invocation(
                    "ssh-server <host-private-key> <allowed-public-key> <workspace>".to_string(),
                ));
            }
            serve_ssh(
                Path::new(&rest[0]),
                Path::new(&rest[1]),
                PathBuf::from(&rest[2]),
            )
            .await
        }
        "tcp-bridge" => {
            if rest.len() != 2 {
                return Err(AgentError::Invocation(
                    "tcp-bridge <host> <port>".to_string(),
                ));
            }
            let host = rest[0]
                .to_str()
                .ok_or_else(|| AgentError::ForwardTarget("host is not UTF-8".to_string()))?;
            let port = rest[1]
                .to_str()
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|port| *port != 0)
                .ok_or_else(|| AgentError::ForwardTarget("port must be 1..=65535".to_string()))?;
            bridge_tcp(host, port).await
        }
        "http-server" => {
            if rest.len() != 1 {
                return Err(AgentError::Invocation("http-server <port>".to_string()));
            }
            let port = rest[0]
                .to_str()
                .and_then(|value| value.parse::<u16>().ok())
                .filter(|port| *port != 0)
                .ok_or_else(|| AgentError::ForwardTarget("port must be 1..=65535".to_string()))?;
            serve_http(port).await
        }
        "lifecycle-runner" => {
            if rest.len() != 3 {
                return Err(AgentError::Invocation(
                    "lifecycle-runner <state> <log> <seconds>".to_string(),
                ));
            }
            let seconds = rest[2]
                .to_str()
                .and_then(|value| value.parse::<u64>().ok())
                .ok_or_else(|| AgentError::Invocation("seconds must be an integer".to_string()))?;
            lifecycle_runner(Path::new(&rest[0]), Path::new(&rest[1]), seconds).await
        }
        other => Err(AgentError::Invocation(format!(
            "unknown subcommand `{other}`"
        ))),
    }
}

async fn serve_ssh(host_key: &Path, allowed_key: &Path, workspace: PathBuf) -> Result<()> {
    let host_key = russh::keys::load_secret_key(host_key, None)?;
    let allowed_key = russh::keys::load_public_key(allowed_key)?;
    let methods = russh::MethodSet::from(&[MethodKind::PublicKey][..]);
    let config = Arc::new(russh::server::Config {
        methods,
        auth_rejection_time: Duration::from_millis(100),
        auth_rejection_time_initial: Some(Duration::ZERO),
        keys: vec![host_key],
        inactivity_timeout: None,
        max_auth_attempts: 1,
        keepalive_interval: Some(Duration::from_secs(15)),
        keepalive_max: 3,
        channel_buffer_size: 64,
        event_buffer_size: 64,
        ..Default::default()
    });
    let handler = SshHandler {
        allowed_key,
        workspace,
        pending: HashMap::new(),
    };
    let stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    let running = russh::server::run_stream(config, stdio, handler).await?;
    running.await
}

struct PendingChannel {
    channel: Channel<Msg>,
    pty: Option<PtyRequest>,
    environment: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
struct PtyRequest {
    term: String,
    columns: u16,
    rows: u16,
    pixel_width: u16,
    pixel_height: u16,
}

struct SshHandler {
    allowed_key: PublicKey,
    workspace: PathBuf,
    pending: HashMap<ChannelId, PendingChannel>,
}

impl russh::server::Handler for SshHandler {
    type Error = AgentError;

    async fn auth_publickey(&mut self, user: &str, public_key: &PublicKey) -> Result<Auth> {
        if user == "cdenv" && public_key == &self.allowed_key {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn auth_publickey_offered(&mut self, user: &str, public_key: &PublicKey) -> Result<Auth> {
        if user == "cdenv" && public_key == &self.allowed_key {
            Ok(Auth::Accept)
        } else {
            Ok(Auth::reject())
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: russh::server::ChannelOpenHandle,
        _session: &mut Session,
    ) -> Result<()> {
        let id = channel.id();
        if self
            .pending
            .insert(
                id,
                PendingChannel {
                    channel,
                    pty: None,
                    environment: BTreeMap::new(),
                },
            )
            .is_some()
        {
            reply.reject(ChannelOpenFailure::ResourceShortage).await;
            return Err(AgentError::DuplicateProcess(id));
        }
        reply.accept().await;
        Ok(())
    }

    async fn env_request(
        &mut self,
        channel: ChannelId,
        variable_name: &str,
        variable_value: &str,
        session: &mut Session,
    ) -> Result<()> {
        let allowed = variable_name == "LANG"
            || variable_name.starts_with("LC_")
            || matches!(variable_name, "TERM" | "COLORTERM" | "NO_COLOR");
        if allowed {
            let pending = self
                .pending
                .get_mut(&channel)
                .ok_or(AgentError::MissingChannel(channel))?;
            pending
                .environment
                .insert(variable_name.to_string(), variable_value.to_string());
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
        _modes: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<()> {
        let pending = self
            .pending
            .get_mut(&channel)
            .ok_or(AgentError::MissingChannel(channel))?;
        pending.pty = Some(PtyRequest {
            term: term.to_string(),
            columns: bounded_dimension(col_width),
            rows: bounded_dimension(row_height),
            pixel_width: bounded_dimension(pix_width),
            pixel_height: bounded_dimension(pix_height),
        });
        session.channel_success(channel)?;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        channel: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<()> {
        let pending = self
            .pending
            .remove(&channel)
            .ok_or(AgentError::MissingChannel(channel))?;
        if pending.pty.is_some() {
            spawn_pty_process(
                pending,
                Some(OsString::from_vec(command.to_vec())),
                self.workspace.clone(),
            )?;
        } else {
            spawn_exec_process(
                pending,
                OsString::from_vec(command.to_vec()),
                self.workspace.clone(),
            )?;
        }
        session.channel_success(channel)?;
        Ok(())
    }

    async fn shell_request(&mut self, channel: ChannelId, session: &mut Session) -> Result<()> {
        let pending = self
            .pending
            .remove(&channel)
            .ok_or(AgentError::MissingChannel(channel))?;
        if pending.pty.is_none() {
            session.channel_failure(channel)?;
            return Ok(());
        }
        spawn_pty_process(pending, None, self.workspace.clone())?;
        session.channel_success(channel)?;
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
    ) -> Result<()> {
        let Ok(port) = u16::try_from(port_to_connect) else {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        };
        if port == 0 || host_to_connect.is_empty() || host_to_connect.contains('\0') {
            reply.reject(ChannelOpenFailure::ConnectFailed).await;
            return Ok(());
        }
        match tokio::net::TcpStream::connect((host_to_connect, port)).await {
            Ok(mut target) => {
                reply.accept().await;
                tokio::spawn(async move {
                    let mut channel = channel.into_stream();
                    if let Err(error) =
                        tokio::io::copy_bidirectional(&mut channel, &mut target).await
                    {
                        eprintln!("direct-tcpip bridge failed: {error}");
                    }
                });
            }
            Err(error) => {
                eprintln!("direct-tcpip target unavailable: {error}");
                reply.reject(ChannelOpenFailure::ConnectFailed).await;
            }
        }
        Ok(())
    }
}

fn bounded_dimension(value: u32) -> u16 {
    u16::try_from(value).unwrap_or(u16::MAX)
}

fn spawn_exec_process(
    pending: PendingChannel,
    command: OsString,
    workspace: PathBuf,
) -> Result<()> {
    let mut process = tokio::process::Command::new("/usr/bin/setsid");
    process
        .arg("/bin/sh")
        .arg("-c")
        .arg(command)
        .current_dir(workspace)
        .envs(pending.environment)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let mut child = process.spawn()?;
    let process_group = child
        .id()
        .map(|id| Pid::from_raw(id.cast_signed()))
        .ok_or_else(|| AgentError::Invocation("spawned command has no process id".to_string()))?;
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| AgentError::Invocation("spawned command has no stdin pipe".to_string()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| AgentError::Invocation("spawned command has no stdout pipe".to_string()))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| AgentError::Invocation("spawned command has no stderr pipe".to_string()))?;
    tokio::spawn(async move {
        if let Err(error) =
            run_exec_channel(pending.channel, child, stdin, stdout, stderr, process_group).await
        {
            eprintln!("exec channel failed: {error}");
        }
    });
    Ok(())
}

async fn run_exec_channel(
    channel: Channel<Msg>,
    mut child: Child,
    stdin: ChildStdin,
    mut stdout: ChildStdout,
    mut stderr: ChildStderr,
    process_group: Pid,
) -> Result<()> {
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
    tokio::spawn(async move {
        let disconnected = copy_channel_input(&mut channel_read, stdin, process_group).await;
        if disconnected {
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
    stdout_task
        .await
        .map_err(|error| AgentError::Io(std::io::Error::other(error)))??;
    stderr_task
        .await
        .map_err(|error| AgentError::Io(std::io::Error::other(error)))??;
    let code = exit_code(status);
    channel_write.exit_status(code).await?;
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
                    return true;
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
                if let Some(signal) = map_signal(&signal) {
                    let _ = killpg(process_group, signal);
                }
            }
            _ => {}
        }
    }
    true
}

fn spawn_pty_process(
    pending: PendingChannel,
    command: Option<OsString>,
    workspace: PathBuf,
) -> Result<()> {
    let request = pending.pty.clone().ok_or_else(|| {
        AgentError::Invocation("PTY process requested without PTY metadata".to_string())
    })?;
    let (pty, pts) = pty_process::open()?;
    pty.resize(pty_process::Size::new(request.rows, request.columns))?;
    let mut process = pty_process::Command::new("/bin/sh")
        .current_dir(workspace)
        .envs(pending.environment)
        .env("TERM", request.term.clone())
        .env("SSH_TTY", "/dev/pts/cdenv-spike")
        .kill_on_drop(true);
    process = if let Some(command) = command {
        process.arg("-c").arg(command)
    } else {
        process.arg("-l")
    };
    let child = process.spawn(pts)?;
    let process_group = child
        .id()
        .map(|id| Pid::from_raw(id.cast_signed()))
        .ok_or_else(|| AgentError::Invocation("PTY child has no process id".to_string()))?;
    tokio::spawn(async move {
        if let Err(error) =
            run_pty_channel(pending.channel, child, pty, process_group, request).await
        {
            eprintln!("PTY channel failed: {error}");
        }
    });
    Ok(())
}

async fn run_pty_channel(
    channel: Channel<Msg>,
    mut child: Child,
    pty: pty_process::Pty,
    process_group: Pid,
    initial: PtyRequest,
) -> Result<()> {
    let (mut channel_read, channel_write) = channel.split();
    let (mut pty_read, pty_write) = pty.into_split();
    let pty_write = Arc::new(Mutex::new(pty_write));
    let mut channel_stdout = channel_write.make_writer();
    let output_task = tokio::spawn(async move {
        match tokio::io::copy(&mut pty_read, &mut channel_stdout).await {
            Ok(_) => Ok(()),
            Err(error) if error.raw_os_error() == Some(5) => Ok(()),
            Err(error) => Err(error),
        }
    });
    let (disconnect_send, mut disconnect_receive) = tokio::sync::oneshot::channel();
    let input_write = Arc::clone(&pty_write);
    tokio::spawn(async move {
        let disconnected =
            handle_pty_input(&mut channel_read, input_write, process_group, initial).await;
        if disconnected {
            let _ = disconnect_send.send(());
        }
    });
    let status = tokio::select! {
        status = child.wait() => status?,
        _ = &mut disconnect_receive => {
            terminate_process_group(process_group).await;
            child.wait().await?
        }
    };
    output_task
        .await
        .map_err(|error| AgentError::Io(std::io::Error::other(error)))??;
    channel_write.exit_status(exit_code(status)).await?;
    channel_write.eof().await?;
    channel_write.close().await?;
    Ok(())
}

async fn handle_pty_input(
    channel: &mut russh::ChannelReadHalf,
    pty: Arc<Mutex<pty_process::OwnedWritePty>>,
    process_group: Pid,
    mut size: PtyRequest,
) -> bool {
    while let Some(message) = channel.wait().await {
        match message {
            ChannelMsg::Data { data } => {
                if pty.lock().await.write_all(&data).await.is_err() {
                    return false;
                }
            }
            ChannelMsg::WindowChange {
                col_width,
                row_height,
                pix_width,
                pix_height,
            } => {
                size.columns = bounded_dimension(col_width);
                size.rows = bounded_dimension(row_height);
                size.pixel_width = bounded_dimension(pix_width);
                size.pixel_height = bounded_dimension(pix_height);
                let dimensions = pty_process::Size::new_with_pixel(
                    size.rows,
                    size.columns,
                    size.pixel_width,
                    size.pixel_height,
                );
                let _ = pty.lock().await.resize(dimensions);
            }
            ChannelMsg::Signal { signal } => {
                if let Some(signal) = map_signal(&signal) {
                    let _ = killpg(process_group, signal);
                }
            }
            ChannelMsg::Eof => {
                let _ = pty.lock().await.shutdown().await;
            }
            ChannelMsg::Close => return true,
            _ => {}
        }
    }
    true
}

fn map_signal(signal: &Sig) -> Option<Signal> {
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

async fn terminate_process_group(process_group: Pid) {
    let _ = killpg(process_group, Signal::SIGHUP);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = killpg(process_group, Signal::SIGTERM);
    tokio::time::sleep(Duration::from_millis(100)).await;
    let _ = killpg(process_group, Signal::SIGKILL);
}

fn exit_code(status: std::process::ExitStatus) -> u32 {
    status.code().map_or_else(
        || 128 + status.signal().unwrap_or(0).cast_unsigned(),
        i32::cast_unsigned,
    )
}

async fn bridge_tcp(host: &str, port: u16) -> Result<()> {
    let mut target = tokio::net::TcpStream::connect((host, port)).await?;
    let mut stdio = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
    tokio::io::copy_bidirectional(&mut stdio, &mut target).await?;
    Ok(())
}

async fn serve_http(port: u16) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    loop {
        let (mut stream, _) = listener.accept().await?;
        tokio::spawn(async move {
            let mut request = [0_u8; 4096];
            let _ = stream.read(&mut request).await;
            let body = b"cdenv-direct-tcpip-ok\n";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.write_all(body).await;
            let _ = stream.shutdown().await;
        });
    }
}

async fn lifecycle_runner(state: &Path, log: &Path, seconds: u64) -> Result<()> {
    let existing = std::fs::read_to_string(state).unwrap_or_else(|_| "pending\n".to_string());
    match existing.trim() {
        "succeeded" => return Ok(()),
        "running" => return Err(AgentError::Indeterminate(state.display().to_string())),
        "pending" => {}
        other => {
            return Err(AgentError::Invocation(format!(
                "unknown lifecycle state `{other}`"
            )));
        }
    }
    atomic_state(state, "running\n")?;
    append_line(log, "postCreate:start\n")?;
    tokio::time::sleep(Duration::from_secs(seconds)).await;
    append_line(log, "postCreate:succeeded\n")?;
    atomic_state(state, "succeeded\n")
}

fn atomic_state(path: &Path, value: &str) -> Result<()> {
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, value)?;
    std::fs::rename(temporary, path)?;
    Ok(())
}

fn append_line(path: &Path, value: &str) -> Result<()> {
    use std::io::Write;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    file.write_all(value.as_bytes())?;
    file.sync_data()?;
    Ok(())
}
