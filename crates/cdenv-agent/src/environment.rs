//! Restricted, binary-safe effective environment capture and reuse.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use serde::{Deserialize, Serialize};
use thiserror::Error;

const SNAPSHOT_MAGIC: &[u8; 12] = b"CDENV-ENV\0\x01\0";
const MAXIMUM_ENTRIES: usize = 16 * 1024;
const MAXIMUM_ENTRY_BYTES: usize = 1024 * 1024;
const MAXIMUM_SNAPSHOT_BYTES: usize = 16 * 1024 * 1024;

/// A configured shell probe used to discover the selected user's environment.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum EnvironmentProbe {
    /// Do not invoke the account shell.
    None,
    /// Invoke the account shell as a login shell.
    LoginShell,
    /// Invoke the account shell as an interactive login shell.
    LoginInteractiveShell,
    /// Invoke the account shell as an interactive shell.
    InteractiveShell,
}

/// One runtime environment template segment supplied by the host planner.
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum EnvironmentTemplateSegment {
    /// UTF-8 configuration text already resolved at the host stage.
    Literal {
        /// Literal bytes.
        value: String,
    },
    /// A lookup in the actual active container environment.
    ContainerEnvironment {
        /// Environment name to inspect.
        name: String,
        /// Value used when the name is absent.
        default: String,
    },
}

/// One remote environment override. A null value removes the name.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct EnvironmentTemplate(pub Vec<EnvironmentTemplateSegment>);

/// Generation-scoped request consumed by `cdenv-agent capture-environment`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EnvironmentCaptureRequest {
    /// Stable generation identifier used in the snapshot filename.
    pub generation: String,
    /// Restricted selected-user state directory outside the checkout.
    pub state_directory: String,
    /// Configured user environment probe.
    pub probe: EnvironmentProbe,
    /// Effective remote environment overrides in deterministic key order.
    pub remote_environment: BTreeMap<String, Option<EnvironmentTemplate>>,
}

/// Safe machine result from environment capture. It contains no names or values.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentCaptureResult {
    /// Generation-scoped container-side snapshot path.
    pub snapshot_path: String,
    /// Number of reusable entries in the snapshot.
    pub entries: usize,
}

/// A decoded effective environment whose debug form never exposes entries.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvironmentSnapshot {
    entries: BTreeMap<OsString, OsString>,
}

impl std::fmt::Debug for EnvironmentSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EnvironmentSnapshot")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl EnvironmentSnapshot {
    /// Loads and validates a restricted binary snapshot without lossy conversion.
    ///
    /// # Errors
    ///
    /// Returns a typed path, permission, I/O, size, or encoding error.
    pub fn load(path: &Path) -> Result<Self, EnvironmentError> {
        platform::load(path)
    }

    /// Replaces a child's inherited environment with this effective snapshot.
    pub fn apply_to(&self, command: &mut Command) {
        command.env_clear();
        command.envs(&self.entries);
    }

    /// Returns the number of effective entries without exposing them.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Reports whether the snapshot is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Captures, probes, merges, filters, and atomically stores one effective environment.
///
/// The caller must execute the agent as the selected remote user. Values are never returned.
///
/// # Errors
///
/// Returns typed platform, request, probe, environment, path, permission, or snapshot errors.
pub fn capture_environment(
    request: &EnvironmentCaptureRequest,
) -> Result<EnvironmentCaptureResult, EnvironmentError> {
    platform::capture(request)
}

/// Runs a child with an exact loaded snapshot and inherited standard streams.
///
/// # Errors
///
/// Returns snapshot validation or child-spawn errors.
pub fn run_with_environment(
    snapshot: &Path,
    program: &OsStr,
    arguments: &[OsString],
) -> Result<ExitStatus, EnvironmentError> {
    let environment = EnvironmentSnapshot::load(snapshot)?;
    let mut command = Command::new(program);
    command.args(arguments);
    environment.apply_to(&mut command);
    command
        .status()
        .map_err(|source| EnvironmentError::ChildProcess { source })
}

