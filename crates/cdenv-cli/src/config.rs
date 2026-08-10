//! Host filesystem boundary for pure Dev Container discovery and parsing.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use cdenv_devcontainer::{
    ConfigInventory, ConfigPath, ConfigPathError, DiscoveryError, MAX_CONFIG_BYTES, discover_config,
};
use thiserror::Error;

use crate::{ConfigContainmentError, validate_explicit_config};

/// Bounded configuration bytes and their canonically contained relative path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigSource {
    path: ConfigPath,
    bytes: Vec<u8>,
}

impl ConfigSource {
    /// Returns the validated path passed to the pure profile crate.
    #[must_use]
    pub const fn path(&self) -> &ConfigPath {
        &self.path
    }

    /// Returns at most one mebibyte plus no sentinel byte.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

/// A host-side configuration inventory or bounded-read failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ConfigLoadError {
    /// A repository-relative path failed pure validation.
    #[error(transparent)]
    Path(#[from] ConfigPathError),
    /// Pure discovery failed.
    #[error(transparent)]
    Discovery(#[from] DiscoveryError),
    /// Selected-file canonical containment failed.
    #[error(transparent)]
    Containment(#[from] ConfigContainmentError),
    /// The candidate inventory could not be inspected.
    #[error("cannot inventory Dev Container configurations below {path:?}: {source}")]
    Inventory {
        /// Inspected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The selected file could not be opened or read.
    #[error("cannot read Dev Container configuration {path:?}: {source}")]
    Read {
        /// Selected path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// The host read observed more than the profile's maximum source bytes.
    #[error("Dev Container configuration {path:?} exceeds {limit} bytes")]
    TooLarge {
        /// Selected path.
        path: PathBuf,
        /// Maximum accepted bytes.
        limit: usize,
    },
}

/// Discovers and reads one checkout-contained regular configuration file.
///
/// Filesystem access and canonical containment end at this boundary. The
/// returned relative path and bounded bytes are suitable for
/// `cdenv_devcontainer::parse_jsonc`.
///
/// # Errors
///
/// Returns [`ConfigLoadError`] for inventory I/O, ambiguity, unsafe explicit or
/// discovered paths, and read/size failures.
pub fn discover_and_read_config(
    checkout: &Path,
    explicit: Option<&Path>,
) -> Result<Option<ConfigSource>, ConfigLoadError> {
    let explicit_path = explicit.map(path_to_config_path).transpose()?;
    let mut files = conventional_inventory(checkout)?;
    if let Some(path) = &explicit_path {
        if checkout.join(path.as_str()).is_file() && !files.contains(path) {
            files.push(path.clone());
        }
    } else if !files.iter().any(|path| {
        matches!(
            path.as_str(),
            ".devcontainer/devcontainer.json" | ".devcontainer.json"
        )
    }) {
        files.extend(folder_inventory(checkout)?);
    }

    let selected = discover_config(ConfigInventory::new(&files), explicit_path.as_ref())?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let contained = validate_explicit_config(checkout, Path::new(selected.as_str()))?;
    let canonical_path = ConfigPath::parse(contained.as_str())?;
    let absolute = checkout.join(canonical_path.as_str());
    let bytes = read_bounded(&absolute)?;
    Ok(Some(ConfigSource {
        path: canonical_path,
        bytes,
    }))
}

fn path_to_config_path(path: &Path) -> Result<ConfigPath, ConfigLoadError> {
    let text = path.to_str().ok_or(ConfigContainmentError::NonUtf8)?;
    Ok(ConfigPath::parse(text)?)
}

fn conventional_inventory(checkout: &Path) -> Result<Vec<ConfigPath>, ConfigLoadError> {
    let mut files = Vec::new();
    for value in [".devcontainer/devcontainer.json", ".devcontainer.json"] {
        let absolute = checkout.join(value);
        match fs::metadata(&absolute) {
            Ok(metadata) if metadata.is_file() => files.push(ConfigPath::parse(value)?),
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigLoadError::Inventory {
                    path: absolute,
                    source,
                });
            }
        }
    }
    Ok(files)
}

fn folder_inventory(checkout: &Path) -> Result<Vec<ConfigPath>, ConfigLoadError> {
    let directory = checkout.join(".devcontainer");
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ConfigLoadError::Inventory {
                path: directory,
                source,
            });
        }
    };
    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ConfigLoadError::Inventory {
            path: directory.clone(),
            source,
        })?;
        let Some(folder) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if folder.contains('/') || matches!(folder.as_str(), "." | "..") {
            continue;
        }
        let candidate = entry.path().join("devcontainer.json");
        match fs::metadata(&candidate) {
            Ok(metadata) if metadata.is_file() => files.push(ConfigPath::parse(&format!(
                ".devcontainer/{folder}/devcontainer.json"
            ))?),
            Ok(_) => {}
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(ConfigLoadError::Inventory {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Ok(files)
}

fn read_bounded(path: &Path) -> Result<Vec<u8>, ConfigLoadError> {
    let file = File::open(path).map_err(|source| ConfigLoadError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take((MAX_CONFIG_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| ConfigLoadError::Read {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(ConfigLoadError::TooLarge {
            path: path.to_path_buf(),
            limit: MAX_CONFIG_BYTES,
        });
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn host_boundary_reads_discovered_bounded_bytes() {
        let checkout = tempfile::tempdir().expect("checkout should be created");
        fs::create_dir(checkout.path().join(".devcontainer"))
            .expect("configuration directory should be created");
        fs::write(
            checkout.path().join(".devcontainer/devcontainer.json"),
            b"{ /* comment */ \"image\": \"debian:13\" }",
        )
        .expect("configuration should be written");
        let source = discover_and_read_config(checkout.path(), None)
            .expect("discovery should succeed")
            .expect("configuration should exist");
        assert_eq!(source.path().as_str(), ".devcontainer/devcontainer.json");
    }

    #[test]
    fn host_boundary_reports_no_configuration_without_creating_files() {
        let checkout = tempfile::tempdir().expect("checkout should be created");
        let source =
            discover_and_read_config(checkout.path(), None).expect("absence should not fail");
        assert_eq!(source, None);
    }

    #[cfg(unix)]
    #[test]
    fn discovered_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;

        let checkout = tempfile::tempdir().expect("checkout should be created");
        let outside = tempfile::NamedTempFile::new().expect("outside file should be created");
        fs::create_dir(checkout.path().join(".devcontainer"))
            .expect("configuration directory should be created");
        symlink(
            outside.path(),
            checkout.path().join(".devcontainer/devcontainer.json"),
        )
        .expect("symlink should be created");
        let error = discover_and_read_config(checkout.path(), None)
            .expect_err("escaping discovery should fail");
        assert!(matches!(error, ConfigLoadError::Containment(_)));
    }
}
