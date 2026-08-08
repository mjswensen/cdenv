//! Workspace labels, SSH hostnames, and repository-name derivation.

use std::borrow::Cow;
use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// The maximum number of ASCII characters in a workspace name.
pub const MAX_WORKSPACE_NAME_LENGTH: usize = 63;

/// The suffix used for cdenv's explicit workspace SSH hostnames.
pub const WORKSPACE_HOST_SUFFIX: &str = ".cdenv";

/// A validated workspace label.
///
/// Workspace names contain only lowercase ASCII letters, digits, and hyphens.
/// They begin and end with a letter or digit and contain at most 63 characters.
/// The private representation prevents unvalidated text from crossing a
/// workspace identity boundary.
///
/// # Examples
///
/// ```
/// use cdenv_core::WorkspaceName;
///
/// let name: WorkspaceName = "project-2".parse()?;
/// assert_eq!(name.as_str(), "project-2");
/// # Ok::<(), cdenv_core::WorkspaceNameError>(())
/// ```
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceName(String);

/// An error returned when parsing an explicit [`WorkspaceName`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceNameError {
    /// The name is empty.
    #[error("a workspace name cannot be empty")]
    Empty,
    /// The name exceeds [`MAX_WORKSPACE_NAME_LENGTH`].
    #[error(
        "a workspace name cannot exceed {maximum} characters; the supplied name contains {length}"
    )]
    TooLong {
        /// The supplied name's byte length.
        length: usize,
        /// The supported maximum length.
        maximum: usize,
    },
    /// The first character is not an ASCII letter or digit.
    #[error("a workspace name must begin with an ASCII letter or digit, not {character:?}")]
    InvalidStart {
        /// The invalid first character.
        character: char,
    },
    /// The final character is not an ASCII letter or digit.
    #[error("a workspace name must end with an ASCII letter or digit, not {character:?}")]
    InvalidEnd {
        /// The invalid final character.
        character: char,
    },
    /// A character is outside the workspace-name alphabet.
    #[error(
        "workspace name character {character:?} at byte {index} is invalid; use lowercase ASCII letters, digits, or hyphens"
    )]
    InvalidCharacter {
        /// The byte index of the invalid character.
        index: usize,
        /// The invalid character.
        character: char,
    },
}

impl WorkspaceName {
    /// Parses and validates an explicit workspace name.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceNameError`] when `value` violates any workspace label
    /// rule.
    pub fn parse(value: &str) -> Result<Self, WorkspaceNameError> {
        Self::try_from(value)
    }

    /// Derives a default name from a Git URL, SCP-like source, or local path.
    ///
    /// This is equivalent to [`derive_workspace_name`].
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceNameSelectionError`] when no valid default can be
    /// derived. Every error directs the caller to request an explicit `--name`.
    pub fn derive_from_git_source(source: &str) -> Result<Self, WorkspaceNameSelectionError> {
        derive_workspace_name(source)
    }

    /// Derives a default name from a local Git path without accessing it.
    ///
    /// This is equivalent to [`derive_workspace_name_from_path`].
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceNameSelectionError`] when the path has no UTF-8 final
    /// component or normalization does not produce a valid name.
    pub fn derive_from_local_path(path: &Path) -> Result<Self, WorkspaceNameSelectionError> {
        derive_workspace_name_from_path(path)
    }

    /// Returns the validated workspace label.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_workspace_name(value: &str) -> Result<(), WorkspaceNameError> {
    if value.is_empty() {
        return Err(WorkspaceNameError::Empty);
    }

    if value.len() > MAX_WORKSPACE_NAME_LENGTH {
        return Err(WorkspaceNameError::TooLong {
            length: value.len(),
            maximum: MAX_WORKSPACE_NAME_LENGTH,
        });
    }

    for (index, character) in value.char_indices() {
        if !(character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-') {
            return Err(WorkspaceNameError::InvalidCharacter { index, character });
        }
    }

    let Some(first) = value.chars().next() else {
        return Err(WorkspaceNameError::Empty);
    };
    if !first.is_ascii_alphanumeric() {
        return Err(WorkspaceNameError::InvalidStart { character: first });
    }

    let Some(last) = value.chars().next_back() else {
        return Err(WorkspaceNameError::Empty);
    };
    if !last.is_ascii_alphanumeric() {
        return Err(WorkspaceNameError::InvalidEnd { character: last });
    }

    Ok(())
}

