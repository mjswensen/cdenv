//! Remembered consent for the optional user OpenSSH `Include` directive.

use std::fs::{self, OpenOptions};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::{
    CdenvRoot, Installation, InstallationError, SshConfigConsent, SshIncludeConsent,
    validate_openssh_path,
};

/// User interaction seam for SSH Include consent.
pub trait SshConsentInteraction {
    /// Whether standard input is a terminal eligible for prompting.
    fn stdin_is_terminal(&self) -> bool;
    /// Writes a prompt or manual instruction to the user-facing diagnostic stream.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the diagnostic stream cannot be written.
    fn write_message(&mut self, message: &str) -> io::Result<()>;
    /// Reads one response line.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the response stream cannot be read.
    fn read_response(&mut self) -> io::Result<String>;
}

/// Real terminal interaction using process stdin and stderr.
#[derive(Debug, Default)]
pub struct ProcessSshConsentInteraction;

impl SshConsentInteraction for ProcessSshConsentInteraction {
    fn stdin_is_terminal(&self) -> bool {
        io::stdin().is_terminal()
    }

    fn write_message(&mut self, message: &str) -> io::Result<()> {
        let mut stderr = io::stderr().lock();
        stderr.write_all(message.as_bytes())?;
        stderr.flush()
    }

    fn read_response(&mut self) -> io::Result<String> {
        let mut response = String::new();
        io::stdin().read_line(&mut response)?;
        Ok(response)
    }
}

/// Result of applying remembered or explicit Include consent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshIncludeOutcome {
    /// The exact Include was already present.
    AlreadyPresent,
    /// The Include was safely inserted.
    Inserted,
    /// Consent is unknown and no TTY or explicit acceptance was available.
    NeedsExplicitConsent,
    /// Remembered or explicit consent declined modification.
    Declined,
    /// Automatic editing was unsafe; exact manual instructions were shown.
    ManualInstructions,
}

/// SSH Include consent or user-config editing failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum SshConsentError {
    /// Persisting remembered consent failed.
    #[error(transparent)]
    Installation(#[from] InstallationError),
    /// The generated Include path cannot be represented safely.
    #[error("cannot represent the managed SSH Include path: {0}")]
    Path(#[from] crate::RequiredPathError),
    /// Terminal interaction failed.
    #[error("SSH Include consent interaction failed: {0}")]
    Interaction(#[source] io::Error),
    /// User SSH configuration could not be inspected or updated.
    #[error("cannot update user SSH configuration {path:?}: {source}")]
    Io {
        /// The user configuration path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: io::Error,
    },
}

/// Applies explicit or remembered Include consent to the user's SSH config.
///
/// Unknown consent prompts only when stdin is a TTY. Explicit decline never
/// removes an existing Include. Unsafe ownership, path kinds, and symlinks are
/// refused with exact manual instructions rather than followed or rewritten.
///
/// # Errors
///
/// Returns [`SshConsentError`] for consent persistence, interaction, path, or
/// filesystem failures that are not classified as unsafe manual-edit cases.
pub fn apply_ssh_include_consent(
    root: &CdenvRoot,
    installation: &mut Installation,
    explicit: Option<SshConfigConsent>,
    home: &Path,
    interaction: &mut impl SshConsentInteraction,
) -> Result<SshIncludeOutcome, SshConsentError> {
    let include_path = root.ssh().config();
    let include_text = validate_openssh_path(&include_path)?;
    let directive = format!("Include {}", ssh_token(include_text));
    let config = home.join(".ssh/config");

    let choice = match explicit {
        Some(SshConfigConsent::Accept) => {
            installation.set_ssh_include_consent(root, SshIncludeConsent::Accepted)?;
            SshIncludeConsent::Accepted
        }
        Some(SshConfigConsent::Decline) => {
            installation.set_ssh_include_consent(root, SshIncludeConsent::Declined)?;
            return Ok(SshIncludeOutcome::Declined);
        }
        None => installation.record().ssh_include_consent(),
    };

    match choice {
        SshIncludeConsent::Accepted => {}
        SshIncludeConsent::Declined => return Ok(SshIncludeOutcome::Declined),
        SshIncludeConsent::Unknown if !interaction.stdin_is_terminal() => {
            return Ok(SshIncludeOutcome::NeedsExplicitConsent);
        }
        SshIncludeConsent::Unknown => {
            let config_display = config.display();
            interaction
                .write_message(&format!(
                    "cdenv can add this global OpenSSH directive to {config_display}:\n  {directive}\nApply this change? [y/N] "
                ))
                .map_err(SshConsentError::Interaction)?;
            let response = interaction
                .read_response()
                .map_err(SshConsentError::Interaction)?;
            if !matches!(response.trim(), "y" | "Y" | "yes" | "YES" | "Yes") {
                installation.set_ssh_include_consent(root, SshIncludeConsent::Declined)?;
                return Ok(SshIncludeOutcome::Declined);
            }
            installation.set_ssh_include_consent(root, SshIncludeConsent::Accepted)?;
        }
    }

    match edit_user_config(&config, include_text, &directive) {
        Ok(outcome) => Ok(outcome),
        Err(UserConfigEditError::Unsafe) => {
            let config_display = config.display();
            interaction
                .write_message(&format!(
                    "cdenv did not modify the unsafe user SSH configuration at {config_display}.\nAdd this line manually before the first Host or Match block:\n  {directive}\n"
                ))
                .map_err(SshConsentError::Interaction)?;
            Ok(SshIncludeOutcome::ManualInstructions)
        }
        Err(UserConfigEditError::Io(source)) => Err(SshConsentError::Io {
            path: config,
            source,
        }),
    }
}

#[derive(Debug)]
enum UserConfigEditError {
    Unsafe,
    Io(io::Error),
}

fn edit_user_config(
    config: &Path,
    include_path: &str,
    directive: &str,
) -> Result<SshIncludeOutcome, UserConfigEditError> {
    let ssh_dir = config.parent().ok_or(UserConfigEditError::Unsafe)?;
    ensure_safe_ssh_directory(ssh_dir)?;
    let metadata = match fs::symlink_metadata(config) {
        Ok(metadata) => Some(metadata),
        Err(source) if source.kind() == io::ErrorKind::NotFound => None,
        Err(source) => return Err(UserConfigEditError::Io(source)),
    };
    if metadata
        .as_ref()
        .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
        || metadata
            .as_ref()
            .is_some_and(|metadata| !owned_by_current_user(metadata))
    {
        return Err(UserConfigEditError::Unsafe);
    }

    let existing = match metadata.as_ref() {
        Some(_) => fs::read(config).map_err(UserConfigEditError::Io)?,
        None => Vec::new(),
    };
    let text = std::str::from_utf8(&existing).map_err(|_| UserConfigEditError::Unsafe)?;
    if contains_include(text, include_path) {
        return Ok(SshIncludeOutcome::AlreadyPresent);
    }
    let replacement = insert_global_include(text, directive);
    let mode = metadata.as_ref().map_or(0o600, permission_mode);
    atomic_user_write(config, replacement.as_bytes(), mode)?;
    Ok(SshIncludeOutcome::Inserted)
}

fn ensure_safe_ssh_directory(path: &Path) -> Result<(), UserConfigEditError> {
    match fs::symlink_metadata(path) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                || !metadata.is_dir()
                || !owned_by_current_user(&metadata) =>
        {
            Err(UserConfigEditError::Unsafe)
        }
        Ok(_) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => {
            let parent = path.parent().ok_or(UserConfigEditError::Unsafe)?;
            let parent_metadata = fs::symlink_metadata(parent).map_err(UserConfigEditError::Io)?;
            if parent_metadata.file_type().is_symlink()
                || !parent_metadata.is_dir()
                || !owned_by_current_user(&parent_metadata)
            {
                return Err(UserConfigEditError::Unsafe);
            }
            let mut options = fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                options.mode(0o700);
            }
            options.create(path).map_err(UserConfigEditError::Io)
        }
        Err(source) => Err(UserConfigEditError::Io(source)),
    }
}

