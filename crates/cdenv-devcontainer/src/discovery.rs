//! Pure Dev Container configuration discovery.

use std::fmt;

use thiserror::Error;

/// A validated UTF-8 path relative to a repository checkout.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigPath(String);

impl ConfigPath {
    /// Validates a slash-separated repository-relative path.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigPathError`] for empty, absolute, traversing, or
    /// backslash-separated paths.
    pub fn parse(value: &str) -> Result<Self, ConfigPathError> {
        if value.is_empty() {
            return Err(ConfigPathError::Empty);
        }
        if value.starts_with('/') {
            return Err(ConfigPathError::Absolute);
        }
        if value.contains('\\') {
            return Err(ConfigPathError::Backslash);
        }
        if value
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        {
            return Err(ConfigPathError::InvalidComponent);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the normalized repository-relative text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ConfigPath {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A repository-relative configuration path validation failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConfigPathError {
    /// The path was empty.
    #[error("configuration path must not be empty")]
    Empty,
    /// The path was absolute.
    #[error("configuration path must be repository-relative")]
    Absolute,
    /// The path used host-dependent separators.
    #[error("configuration path must use forward slashes")]
    Backslash,
    /// A path component was empty, current-directory, or parent-directory.
    #[error("configuration path contains an invalid component")]
    InvalidComponent,
}

/// A filesystem-free inventory of validated repository-relative regular files.
#[derive(Clone, Copy, Debug)]
pub struct ConfigInventory<'a> {
    files: &'a [ConfigPath],
}

impl<'a> ConfigInventory<'a> {
    /// Creates an inventory from paths validated by the host boundary.
    #[must_use]
    pub const fn new(files: &'a [ConfigPath]) -> Self {
        Self { files }
    }

    fn contains(&self, path: &ConfigPath) -> bool {
        self.files.contains(path)
    }
}

/// A deterministic configuration discovery failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum DiscoveryError {
    /// An explicit path was not present in the regular-file inventory.
    #[error("explicit configuration `{path}` is not a repository regular file")]
    ExplicitNotFound {
        /// Requested path.
        path: ConfigPath,
    },
    /// More than one folder-form configuration was present.
    #[error(
        "ambiguous folder-form Dev Container configurations: {matches:?}; select one with --config"
    )]
    AmbiguousFolder {
        /// Stable lexical list of matching paths.
        matches: Vec<ConfigPath>,
    },
}

/// Selects a Dev Container configuration without accessing the filesystem.
///
/// Explicit selection wins. Automatic discovery checks
/// `.devcontainer/devcontainer.json`, then `.devcontainer.json`, and finally
/// `.devcontainer/<folder>/devcontainer.json`.
///
/// # Errors
///
/// Returns [`DiscoveryError`] when an explicit selection is absent or the
/// folder form has multiple matches.
pub fn discover_config(
    inventory: ConfigInventory<'_>,
    explicit: Option<&ConfigPath>,
) -> Result<Option<ConfigPath>, DiscoveryError> {
    if let Some(path) = explicit {
        return inventory
            .contains(path)
            .then(|| Some(path.clone()))
            .ok_or_else(|| DiscoveryError::ExplicitNotFound { path: path.clone() });
    }

    for conventional in [".devcontainer/devcontainer.json", ".devcontainer.json"] {
        let path = ConfigPath(conventional.to_owned());
        if inventory.contains(&path) {
            return Ok(Some(path));
        }
    }

    let mut folder_matches = inventory
        .files
        .iter()
        .filter(|path| is_folder_form(path.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    folder_matches.sort();
    folder_matches.dedup();
    match folder_matches.len() {
        0 => Ok(None),
        1 => Ok(folder_matches.pop()),
        _ => Err(DiscoveryError::AmbiguousFolder {
            matches: folder_matches,
        }),
    }
}

fn is_folder_form(path: &str) -> bool {
    let Some(rest) = path.strip_prefix(".devcontainer/") else {
        return false;
    };
    let Some(folder) = rest.strip_suffix("/devcontainer.json") else {
        return false;
    };
    !folder.is_empty() && !folder.contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(values: &[&str]) -> Vec<ConfigPath> {
        values
            .iter()
            .map(|value| ConfigPath::parse(value).expect("fixture path should be valid"))
            .collect()
    }

    #[test]
    fn explicit_selection_wins_over_conventional_paths() {
        let files = paths(&["custom/config.json", ".devcontainer/devcontainer.json"]);
        let explicit = ConfigPath::parse("custom/config.json").expect("path should be valid");
        let selected = discover_config(ConfigInventory::new(&files), Some(&explicit))
            .expect("explicit file is inventoried");
        assert_eq!(selected, Some(explicit));
    }

    #[test]
    fn explicit_selection_must_exist_in_inventory() {
        let explicit = ConfigPath::parse("custom/config.json").expect("path should be valid");
        let error = discover_config(ConfigInventory::new(&[]), Some(&explicit))
            .expect_err("absent explicit path should fail");
        assert_eq!(error, DiscoveryError::ExplicitNotFound { path: explicit });
    }

    #[test]
    fn nested_conventional_path_has_highest_automatic_precedence() {
        let files = paths(&[
            ".devcontainer/project/devcontainer.json",
            ".devcontainer.json",
            ".devcontainer/devcontainer.json",
        ]);
        let selected = discover_config(ConfigInventory::new(&files), None)
            .expect("inventory should be unambiguous");
        assert_eq!(
            selected.as_ref().map(ConfigPath::as_str),
            Some(".devcontainer/devcontainer.json")
        );
    }

    #[test]
    fn root_conventional_path_precedes_folder_form() {
        let files = paths(&[
            ".devcontainer/project/devcontainer.json",
            ".devcontainer.json",
        ]);
        let selected = discover_config(ConfigInventory::new(&files), None)
            .expect("inventory should be unambiguous");
        assert_eq!(
            selected.as_ref().map(ConfigPath::as_str),
            Some(".devcontainer.json")
        );
    }

    #[test]
    fn one_folder_form_is_selected() {
        let files = paths(&[".devcontainer/project/devcontainer.json"]);
        let selected = discover_config(ConfigInventory::new(&files), None)
            .expect("one folder form should be selected");
        assert_eq!(
            selected.as_ref().map(ConfigPath::as_str),
            Some(".devcontainer/project/devcontainer.json")
        );
    }

    #[test]
    fn no_configuration_returns_none() {
        let files = paths(&["README.md"]);
        assert_eq!(
            discover_config(ConfigInventory::new(&files), None),
            Ok(None)
        );
    }

    #[test]
    fn multiple_folder_forms_are_reported_in_stable_order() {
        let files = paths(&[
            ".devcontainer/z/devcontainer.json",
            ".devcontainer/a/devcontainer.json",
        ]);
        let error = discover_config(ConfigInventory::new(&files), None)
            .expect_err("multiple folder forms should fail");
        let DiscoveryError::AmbiguousFolder { matches } = error else {
            panic!("expected ambiguity");
        };
        assert_eq!(
            matches.iter().map(ConfigPath::as_str).collect::<Vec<_>>(),
            [
                ".devcontainer/a/devcontainer.json",
                ".devcontainer/z/devcontainer.json"
            ]
        );
    }
}