impl AsRef<str> for WorkspaceName {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for WorkspaceName {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for WorkspaceName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for WorkspaceName {
    type Err = WorkspaceNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for WorkspaceName {
    type Error = WorkspaceNameError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate_workspace_name(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for WorkspaceName {
    type Error = WorkspaceNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_workspace_name(&value)?;
        Ok(Self(value))
    }
}

impl Serialize for WorkspaceName {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for WorkspaceName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// A validated explicit SSH hostname for one workspace.
///
/// The host is always `<workspace>.cdenv`; wildcard host expansion is not part
/// of this type.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WorkspaceHost(WorkspaceName);

/// An error returned when parsing a [`WorkspaceHost`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceHostError {
    /// The hostname does not have the exact `.cdenv` suffix.
    #[error("a workspace host must end with the exact suffix `.cdenv`")]
    InvalidSuffix,
    /// The label before `.cdenv` is not a valid workspace name.
    #[error("invalid workspace label in host: {0}")]
    InvalidWorkspaceName(#[from] WorkspaceNameError),
}

impl WorkspaceHost {
    /// Creates the exact SSH hostname for `workspace_name`.
    #[must_use]
    pub const fn from_workspace_name(workspace_name: WorkspaceName) -> Self {
        Self(workspace_name)
    }

    /// Parses and validates an explicit cdenv SSH hostname.
    ///
    /// # Errors
    ///
    /// Returns [`WorkspaceHostError`] unless `value` consists of one valid
    /// workspace label followed by `.cdenv`.
    pub fn parse(value: &str) -> Result<Self, WorkspaceHostError> {
        Self::try_from(value)
    }

    /// Returns the workspace identity represented by this hostname.
    #[must_use]
    pub const fn workspace_name(&self) -> &WorkspaceName {
        &self.0
    }

    /// Consumes the hostname and returns its workspace identity.
    #[must_use]
    pub fn into_workspace_name(self) -> WorkspaceName {
        self.0
    }
}

impl fmt::Display for WorkspaceHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}{WORKSPACE_HOST_SUFFIX}",
            self.workspace_name()
        )
    }
}

impl From<WorkspaceName> for WorkspaceHost {
    fn from(value: WorkspaceName) -> Self {
        Self::from_workspace_name(value)
    }
}

impl From<&WorkspaceName> for WorkspaceHost {
    fn from(value: &WorkspaceName) -> Self {
        Self::from_workspace_name(value.clone())
    }
}

impl FromStr for WorkspaceHost {
    type Err = WorkspaceHostError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for WorkspaceHost {
    type Error = WorkspaceHostError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let Some(workspace_name) = value.strip_suffix(WORKSPACE_HOST_SUFFIX) else {
            return Err(WorkspaceHostError::InvalidSuffix);
        };

        Ok(Self(WorkspaceName::parse(workspace_name)?))
    }
}

impl Serialize for WorkspaceHost {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for WorkspaceHost {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value.as_str()).map_err(serde::de::Error::custom)
    }
}