fn contains_include(text: &str, include_path: &str) -> bool {
    text.lines().any(|line| {
        let trimmed = line.trim();
        let Some((keyword, value)) = trimmed.split_once(char::is_whitespace) else {
            return false;
        };
        keyword.eq_ignore_ascii_case("include")
            && decode_ssh_token(value.trim()).as_deref() == Some(include_path)
    })
}

fn decode_ssh_token(value: &str) -> Option<String> {
    if !value.starts_with('"') {
        return (!value.chars().any(char::is_whitespace)).then(|| value.to_owned());
    }
    let inner = value.strip_prefix('"')?.strip_suffix('"')?;
    let mut decoded = String::with_capacity(inner.len());
    let mut characters = inner.chars();
    while let Some(character) = characters.next() {
        if character == '\\' {
            decoded.push(characters.next()?);
        } else {
            decoded.push(character);
        }
    }
    Some(decoded)
}

fn insert_global_include(text: &str, directive: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let insertion = text
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            let start = *offset;
            *offset += line.len();
            Some((start, line))
        })
        .find_map(|(offset, line)| is_host_or_match(line).then_some(offset))
        .unwrap_or(text.len());
    let (before, after) = text.split_at(insertion);
    let separator = if before.is_empty() || before.ends_with('\n') {
        ""
    } else {
        newline
    };
    format!("{before}{separator}{directive}{newline}{after}")
}

fn is_host_or_match(line: &str) -> bool {
    let keyword = line.split_whitespace().next().unwrap_or("");
    keyword.eq_ignore_ascii_case("host") || keyword.eq_ignore_ascii_case("match")
}

fn ssh_token(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

fn atomic_user_write(path: &Path, bytes: &[u8], mode: u32) -> Result<(), UserConfigEditError> {
    let parent = path.parent().ok_or(UserConfigEditError::Unsafe)?;
    let filename = path.file_name().ok_or(UserConfigEditError::Unsafe)?;
    let mut random = [0_u8; 16];
    getrandom::fill(&mut random)
        .map_err(|source| UserConfigEditError::Io(io::Error::other(source)))?;
    let temporary = parent.join(format!(
        ".{}.cdenv-tmp-{}",
        filename.to_string_lossy(),
        hex::encode(random)
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(mode);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(mode))?;
        }
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(UserConfigEditError::Io)
}

#[cfg(unix)]
fn owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == nix::unistd::geteuid().as_raw()
}

#[cfg(not(unix))]
const fn owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn permission_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
const fn permission_mode(_metadata: &fs::Metadata) -> u32 {
    0o600
}
