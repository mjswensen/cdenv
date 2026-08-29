//! Linux PTY allocation and terminal setup behind safe wrappers.
//!
//! `pty-process` owns the necessary `setsid`, controlling-terminal, and ioctl
//! operations. This module deliberately exposes no file descriptors or unsafe API.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;

use nix::sys::termios::{
    ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg, SpecialCharacterIndices, tcgetattr,
    tcsetattr,
};
use nix::unistd::ttyname;
use pty_process::{Command, Pty, Size, open};
use russh::Pty as SshPty;
use tokio::process::Child;

/// Requested terminal parameters captured before a channel starts.
#[derive(Clone, Debug)]
pub(super) struct PtyRequest {
    pub(super) term: String,
    pub(super) columns: u32,
    pub(super) rows: u32,
    pub(super) pixels_width: u32,
    pub(super) pixels_height: u32,
    pub(super) modes: Vec<(SshPty, u32)>,
}

/// A running PTY and the associated child.
pub(super) struct PtyProcess {
    pub(super) pty: Pty,
    pub(super) child: Child,
}

/// Starts a command in a new session with the PTY as its controlling terminal.
#[expect(
    clippy::too_many_arguments,
    reason = "PTY spawn inputs are independently security-sensitive"
)]
pub(super) fn spawn(
    shell: &OsStr,
    arguments: &[OsString],
    login_argv0: Option<OsString>,
    environment: &BTreeMap<OsString, OsString>,
    client_environment: &BTreeMap<String, String>,
    original_command: Option<&OsStr>,
    workspace: &Path,
    request: &PtyRequest,
) -> std::io::Result<PtyProcess> {
    let (pty, pts) = open().map_err(std::io::Error::other)?;
    pty.resize(size(request)).map_err(std::io::Error::other)?;
    apply_modes(&pts, &request.modes)?;
    let tty = ttyname(&pts).map_err(std::io::Error::other)?;
    let mut command = Command::new(shell)
        .args(arguments)
        .current_dir(workspace)
        .env_clear()
        .envs(environment)
        .envs(client_environment)
        .env("TERM", &request.term)
        .env("SSH_TTY", &tty)
        .env("SSH_CONNECTION", "127.0.0.1 0 127.0.0.1 22")
        .env("SSH_CLIENT", "127.0.0.1 0 22")
        .kill_on_drop(true);
    if let Some(command_string) = original_command {
        command = command.env("SSH_ORIGINAL_COMMAND", command_string);
    }
    if let Some(argv0) = login_argv0 {
        command = command.arg0(argv0);
    }
    let child = command.spawn(pts).map_err(std::io::Error::other)?;
    Ok(PtyProcess { pty, child })
}

/// Converts an SSH window size to the kernel's bounded terminal representation.
pub(super) fn size(request: &PtyRequest) -> Size {
    Size::new_with_pixel(
        u16::try_from(request.rows).map_or(u16::MAX, std::convert::identity),
        u16::try_from(request.columns).map_or(u16::MAX, std::convert::identity),
        u16::try_from(request.pixels_width).map_or(u16::MAX, std::convert::identity),
        u16::try_from(request.pixels_height).map_or(u16::MAX, std::convert::identity),
    )
}

macro_rules! set_flag {
    ($flags:expr, $flag:expr, $enabled:expr) => {
        if $enabled {
            $flags.insert($flag);
        } else {
            $flags.remove($flag);
        }
    };
}

fn apply_modes(pts: &pty_process::Pts, modes: &[(SshPty, u32)]) -> std::io::Result<()> {
    let mut attributes = tcgetattr(pts).map_err(std::io::Error::other)?;
    for (mode, value) in modes {
        let enabled = *value != 0;
        match mode {
            SshPty::VINTR => set_control(&mut attributes, SpecialCharacterIndices::VINTR, *value),
            SshPty::VQUIT => set_control(&mut attributes, SpecialCharacterIndices::VQUIT, *value),
            SshPty::VERASE => set_control(&mut attributes, SpecialCharacterIndices::VERASE, *value),
            SshPty::VKILL => set_control(&mut attributes, SpecialCharacterIndices::VKILL, *value),
            SshPty::VEOF => set_control(&mut attributes, SpecialCharacterIndices::VEOF, *value),
            SshPty::ISIG => set_flag!(&mut attributes.local_flags, LocalFlags::ISIG, enabled),
            SshPty::ICANON => set_flag!(&mut attributes.local_flags, LocalFlags::ICANON, enabled),
            SshPty::ECHO => set_flag!(&mut attributes.local_flags, LocalFlags::ECHO, enabled),
            SshPty::ECHOE => set_flag!(&mut attributes.local_flags, LocalFlags::ECHOE, enabled),
            SshPty::ECHOK => set_flag!(&mut attributes.local_flags, LocalFlags::ECHOK, enabled),
            SshPty::IEXTEN => set_flag!(&mut attributes.local_flags, LocalFlags::IEXTEN, enabled),
            SshPty::ICRNL => set_flag!(&mut attributes.input_flags, InputFlags::ICRNL, enabled),
            SshPty::IXON => set_flag!(&mut attributes.input_flags, InputFlags::IXON, enabled),
            SshPty::OPOST => set_flag!(&mut attributes.output_flags, OutputFlags::OPOST, enabled),
            SshPty::ONLCR => set_flag!(&mut attributes.output_flags, OutputFlags::ONLCR, enabled),
            SshPty::CS7 => set_flag!(&mut attributes.control_flags, ControlFlags::CS7, enabled),
            SshPty::CS8 => set_flag!(&mut attributes.control_flags, ControlFlags::CS8, enabled),
            _ => {}
        }
    }
    tcsetattr(pts, SetArg::TCSANOW, &attributes).map_err(std::io::Error::other)
}

fn set_control(
    attributes: &mut nix::sys::termios::Termios,
    index: SpecialCharacterIndices,
    value: u32,
) {
    attributes.control_chars[index as usize] =
        u8::try_from(value).map_or(u8::MAX, std::convert::identity);
}