/// An error selecting a default workspace name.
///
/// These errors never choose a suffixed alternative. The future atomic
/// reservation layer reports conflicts through [`Self::already_reserved`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkspaceNameSelectionError {
    /// The source has no repository path component.
    #[error(
        "cannot derive a workspace name because the repository source has no final path component; specify one with `--name`"
    )]
    MissingFinalComponent,
    /// A local path's final component is not UTF-8.
    #[error(
        "cannot derive a workspace name from a non-UTF-8 repository path; specify one with `--name`"
    )]
    NonUtf8Path,
    /// A URL uses a source form outside the supported derivation contract.
    #[error(
        "cannot derive a workspace name from unsupported URL scheme {scheme:?}; specify one with `--name`"
    )]
    UnsupportedUrlScheme {
        /// The rejected URL scheme.
        scheme: String,
    },
    /// A percent escape in a URL path is malformed or not UTF-8.
    #[error(
        "cannot derive a workspace name because the repository URL path has invalid percent encoding; specify one with `--name`"
    )]
    InvalidUrlEncoding,
    /// The normalized final component is not a valid workspace label.
    #[error(
        "cannot derive a valid workspace name from the repository source: {reason}; specify one with `--name`"
    )]
    InvalidDerivedName {
        /// The workspace rule violated after normalization.
        #[source]
        reason: WorkspaceNameError,
    },
    /// The selected name is already reserved in the configured cdenv root.
    #[error("workspace name `{name}` is already reserved; choose a different name with `--name`")]
    AlreadyReserved {
        /// The conflicting validated name.
        name: WorkspaceName,
    },
}

impl WorkspaceNameSelectionError {
    /// Creates the conflict returned by an atomic workspace reservation.
    ///
    /// The error retains the requested name and does not invent a numeric
    /// suffix.
    #[must_use]
    pub const fn already_reserved(name: WorkspaceName) -> Self {
        Self::AlreadyReserved { name }
    }
}

/// Derives a default workspace name from a Git repository source.
///
/// Supported forms are HTTPS and SSH URLs, SCP-like Git sources, `file://`
/// URLs, and local paths. Derivation takes the final path component, strips one
/// lowercase `.git` suffix, lowercases ASCII, replaces each run of invalid
/// characters with `-`, trims boundary hyphens, and then validates the result.
/// Trailing path separators are ignored.
///
/// # Examples
///
/// ```
/// use cdenv_core::derive_workspace_name;
///
/// let name = derive_workspace_name("git@github.com:Example/My_Project.git")?;
/// assert_eq!(name.as_str(), "my-project");
/// # Ok::<(), cdenv_core::WorkspaceNameSelectionError>(())
/// ```
///
/// # Errors
///
/// Returns [`WorkspaceNameSelectionError`] when the source form is unsupported,
/// has no usable final component, has invalid URL encoding, or derives an empty
/// or overlong name. Errors do not contain the repository source, which may
/// include credentials.
pub fn derive_workspace_name(source: &str) -> Result<WorkspaceName, WorkspaceNameSelectionError> {
    if let Some((scheme, remainder)) = source.split_once("://") {
        if !(scheme.eq_ignore_ascii_case("https")
            || scheme.eq_ignore_ascii_case("ssh")
            || scheme.eq_ignore_ascii_case("file"))
        {
            return Err(WorkspaceNameSelectionError::UnsupportedUrlScheme {
                scheme: scheme.to_owned(),
            });
        }

        let component = url_repository_component(scheme, remainder)?;
        return normalize_repository_component(&component);
    }

    if let Some(remote_path) = scp_like_remote_path(source) {
        let component = final_slash_component(remote_path)?;
        return normalize_repository_component(component);
    }

    derive_workspace_name_from_path(Path::new(source))
}

/// Derives a default workspace name from a local path without filesystem I/O.
///
/// Path handling is lexical. A trailing separator is accepted, while a root,
/// `.`/`..`, or non-UTF-8 final component cannot provide a default name.
///
/// # Errors
///
/// Returns [`WorkspaceNameSelectionError`] when `path` has no UTF-8 final
/// component or normalization derives an invalid workspace name.
pub fn derive_workspace_name_from_path(
    path: &Path,
) -> Result<WorkspaceName, WorkspaceNameSelectionError> {
    let Some(component) = path.file_name() else {
        return Err(WorkspaceNameSelectionError::MissingFinalComponent);
    };
    let Some(component) = component.to_str() else {
        return Err(WorkspaceNameSelectionError::NonUtf8Path);
    };

    normalize_repository_component(component)
}