/// Effective environment capture, encoding, or reuse failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum EnvironmentError {
    /// Environment capture is available only in the Linux agent.
    #[error("effective environment capture is supported on Linux only")]
    UnsupportedOperatingSystem,
    /// A generation identifier or state path was unsafe.
    #[error("invalid environment capture {field}")]
    InvalidRequest {
        /// Safe field name.
        field: &'static str,
    },
    /// A configured remote environment name was invalid.
    #[error("invalid remote environment name `{name}`")]
    InvalidEnvironmentName {
        /// Rejected configuration key; values are never retained.
        name: String,
    },
    /// The selected account's configured shell could not be probed.
    #[error("selected-user {probe:?} environment probe failed")]
    ProbeFailed {
        /// Probe mode, without captured output.
        probe: EnvironmentProbe,
    },
    /// Probe output exceeded the binary capture bound.
    #[error("selected-user environment probe exceeded its output bound")]
    ProbeOutputTooLarge,
    /// A snapshot exceeded a configured entry, value, or total bound.
    #[error("effective environment snapshot exceeded its {bound} bound")]
    SnapshotTooLarge {
        /// Safe bound category.
        bound: &'static str,
    },
    /// A snapshot did not match the binary framing contract.
    #[error("malformed effective environment snapshot: {reason}")]
    MalformedSnapshot {
        /// Safe parser reason that contains no entry bytes.
        reason: &'static str,
    },
    /// Restricted snapshot state could not be prepared.
    #[error("cannot prepare restricted effective environment state: {source}")]
    StateIo {
        /// Filesystem failure.
        #[source]
        source: std::io::Error,
    },
    /// A snapshot or its parent had unsafe ownership, type, or permissions.
    #[error("effective environment snapshot state is not restricted")]
    UnsafeState,
    /// A child using the effective environment could not be started.
    #[error("cannot start process with effective environment: {source}")]
    ChildProcess {
        /// Process-spawn failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(target_os = "linux")]
mod platform {
    use std::fs::{self, OpenOptions};
    use std::io::{Read, Write};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::process::CommandExt;
    use std::thread;

    use nix::unistd::geteuid;

    use super::{
        BTreeMap, Command, EnvironmentCaptureRequest, EnvironmentCaptureResult, EnvironmentError,
        EnvironmentProbe, EnvironmentSnapshot, EnvironmentTemplate, EnvironmentTemplateSegment,
        MAXIMUM_ENTRIES, MAXIMUM_ENTRY_BYTES, MAXIMUM_SNAPSHOT_BYTES, OsStr, OsString, Path,
        PathBuf, SNAPSHOT_MAGIC,
    };
    use crate::identity;

    pub(super) fn capture(
        request: &EnvironmentCaptureRequest,
    ) -> Result<EnvironmentCaptureResult, EnvironmentError> {
        validate_generation(&request.generation)?;
        let directory = prepare_state_directory(&request.state_directory)?;
        let snapshot_path =
            directory.join(format!(".cdenv-environment-{}.bin", request.generation));
        let container = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let mut effective = match request.probe {
            EnvironmentProbe::None => container.clone(),
            probe => probe_environment(probe)?,
        };
        merge_remote(&mut effective, &container, &request.remote_environment)?;
        filter_transient(&mut effective);
        let encoded = encode(&effective)?;
        atomic_replace(&snapshot_path, &encoded)?;
        Ok(EnvironmentCaptureResult {
            snapshot_path: snapshot_path.display().to_string(),
            entries: effective.len(),
        })
    }

    pub(super) fn load(path: &Path) -> Result<EnvironmentSnapshot, EnvironmentError> {
        validate_snapshot_path(path)?;
        let metadata =
            fs::symlink_metadata(path).map_err(|source| EnvironmentError::StateIo { source })?;
        if !metadata.is_file()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(EnvironmentError::UnsafeState);
        }
        let bytes = read_bounded(path)?;
        let mut entries = decode(&bytes)?;
        filter_transient(&mut entries);
        Ok(EnvironmentSnapshot { entries })
    }

    fn validate_generation(generation: &str) -> Result<(), EnvironmentError> {
        if generation.is_empty()
            || generation.len() > 128
            || !generation
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            Err(EnvironmentError::InvalidRequest {
                field: "generation",
            })
        } else {
            Ok(())
        }
    }

    fn prepare_state_directory(value: &str) -> Result<PathBuf, EnvironmentError> {
        let path = safe_absolute(value).ok_or(EnvironmentError::InvalidRequest {
            field: "stateDirectory",
        })?;
        reject_symlink_components(path)?;
        fs::create_dir_all(path).map_err(|source| EnvironmentError::StateIo { source })?;
        reject_symlink_components(path)?;
        let metadata =
            fs::symlink_metadata(path).map_err(|source| EnvironmentError::StateIo { source })?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
        {
            return Err(EnvironmentError::UnsafeState);
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|source| EnvironmentError::StateIo { source })?;
        Ok(path.to_path_buf())
    }

    fn validate_snapshot_path(path: &Path) -> Result<(), EnvironmentError> {
        if safe_absolute_os(path).is_none()
            || !path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| {
                    name.starts_with(".cdenv-environment-")
                        && Path::new(name).extension() == Some(OsStr::new("bin"))
                })
        {
            return Err(EnvironmentError::InvalidRequest {
                field: "snapshotPath",
            });
        }
        let parent = path.parent().ok_or(EnvironmentError::InvalidRequest {
            field: "snapshotPath",
        })?;
        reject_symlink_components(parent)?;
        let metadata =
            fs::symlink_metadata(parent).map_err(|source| EnvironmentError::StateIo { source })?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid() != geteuid().as_raw()
            || metadata.mode() & 0o077 != 0
        {
            return Err(EnvironmentError::UnsafeState);
        }
        Ok(())
    }

    fn reject_symlink_components(path: &Path) -> Result<(), EnvironmentError> {
        let mut current = PathBuf::from("/");
        for component in path.components().skip(1) {
            current.push(component);
            match fs::symlink_metadata(&current) {
                Ok(metadata) if metadata.file_type().is_symlink() => {
                    return Err(EnvironmentError::UnsafeState);
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
                Err(source) => return Err(EnvironmentError::StateIo { source }),
            }
        }
        Ok(())
    }

    fn safe_absolute(value: &str) -> Option<&Path> {
        safe_absolute_os(Path::new(value))
    }

    fn safe_absolute_os(path: &Path) -> Option<&Path> {
        use std::path::Component;
        (path.is_absolute()
            && path != Path::new("/")
            && !path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::CurDir | Component::Prefix(_)
                )
            }))
        .then_some(path)
    }

    fn probe_environment(
        probe: EnvironmentProbe,
    ) -> Result<BTreeMap<OsString, OsString>, EnvironmentError> {
        let account = identity().map_err(|_| EnvironmentError::ProbeFailed { probe })?;
        let executable =
            std::env::current_exe().map_err(|_| EnvironmentError::ProbeFailed { probe })?;
        let command = format!(
            "{} emit-environment",
            shell_quote(executable.as_os_str().as_bytes())?
        );
        let mut process = Command::new(&account.shell);
        process.arg0(&account.shell);
        match probe {
            EnvironmentProbe::None => {}
            EnvironmentProbe::LoginShell => {
                process.args(["-l", "-c", &command]);
            }
            EnvironmentProbe::LoginInteractiveShell => {
                process.args(["-l", "-i", "-c", &command]);
            }
            EnvironmentProbe::InteractiveShell => {
                process.args(["-i", "-c", &command]);
            }
        }
        process.stdin(std::process::Stdio::null());
        process.stdout(std::process::Stdio::piped());
        process.stderr(std::process::Stdio::piped());
        let mut child = process
            .spawn()
            .map_err(|_| EnvironmentError::ProbeFailed { probe })?;
        let stdout = child
            .stdout
            .take()
            .ok_or(EnvironmentError::ProbeFailed { probe })?;
        let stderr = child
            .stderr
            .take()
            .ok_or(EnvironmentError::ProbeFailed { probe })?;
        let stdout_reader = thread::spawn(move || read_stream_bounded(stdout));
        let stderr_reader = thread::spawn(move || read_stream_bounded(stderr));
        let status = child
            .wait()
            .map_err(|_| EnvironmentError::ProbeFailed { probe })?;
        let stdout = stdout_reader
            .join()
            .map_err(|_| EnvironmentError::ProbeFailed { probe })??;
        let _stderr = stderr_reader
            .join()
            .map_err(|_| EnvironmentError::ProbeFailed { probe })??;
        if !status.success() {
            return Err(EnvironmentError::ProbeFailed { probe });
        }
        decode_embedded(&stdout)
    }

    fn shell_quote(bytes: &[u8]) -> Result<String, EnvironmentError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| EnvironmentError::InvalidRequest { field: "agentPath" })?;
        Ok(format!("'{}'", text.replace('\'', "'\\''")))
    }

    fn read_stream_bounded(mut stream: impl Read) -> Result<Vec<u8>, EnvironmentError> {
        let mut bytes = Vec::new();
        stream
            .by_ref()
            .take((MAXIMUM_SNAPSHOT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| EnvironmentError::ProbeFailed {
                probe: EnvironmentProbe::None,
            })?;
        if bytes.len() > MAXIMUM_SNAPSHOT_BYTES {
            return Err(EnvironmentError::ProbeOutputTooLarge);
        }
        Ok(bytes)
    }

    fn merge_remote(
        effective: &mut BTreeMap<OsString, OsString>,
        container: &BTreeMap<OsString, OsString>,
        remote: &BTreeMap<String, Option<EnvironmentTemplate>>,
    ) -> Result<(), EnvironmentError> {
        for (name, template) in remote {
            validate_name(name)?;
            let key = OsString::from(name);
            if let Some(template) = template {
                let mut value = Vec::new();
                for segment in &template.0 {
                    match segment {
                        EnvironmentTemplateSegment::Literal { value: literal } => {
                            value.extend_from_slice(literal.as_bytes());
                        }
                        EnvironmentTemplateSegment::ContainerEnvironment { name, default } => {
                            validate_name(name)?;
                            value.extend_from_slice(
                                container
                                    .get(OsStr::new(name))
                                    .map_or(default.as_bytes(), |value| value.as_bytes()),
                            );
                        }
                    }
                    if value.len() > MAXIMUM_ENTRY_BYTES {
                        return Err(EnvironmentError::SnapshotTooLarge { bound: "entry" });
                    }
                }
                effective.insert(key, OsString::from_vec(value));
            } else {
                effective.remove(&key);
            }
        }
        Ok(())
    }

    fn validate_name(name: &str) -> Result<(), EnvironmentError> {
        if name.is_empty() || name.as_bytes().contains(&b'=') || name.as_bytes().contains(&0) {
            Err(EnvironmentError::InvalidEnvironmentName {
                name: name.to_owned(),
            })
        } else {
            Ok(())
        }
    }

    fn filter_transient(entries: &mut BTreeMap<OsString, OsString>) {
        entries.retain(|name, _| {
            let bytes = name.as_bytes();
            !matches!(bytes, b"PWD" | b"OLDPWD" | b"SHLVL" | b"_") && !bytes.starts_with(b"SSH_")
        });
    }

    pub(crate) fn encode(
        entries: &BTreeMap<OsString, OsString>,
    ) -> Result<Vec<u8>, EnvironmentError> {
        if entries.len() > MAXIMUM_ENTRIES {
            return Err(EnvironmentError::SnapshotTooLarge {
                bound: "entry count",
            });
        }
        let mut bytes = Vec::with_capacity(SNAPSHOT_MAGIC.len() + 4 + entries.len() * 16);
        bytes.extend_from_slice(SNAPSHOT_MAGIC);
        push_length(&mut bytes, entries.len())?;
        for (name, value) in entries {
            let name = name.as_bytes();
            let value = value.as_bytes();
            if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
                return Err(EnvironmentError::MalformedSnapshot {
                    reason: "invalid Unix environment entry",
                });
            }
            if name.len() > MAXIMUM_ENTRY_BYTES || value.len() > MAXIMUM_ENTRY_BYTES {
                return Err(EnvironmentError::SnapshotTooLarge { bound: "entry" });
            }
            push_length(&mut bytes, name.len())?;
            bytes.extend_from_slice(name);
            push_length(&mut bytes, value.len())?;
            bytes.extend_from_slice(value);
            if bytes.len() > MAXIMUM_SNAPSHOT_BYTES {
                return Err(EnvironmentError::SnapshotTooLarge {
                    bound: "total size",
                });
            }
        }
        Ok(bytes)
    }

    fn push_length(bytes: &mut Vec<u8>, length: usize) -> Result<(), EnvironmentError> {
        let length = u32::try_from(length)
            .map_err(|_| EnvironmentError::SnapshotTooLarge { bound: "length" })?;
        bytes.extend_from_slice(&length.to_be_bytes());
        Ok(())
    }

    pub(crate) fn decode(bytes: &[u8]) -> Result<BTreeMap<OsString, OsString>, EnvironmentError> {
        if bytes.len() > MAXIMUM_SNAPSHOT_BYTES {
            return Err(EnvironmentError::SnapshotTooLarge {
                bound: "total size",
            });
        }
        let mut decoder = Decoder { bytes, offset: 0 };
        if decoder.take(SNAPSHOT_MAGIC.len())? != SNAPSHOT_MAGIC {
            return Err(EnvironmentError::MalformedSnapshot {
                reason: "wrong magic or version",
            });
        }
        let count = decoder.length()?;
        if count > MAXIMUM_ENTRIES {
            return Err(EnvironmentError::SnapshotTooLarge {
                bound: "entry count",
            });
        }
        let mut entries = BTreeMap::new();
        for _ in 0..count {
            let name = decoder.field()?;
            let value = decoder.field()?;
            if name.is_empty() || name.contains(&b'=') || name.contains(&0) || value.contains(&0) {
                return Err(EnvironmentError::MalformedSnapshot {
                    reason: "invalid Unix environment entry",
                });
            }
            if entries
                .insert(
                    OsString::from_vec(name.to_vec()),
                    OsString::from_vec(value.to_vec()),
                )
                .is_some()
            {
                return Err(EnvironmentError::MalformedSnapshot {
                    reason: "duplicate environment name",
                });
            }
        }
        if decoder.offset != bytes.len() {
            return Err(EnvironmentError::MalformedSnapshot {
                reason: "trailing bytes",
            });
        }
        Ok(entries)
    }

    fn decode_embedded(bytes: &[u8]) -> Result<BTreeMap<OsString, OsString>, EnvironmentError> {
        for (index, window) in bytes.windows(SNAPSHOT_MAGIC.len()).enumerate() {
            if window == SNAPSHOT_MAGIC
                && let Ok(entries) = decode(&bytes[index..])
            {
                return Ok(entries);
            }
        }
        Err(EnvironmentError::MalformedSnapshot {
            reason: "probe did not emit a valid frame",
        })
    }

    struct Decoder<'a> {
        bytes: &'a [u8],
        offset: usize,
    }

    impl<'a> Decoder<'a> {
        fn length(&mut self) -> Result<usize, EnvironmentError> {
            let value = self.take(4)?;
            Ok(u32::from_be_bytes([value[0], value[1], value[2], value[3]]) as usize)
        }

        fn field(&mut self) -> Result<&'a [u8], EnvironmentError> {
            let length = self.length()?;
            if length > MAXIMUM_ENTRY_BYTES {
                return Err(EnvironmentError::SnapshotTooLarge { bound: "entry" });
            }
            self.take(length)
        }

        fn take(&mut self, length: usize) -> Result<&'a [u8], EnvironmentError> {
            let end =
                self.offset
                    .checked_add(length)
                    .ok_or(EnvironmentError::MalformedSnapshot {
                        reason: "length overflow",
                    })?;
            let value =
                self.bytes
                    .get(self.offset..end)
                    .ok_or(EnvironmentError::MalformedSnapshot {
                        reason: "truncated field",
                    })?;
            self.offset = end;
            Ok(value)
        }
    }

    fn atomic_replace(path: &Path, bytes: &[u8]) -> Result<(), EnvironmentError> {
        let parent = path.parent().ok_or(EnvironmentError::InvalidRequest {
            field: "snapshotPath",
        })?;
        let temporary = parent.join(format!(".cdenv-environment-write-{}", std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))?;
            fs::rename(&temporary, path)?;
            OpenOptions::new().read(true).open(parent)?.sync_all()
        })();
        if let Err(source) = result {
            let _ = fs::remove_file(&temporary);
            return Err(EnvironmentError::StateIo { source });
        }
        Ok(())
    }

    fn read_bounded(path: &Path) -> Result<Vec<u8>, EnvironmentError> {
        let file = OpenOptions::new()
            .read(true)
            .open(path)
            .map_err(|source| EnvironmentError::StateIo { source })?;
        let mut bytes = Vec::new();
        file.take((MAXIMUM_SNAPSHOT_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|source| EnvironmentError::StateIo { source })?;
        if bytes.len() > MAXIMUM_SNAPSHOT_BYTES {
            return Err(EnvironmentError::SnapshotTooLarge {
                bound: "total size",
            });
        }
        Ok(bytes)
    }

    pub(crate) fn emit_current() -> Result<(), EnvironmentError> {
        let entries = std::env::vars_os().collect::<BTreeMap<_, _>>();
        let bytes = encode(&entries)?;
        std::io::stdout()
            .lock()
            .write_all(&bytes)
            .map_err(|source| EnvironmentError::StateIo { source })
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn binary_codec_round_trips_non_utf8_names_and_values() {
            let entries = BTreeMap::from([(
                OsString::from_vec(vec![b'K', 0x80]),
                OsString::from_vec(vec![b'V', 0xff]),
            )]);
            let decoded = decode(&encode(&entries).expect("encode")).expect("decode");
            assert_eq!(decoded, entries);
        }

        #[test]
        fn filtering_removes_transient_and_ssh_session_entries() {
            let mut entries = BTreeMap::from([
                (OsString::from("PWD"), OsString::from("secret")),
                (OsString::from("SSH_AUTH_SOCK"), OsString::from("secret")),
                (OsString::from("KEEP"), OsString::from("value")),
            ]);
            filter_transient(&mut entries);
            assert_eq!(entries.len(), 1);
        }

        #[test]
        fn malformed_snapshot_rejects_duplicate_names() {
            let mut bytes = SNAPSHOT_MAGIC.to_vec();
            bytes.extend_from_slice(&2_u32.to_be_bytes());
            for _ in 0..2 {
                bytes.extend_from_slice(&1_u32.to_be_bytes());
                bytes.push(b'A');
                bytes.extend_from_slice(&1_u32.to_be_bytes());
                bytes.push(b'1');
            }
            assert!(matches!(
                decode(&bytes),
                Err(EnvironmentError::MalformedSnapshot {
                    reason: "duplicate environment name"
                })
            ));
        }

        #[test]
        fn malformed_snapshot_rejects_wrong_magic() {
            assert!(matches!(
                decode(b"not-an-environment"),
                Err(EnvironmentError::MalformedSnapshot { .. })
            ));
        }

        #[test]
        fn malformed_snapshot_rejects_truncated_fields() {
            let mut bytes = SNAPSHOT_MAGIC.to_vec();
            bytes.extend_from_slice(&1_u32.to_be_bytes());
            bytes.extend_from_slice(&8_u32.to_be_bytes());
            bytes.push(b'A');
            assert!(matches!(
                decode(&bytes),
                Err(EnvironmentError::MalformedSnapshot {
                    reason: "truncated field"
                })
            ));
        }

        #[test]
        fn malformed_snapshot_rejects_trailing_bytes() {
            let entries = BTreeMap::from([(OsString::from("A"), OsString::from("1"))]);
            let mut bytes = encode(&entries).expect("encode");
            bytes.push(0);
            assert!(matches!(
                decode(&bytes),
                Err(EnvironmentError::MalformedSnapshot {
                    reason: "trailing bytes"
                })
            ));
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::*;

    pub(super) fn capture(
        _: &EnvironmentCaptureRequest,
    ) -> Result<EnvironmentCaptureResult, EnvironmentError> {
        Err(EnvironmentError::UnsupportedOperatingSystem)
    }

    pub(super) fn load(_: &Path) -> Result<EnvironmentSnapshot, EnvironmentError> {
        Err(EnvironmentError::UnsupportedOperatingSystem)
    }
}

/// Emits the current process environment in the private binary framing used by shell probes.
///
/// This is an internal executable boundary and must not be used for diagnostics.
#[doc(hidden)]
pub fn emit_current_environment() -> Result<(), EnvironmentError> {
    #[cfg(target_os = "linux")]
    {
        platform::emit_current()
    }
    #[cfg(not(target_os = "linux"))]
    {
        Err(EnvironmentError::UnsupportedOperatingSystem)
    }
}