fn url_repository_component<'a>(
    scheme: &str,
    remainder: &'a str,
) -> Result<Cow<'a, str>, WorkspaceNameSelectionError> {
    let without_query = remainder
        .find(['?', '#'])
        .map_or(remainder, |index| &remainder[..index]);

    let path = if scheme.eq_ignore_ascii_case("file") && without_query.starts_with('/') {
        without_query
    } else {
        let Some((authority, path)) = without_query.split_once('/') else {
            return Err(WorkspaceNameSelectionError::MissingFinalComponent);
        };
        if authority.is_empty() && !scheme.eq_ignore_ascii_case("file") {
            return Err(WorkspaceNameSelectionError::MissingFinalComponent);
        }
        path
    };

    let component = final_slash_component(path)?;
    percent_decode(component)
}

fn final_slash_component(path: &str) -> Result<&str, WorkspaceNameSelectionError> {
    let path = path.trim_end_matches('/');
    let component = path.rsplit('/').next().filter(|value| !value.is_empty());
    component.ok_or(WorkspaceNameSelectionError::MissingFinalComponent)
}

fn scp_like_remote_path(source: &str) -> Option<&str> {
    let (prefix, path) = source.split_once(':')?;

    if prefix.is_empty() || prefix.contains(['/', '\\']) || prefix.chars().any(char::is_whitespace)
    {
        return None;
    }

    let windows_drive = prefix.len() == 1
        && prefix
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
        && (path.starts_with('/') || path.starts_with('\\'));
    if windows_drive {
        return None;
    }

    Some(path)
}

fn percent_decode(value: &str) -> Result<Cow<'_, str>, WorkspaceNameSelectionError> {
    if !value.as_bytes().contains(&b'%') {
        return Ok(Cow::Borrowed(value));
    }

    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] != b'%' {
            decoded.push(bytes[index]);
            index += 1;
            continue;
        }

        let Some(high) = bytes
            .get(index + 1)
            .and_then(|value| hexadecimal_value(*value))
        else {
            return Err(WorkspaceNameSelectionError::InvalidUrlEncoding);
        };
        let Some(low) = bytes
            .get(index + 2)
            .and_then(|value| hexadecimal_value(*value))
        else {
            return Err(WorkspaceNameSelectionError::InvalidUrlEncoding);
        };
        decoded.push((high << 4) | low);
        index += 3;
    }

    String::from_utf8(decoded)
        .map(Cow::Owned)
        .map_err(|_| WorkspaceNameSelectionError::InvalidUrlEncoding)
}

const fn hexadecimal_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn normalize_repository_component(
    component: &str,
) -> Result<WorkspaceName, WorkspaceNameSelectionError> {
    let component = component.strip_suffix(".git").unwrap_or(component);
    let mut normalized = String::with_capacity(component.len());
    let mut invalid_run = false;

    for character in component.chars() {
        let character = character.to_ascii_lowercase();
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if invalid_run && !normalized.is_empty() {
                normalized.push('-');
            }
            normalized.push(character);
            invalid_run = false;
        } else if character == '-' {
            if invalid_run && !normalized.is_empty() {
                normalized.push('-');
            }
            if !normalized.is_empty() {
                normalized.push('-');
            }
            invalid_run = false;
        } else {
            invalid_run = true;
        }
    }

    while normalized.ends_with('-') {
        normalized.pop();
    }

    WorkspaceName::try_from(normalized)
        .map_err(|reason| WorkspaceNameSelectionError::InvalidDerivedName { reason })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        MAX_WORKSPACE_NAME_LENGTH, WorkspaceHost, WorkspaceHostError, WorkspaceName,
        WorkspaceNameError, WorkspaceNameSelectionError, derive_workspace_name,
        derive_workspace_name_from_path,
    };

    #[test]
    fn workspace_name_accepts_a_single_lowercase_letter() {
        assert_eq!(
            WorkspaceName::parse("a").map(|name| name.to_string()),
            Ok("a".to_owned())
        );
    }

    #[test]
    fn workspace_name_accepts_lowercase_letters_digits_and_internal_hyphens() {
        assert_eq!(
            WorkspaceName::parse("project-2-api").map(|name| name.to_string()),
            Ok("project-2-api".to_owned())
        );
    }

    #[test]
    fn workspace_name_accepts_a_digit_at_each_boundary() {
        assert_eq!(
            WorkspaceName::parse("2project3").map(|name| name.to_string()),
            Ok("2project3".to_owned())
        );
    }

    #[test]
    fn workspace_name_accepts_the_63_character_limit() {
        let value = "a".repeat(MAX_WORKSPACE_NAME_LENGTH);

        assert!(WorkspaceName::parse(&value).is_ok());
    }

    #[test]
    fn workspace_name_rejects_empty_text() {
        assert_eq!(WorkspaceName::parse(""), Err(WorkspaceNameError::Empty));
    }

    #[test]
    fn workspace_name_rejects_more_than_63_characters() {
        let value = "a".repeat(MAX_WORKSPACE_NAME_LENGTH + 1);

        assert_eq!(
            WorkspaceName::parse(&value),
            Err(WorkspaceNameError::TooLong {
                length: MAX_WORKSPACE_NAME_LENGTH + 1,
                maximum: MAX_WORKSPACE_NAME_LENGTH,
            })
        );
    }

    #[test]
    fn workspace_name_rejects_a_leading_hyphen() {
        assert_eq!(
            WorkspaceName::parse("-project"),
            Err(WorkspaceNameError::InvalidStart { character: '-' })
        );
    }

    #[test]
    fn workspace_name_rejects_a_trailing_hyphen() {
        assert_eq!(
            WorkspaceName::parse("project-"),
            Err(WorkspaceNameError::InvalidEnd { character: '-' })
        );
    }

    #[test]
    fn workspace_name_rejects_uppercase_ascii() {
        assert_eq!(
            WorkspaceName::parse("Project"),
            Err(WorkspaceNameError::InvalidCharacter {
                index: 0,
                character: 'P'
            })
        );
    }

    #[test]
    fn workspace_name_rejects_underscore() {
        assert_eq!(
            WorkspaceName::parse("my_project"),
            Err(WorkspaceNameError::InvalidCharacter {
                index: 2,
                character: '_'
            })
        );
    }

    #[test]
    fn workspace_name_rejects_unicode() {
        let result = WorkspaceName::parse("café");

        assert!(
            matches!(
                &result,
                Err(WorkspaceNameError::InvalidCharacter {
                    character: 'é', ..
                })
            ),
            "unexpected workspace name result: {result:?}"
        );
    }

    #[test]
    fn workspace_name_rejects_path_punctuation() {
        let result = WorkspaceName::parse("group/project");

        assert!(
            matches!(
                &result,
                Err(WorkspaceNameError::InvalidCharacter { character: '/', .. })
            ),
            "unexpected workspace name result: {result:?}"
        );
    }

    #[test]
    fn workspace_name_serde_round_trip_preserves_validation() {
        let name = WorkspaceName::parse("project-2").expect("the test name should be valid");
        let json = serde_json::to_string(&name).expect("serialization should succeed");
        let decoded: WorkspaceName =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!((json, decoded), (r#""project-2""#.to_owned(), name));
    }

    #[test]
    fn workspace_name_serde_rejects_an_invalid_value() {
        assert!(serde_json::from_str::<WorkspaceName>(r#""Project""#).is_err());
    }

    #[test]
    fn workspace_host_is_derived_from_a_validated_name() {
        let name = WorkspaceName::parse("project").expect("the test name should be valid");

        assert_eq!(WorkspaceHost::from(name).to_string(), "project.cdenv");
    }

    #[test]
    fn workspace_host_parser_recovers_the_workspace_name() {
        let host = WorkspaceHost::parse("project.cdenv").expect("the test host should be valid");

        assert_eq!(host.workspace_name().as_str(), "project");
    }

    #[test]
    fn workspace_host_rejects_a_different_suffix() {
        assert_eq!(
            WorkspaceHost::parse("project.example"),
            Err(WorkspaceHostError::InvalidSuffix)
        );
    }

    #[test]
    fn workspace_host_rejects_an_invalid_workspace_label() {
        let result = WorkspaceHost::parse("Project.cdenv");

        assert!(
            matches!(&result, Err(WorkspaceHostError::InvalidWorkspaceName(_))),
            "unexpected workspace host result: {result:?}"
        );
    }

    #[test]
    fn workspace_host_serde_round_trip_preserves_validation() {
        let host = WorkspaceHost::parse("project.cdenv").expect("the test host should be valid");
        let json = serde_json::to_string(&host).expect("serialization should succeed");
        let decoded: WorkspaceHost =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!((json, decoded), (r#""project.cdenv""#.to_owned(), host));
    }

    #[test]
    fn workspace_host_serde_rejects_an_invalid_value() {
        assert!(serde_json::from_str::<WorkspaceHost>(r#""project.example""#).is_err());
    }

    #[test]
    fn derivation_supports_https_git_urls() {
        let name = derive_workspace_name("https://github.com/Example/Project.git")
            .expect("an HTTPS Git URL should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_supports_ssh_git_urls() {
        let name = derive_workspace_name("ssh://git@github.com/Example/Project.git")
            .expect("an SSH Git URL should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_supports_scp_like_git_urls() {
        let name = derive_workspace_name("git@github.com:Example/Project.git")
            .expect("an SCP-like Git URL should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_supports_file_urls() {
        let name = derive_workspace_name("file:///srv/git/Project.git")
            .expect("a file URL should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_supports_local_git_paths() {
        let name = derive_workspace_name("../repositories/Project.git")
            .expect("a local Git path should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn path_derivation_borrows_a_local_path() {
        let name = derive_workspace_name_from_path(Path::new("/srv/git/Project.git"))
            .expect("a local Git path should derive a name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_strips_one_lowercase_git_suffix() {
        let name = derive_workspace_name("Project.git.git")
            .expect("a repository component should derive a name");

        assert_eq!(name.as_str(), "project-git");
    }

    #[test]
    fn derivation_rejects_a_component_that_is_only_the_git_suffix() {
        let error = derive_workspace_name(".git")
            .expect_err("stripping a component that is only .git should produce no name");

        assert!(
            matches!(
                &error,
                WorkspaceNameSelectionError::InvalidDerivedName {
                    reason: WorkspaceNameError::Empty
                }
            ),
            "unexpected derivation error: {error:?}"
        );
    }

    #[test]
    fn derivation_lowercases_ascii() {
        let name = derive_workspace_name("MyPROJECT")
            .expect("uppercase ASCII should normalize to lowercase");

        assert_eq!(name.as_str(), "myproject");
    }

    #[test]
    fn derivation_collapses_each_invalid_run() {
        let name = derive_workspace_name("my___project...api")
            .expect("invalid runs should normalize to hyphens");

        assert_eq!(name.as_str(), "my-project-api");
    }

    #[test]
    fn derivation_trims_boundary_hyphens() {
        let name =
            derive_workspace_name("---Project---").expect("boundary hyphens should be trimmed");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_replaces_unicode_with_an_invalid_run() {
        let name = derive_workspace_name("Crème-Brûlée.git")
            .expect("ASCII portions around Unicode should derive a name");

        assert_eq!(name.as_str(), "cr-me-br-l-e");
    }

    #[test]
    fn derivation_rejects_an_all_unicode_component() {
        let error = derive_workspace_name("日本語.git")
            .expect_err("an all-Unicode component should derive an empty name");

        assert!(
            matches!(
                &error,
                WorkspaceNameSelectionError::InvalidDerivedName {
                    reason: WorkspaceNameError::Empty
                }
            ),
            "unexpected derivation error: {error:?}"
        );
    }

    #[test]
    fn derivation_rejects_an_empty_source() {
        assert_eq!(
            derive_workspace_name(""),
            Err(WorkspaceNameSelectionError::MissingFinalComponent)
        );
    }

    #[test]
    fn derivation_rejects_an_overlong_result() {
        let source = "a".repeat(MAX_WORKSPACE_NAME_LENGTH + 1);
        let error = derive_workspace_name(&source)
            .expect_err("an overlong derived workspace name should fail");

        assert!(
            matches!(
                &error,
                WorkspaceNameSelectionError::InvalidDerivedName {
                    reason: WorkspaceNameError::TooLong { .. }
                }
            ),
            "unexpected derivation error: {error:?}"
        );
    }

    #[test]
    fn derivation_accepts_trailing_url_separators() {
        let name = derive_workspace_name("https://github.com/Example/Project.git///")
            .expect("trailing URL separators should be ignored");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_accepts_trailing_local_path_separators() {
        let name = derive_workspace_name("/srv/git/Project.git///")
            .expect("trailing local path separators should be ignored");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_rejects_a_url_without_a_repository_component() {
        assert_eq!(
            derive_workspace_name("https://github.com/"),
            Err(WorkspaceNameSelectionError::MissingFinalComponent)
        );
    }

    #[test]
    fn derivation_decodes_a_percent_encoded_url_component() {
        let name = derive_workspace_name("https://github.com/Example/My%20Project.git")
            .expect("a percent-encoded URL component should derive a name");

        assert_eq!(name.as_str(), "my-project");
    }

    #[test]
    fn derivation_ignores_https_query_and_fragment_data() {
        let name = derive_workspace_name("https://example.test/Project.git?ref=main#readme")
            .expect("URL query and fragment data should not enter the name");

        assert_eq!(name.as_str(), "project");
    }

    #[test]
    fn derivation_rejects_invalid_percent_encoding() {
        assert_eq!(
            derive_workspace_name("https://example.test/Project%GG.git"),
            Err(WorkspaceNameSelectionError::InvalidUrlEncoding)
        );
    }

    #[test]
    fn derivation_rejects_non_utf8_percent_encoding() {
        assert_eq!(
            derive_workspace_name("https://example.test/Project%FF.git"),
            Err(WorkspaceNameSelectionError::InvalidUrlEncoding)
        );
    }

    #[test]
    fn derivation_rejects_an_unsupported_url_scheme() {
        assert_eq!(
            derive_workspace_name("http://example.test/Project.git"),
            Err(WorkspaceNameSelectionError::UnsupportedUrlScheme {
                scheme: "http".to_owned()
            })
        );
    }

    #[test]
    fn reserved_name_error_keeps_the_name_and_requests_an_explicit_alternative() {
        let name = WorkspaceName::parse("project").expect("the test name should be valid");
        let error = WorkspaceNameSelectionError::already_reserved(name.clone());

        assert_eq!(
            (error.to_string(), error),
            (
                "workspace name `project` is already reserved; choose a different name with `--name`"
                    .to_owned(),
                WorkspaceNameSelectionError::AlreadyReserved { name }
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_derivation_rejects_a_non_utf8_final_component() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;

        let path = Path::new(OsStr::from_bytes(b"/tmp/repo-\xff"));

        assert_eq!(
            derive_workspace_name_from_path(path),
            Err(WorkspaceNameSelectionError::NonUtf8Path)
        );
    }
}
