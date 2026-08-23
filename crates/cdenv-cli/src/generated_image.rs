//! Deterministic generated Feature-image material and UID/GID mutation planning.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use cdenv_core::ContainerArchitecture;
use cdenv_devcontainer::{DockerfileBuildPlan, FeatureValue, RepositoryPath, ResolvedFeature};
use serde_json::Value;
use thiserror::Error;

use crate::{AgentArtifactError, AgentArtifactProvider, GeneratedContextFile};

const METADATA_LABEL: &str = "devcontainer.metadata";
const AGENT_CONTEXT_PATH: &str = ".cdenv/cdenv-agent";
const UPDATE_CONTEXT_PATH: &str = ".cdenv/update-user.json";

/// One Feature directory already copied into generated build material.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedFeature {
    /// Deterministic resolved Feature, including effective options.
    pub resolved: ResolvedFeature,
    /// Files below the Feature directory, keyed by a safe slash-relative path.
    pub files: Vec<GeneratedContextFile>,
}

impl GeneratedFeature {
    /// Reads one already-verified, regular Feature directory into generated material.
    ///
    /// The caller must supply a cache/extraction directory, never a checkout
    /// path. Symlinks and special files are rejected rather than followed.
    ///
    /// # Errors
    ///
    /// Returns a typed source-tree error for unreadable, linked, or non-regular
    /// Feature files.
    pub fn from_directory(
        resolved: ResolvedFeature,
        directory: &Path,
    ) -> Result<Self, GeneratedImageError> {
        let mut files = Vec::new();
        collect_feature_files(directory, directory, &mut files)?;
        Ok(Self { resolved, files })
    }
}

/// Immutable material for a Feature-derived Docker image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImagePlan {
    dockerfile: String,
    files: Vec<GeneratedContextFile>,
}

/// Architecture-matched helper material for one derived UID/GID layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedUidGidUpdate {
    helper: Vec<u8>,
    manifest: Vec<u8>,
}

impl GeneratedUidGidUpdate {
    /// Selects the verified static helper for the inspected base-image architecture.
    ///
    /// # Errors
    ///
    /// Rejects an unavailable/malformed helper or unsafe account name.
    pub fn from_provider(
        provider: AgentArtifactProvider,
        architecture: ContainerArchitecture,
        mutation: &UidGidMutation,
    ) -> Result<Self, GeneratedImageError> {
        let helper = provider.artifact(architecture)?;
        Self::from_helper(helper, mutation)
    }

    fn from_helper(helper: &[u8], mutation: &UidGidMutation) -> Result<Self, GeneratedImageError> {
        if !valid_account_name(&mutation.account) {
            return Err(GeneratedImageError::InvalidAccount);
        }
        let manifest = serde_json::to_vec(&serde_json::json!({
            "account": mutation.account,
            "uid": mutation.target_uid,
            "gid": mutation.target_gid,
        }))
        .map_err(|_| GeneratedImageError::MetadataEncoding)?;
        Ok(Self {
            helper: helper.to_vec(),
            manifest,
        })
    }
}

/// A safe generated-image planning failure.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum GeneratedImageError {
    /// A Feature source was not represented by a safe relative file.
    #[error("Feature `{feature}` contains an unsafe generated path `{path}`")]
    UnsafeFeaturePath {
        /// Validated Feature identifier.
        feature: String,
        /// Rejected path rendering.
        path: String,
    },
    /// The source lacks the Feature contract's install script.
    #[error("Feature `{feature}` has no regular install.sh file")]
    MissingInstallScript {
        /// Validated Feature identifier.
        feature: String,
    },
    /// Final image metadata could not be encoded as a Docker label.
    #[error("generated image metadata cannot be encoded")]
    MetadataEncoding,
    /// The base image, label, or option could alter generated Dockerfile structure.
    #[error("generated image input `{field}` is unsafe")]
    UnsafeDockerfileInput {
        /// Safe input category.
        field: &'static str,
    },
    /// The architecture-matched static helper was unavailable or invalid.
    #[error(transparent)]
    AgentArtifact(#[from] AgentArtifactError),
    /// The account cannot be represented by the generated helper manifest.
    #[error("generated UID/GID account name is unsafe")]
    InvalidAccount,
    /// A verified Feature tree could not be read as regular files.
    #[error("cannot read Feature source `{path}`: {source}")]
    FeatureSource {
        /// Source path.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A Feature source contains a symlink, special file, or invalid root.
    #[error("Feature source `{path}` is not a regular contained file tree")]
    InvalidFeatureSource {
        /// Rejected source path.
        path: PathBuf,
    },
    /// The generated context cannot be represented as a neutral Docker plan.
    #[error("cannot construct generated Docker build plan")]
    BuildPlan,
}

impl GeneratedImagePlan {
    /// Produces a deterministic Dockerfile and entirely generated context.
    ///
    /// Each supplied Feature contributes exactly one `COPY`/`RUN` layer.  The
    /// caller supplies only verified Feature files, so this pure boundary never
    /// reads a checkout or an ambient cache.
    ///
    /// # Errors
    ///
    /// Returns an error for unsafe paths, missing install scripts, or metadata
    /// serialization failures.
    pub fn new(
        base_image: &str,
        features: &[GeneratedFeature],
        metadata: &Value,
        labels: &BTreeMap<String, String>,
    ) -> Result<Self, GeneratedImageError> {
        Self::new_with_uid_update(base_image, features, metadata, labels, None)
    }

    /// Produces generated Feature material followed by an optional UID/GID layer.
    ///
    /// The helper must have been selected for the inspected base-image architecture
    /// by [`crate::AgentArtifactProvider`].
    ///
    /// # Errors
    ///
    /// Returns the same validation failures as [`Self::new`].
    #[expect(
        clippy::format_push_string,
        reason = "the short deterministic Dockerfile rendering remains directly readable"
    )]
    pub fn new_with_uid_update(
        base_image: &str,
        features: &[GeneratedFeature],
        metadata: &Value,
        labels: &BTreeMap<String, String>,
        uid_update: Option<&GeneratedUidGidUpdate>,
    ) -> Result<Self, GeneratedImageError> {
        if base_image.is_empty()
            || base_image.starts_with('-')
            || base_image.chars().any(char::is_whitespace)
            || base_image.chars().any(char::is_control)
        {
            return Err(GeneratedImageError::UnsafeDockerfileInput {
                field: "base image",
            });
        }
        let metadata =
            serde_json::to_string(metadata).map_err(|_| GeneratedImageError::MetadataEncoding)?;
        let mut dockerfile = format!("FROM {base_image}\nUSER root\n");
        let mut files = Vec::new();
        for (index, feature) in features.iter().enumerate() {
            let directory = format!("features/{index}");
            let has_install = feature
                .files
                .iter()
                .any(|file| file.path == std::path::Path::new("install.sh"));
            if !has_install {
                return Err(GeneratedImageError::MissingInstallScript {
                    feature: feature.resolved.metadata.id.clone(),
                });
            }
            for file in &feature.files {
                let path = file.path.to_string_lossy();
                if path.is_empty()
                    || file.path.is_absolute()
                    || path.split('/').any(|part| matches!(part, "" | "." | ".."))
                {
                    return Err(GeneratedImageError::UnsafeFeaturePath {
                        feature: feature.resolved.metadata.id.clone(),
                        path: path.into_owned(),
                    });
                }
                files.push(GeneratedContextFile {
                    path: format!("{directory}/{path}").into(),
                    contents: file.contents.clone(),
                });
            }
            let assignments = feature_assignments(&feature.resolved.options)?;
            dockerfile.push_str(&format!(
                "COPY --chown=0:0 {directory}/ /tmp/cdenv-feature-{index}/\n"
            ));
            dockerfile.push_str(&format!(
                "RUN cd /tmp/cdenv-feature-{index} && {assignments}/bin/sh ./install.sh && cd / && rm -rf /tmp/cdenv-feature-{index}\n"
            ));
        }
        if let Some(update) = uid_update {
            files.push(GeneratedContextFile {
                path: AGENT_CONTEXT_PATH.into(),
                contents: update.helper.clone(),
            });
            files.push(GeneratedContextFile {
                path: UPDATE_CONTEXT_PATH.into(),
                contents: update.manifest.clone(),
            });
            dockerfile.push_str(&format!(
                "COPY --chown=0:0 --chmod=0555 {AGENT_CONTEXT_PATH} /tmp/cdenv-agent\nCOPY --chown=0:0 --chmod=0444 {UPDATE_CONTEXT_PATH} /tmp/cdenv-update-user.json\nRUN /tmp/cdenv-agent update-user /tmp/cdenv-update-user.json && rm -f /tmp/cdenv-agent /tmp/cdenv-update-user.json\n"
            ));
        }
        dockerfile.push_str(&format!("LABEL {METADATA_LABEL}={metadata:?}\n"));
        for (key, value) in labels {
            if key == METADATA_LABEL || !valid_label_key(key) {
                return Err(GeneratedImageError::UnsafeDockerfileInput { field: "label" });
            }
            dockerfile.push_str(&format!("LABEL {key}={value:?}\n"));
        }
        Ok(Self { dockerfile, files })
    }

    /// Returns a neutral `BuildKit` plan for this entirely generated context.
    ///
    /// The Docker CLI adapter ignores the repository paths when used with
    /// generated context and Dockerfile inputs; empty paths avoid leaking a
    /// checkout path into the generated-image plan.
    ///
    /// # Errors
    ///
    /// Returns an error only if the internal neutral paths violate the shared
    /// repository-path invariant.
    pub fn build_plan(&self) -> Result<DockerfileBuildPlan, GeneratedImageError> {
        Ok(DockerfileBuildPlan {
            dockerfile: RepositoryPath::parse("").map_err(|_| GeneratedImageError::BuildPlan)?,
            context: RepositoryPath::parse("").map_err(|_| GeneratedImageError::BuildPlan)?,
            target: None,
            arguments: BTreeMap::new(),
            cache_from: Vec::new(),
            options: Vec::new(),
        })
    }

    /// Returns exact generated Dockerfile bytes.
    #[must_use]
    pub fn dockerfile(&self) -> &[u8] {
        self.dockerfile.as_bytes()
    }

    /// Returns generated context files, never checkout paths.
    #[must_use]
    pub fn files(&self) -> &[GeneratedContextFile] {
        &self.files
    }
}

fn collect_feature_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<GeneratedContextFile>,
) -> Result<(), GeneratedImageError> {
    let metadata =
        fs::symlink_metadata(directory).map_err(|source| GeneratedImageError::FeatureSource {
            path: directory.to_path_buf(),
            source,
        })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(GeneratedImageError::InvalidFeatureSource {
            path: directory.to_path_buf(),
        });
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|source| GeneratedImageError::FeatureSource {
            path: directory.to_path_buf(),
            source,
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| GeneratedImageError::FeatureSource {
            path: directory.to_path_buf(),
            source,
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let metadata =
            fs::symlink_metadata(&path).map_err(|source| GeneratedImageError::FeatureSource {
                path: path.clone(),
                source,
            })?;
        if metadata.file_type().is_symlink() {
            return Err(GeneratedImageError::InvalidFeatureSource { path });
        }
        if metadata.is_dir() {
            collect_feature_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .map_err(|_| GeneratedImageError::InvalidFeatureSource { path: path.clone() })?;
            files.push(GeneratedContextFile {
                path: relative.to_path_buf(),
                contents: fs::read(&path)
                    .map_err(|source| GeneratedImageError::FeatureSource { path, source })?,
            });
        } else {
            return Err(GeneratedImageError::InvalidFeatureSource { path });
        }
    }
    Ok(())
}

fn feature_assignments(
    options: &BTreeMap<String, FeatureValue>,
) -> Result<String, GeneratedImageError> {
    let mut assignments = String::new();
    for (name, value) in options {
        let name = name.to_ascii_uppercase();
        if !valid_environment_name(&name) {
            return Err(GeneratedImageError::UnsafeDockerfileInput {
                field: "Feature option name",
            });
        }
        let value = match value {
            FeatureValue::Boolean(value) => value.to_string(),
            FeatureValue::String(value) => shell_quote(value),
        };
        write!(assignments, "{name}={value} ").map_err(|_| {
            GeneratedImageError::UnsafeDockerfileInput {
                field: "Feature option value",
            }
        })?;
    }
    Ok(assignments)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn valid_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn valid_label_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn valid_account_name(value: &str) -> bool {
    !value.is_empty()
        && value != "root"
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

/// One account observed by the architecture-matched container helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinuxAccount {
    /// Account name.
    pub name: String,
    /// Numeric user ID.
    pub uid: u32,
    /// Primary numeric group ID.
    pub gid: u32,
}

/// The no-shell filesystem mutation requested from the Linux helper.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UidGidMutation {
    /// Effective remote account.
    pub account: String,
    /// Current UID.
    pub current_uid: u32,
    /// Current GID.
    pub current_gid: u32,
    /// Requested host UID.
    pub target_uid: u32,
    /// Requested host GID.
    pub target_gid: u32,
}

/// Account-resolution failure before a container filesystem is mutated.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum UidGidUpdateError {
    /// A named remote user was not present in helper output.
    #[error("cannot update UID/GID: remote user `{user}` does not exist")]
    MissingUser {
        /// Requested user name.
        user: String,
    },
    /// Root must never be rewritten.
    #[error("cannot update UID/GID for root")]
    RootAccount,
    /// A different account owns one of the requested IDs.
    #[error("cannot update UID/GID: {kind} {id} belongs to `{owner}`")]
    Conflict {
        /// `UID` or `GID`.
        kind: &'static str,
        /// Conflicting numeric ID.
        id: u32,
        /// Existing account.
        owner: String,
    },
}

/// Plans a safe UID/GID rewrite without persisting host identity.
///
/// `None` means the feature/configuration disabled updating or the account
/// already has both requested IDs. Root and conflicting accounts always fail.
///
/// # Errors
///
/// Returns a typed missing-user, root-account, or account-conflict error before
/// any container mutation occurs.
#[expect(
    clippy::similar_names,
    reason = "UID and GID are the conventional paired Linux account terms"
)]
pub fn plan_uid_gid_update(
    enabled: bool,
    remote_user: &str,
    accounts: &[LinuxAccount],
    host_uid: u32,
    host_gid: u32,
) -> Result<Option<UidGidMutation>, UidGidUpdateError> {
    if !enabled {
        return Ok(None);
    }
    let remote_account = remote_user.split(':').next().unwrap_or_default();
    let account = accounts
        .iter()
        .find(|account| account.name == remote_account)
        .ok_or_else(|| UidGidUpdateError::MissingUser {
            user: remote_account.to_owned(),
        })?;
    if account.uid == 0 {
        return Err(UidGidUpdateError::RootAccount);
    }
    for other in accounts.iter().filter(|other| other.name != account.name) {
        if other.uid == host_uid {
            return Err(UidGidUpdateError::Conflict {
                kind: "UID",
                id: host_uid,
                owner: other.name.clone(),
            });
        }
        if other.gid == host_gid {
            return Err(UidGidUpdateError::Conflict {
                kind: "GID",
                id: host_gid,
                owner: other.name.clone(),
            });
        }
    }
    if account.uid == host_uid && account.gid == host_gid {
        return Ok(None);
    }
    Ok(Some(UidGidMutation {
        account: account.name.clone(),
        current_uid: account.uid,
        current_gid: account.gid,
        target_uid: host_uid,
        target_gid: host_gid,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature(id: &str, options: BTreeMap<String, FeatureValue>) -> GeneratedFeature {
        let reference =
            cdenv_devcontainer::FeatureReference::parse(&format!("./{id}")).expect("reference");
        GeneratedFeature {
            resolved: ResolvedFeature {
                identity: cdenv_devcontainer::FeatureInstallIdentity::local(&reference),
                reference,
                metadata: cdenv_devcontainer::FeatureMetadata {
                    id: id.to_owned(),
                    version: "1.0.0".to_owned(),
                    options: BTreeMap::new(),
                    depends_on: BTreeMap::new(),
                    installs_after: Vec::new(),
                    contributions: cdenv_devcontainer::FeatureContributions::default(),
                },
                options,
            },
            files: vec![GeneratedContextFile {
                path: "install.sh".into(),
                contents: b"#!/bin/sh\n".to_vec(),
            }],
        }
    }

    #[test]
    fn generated_dockerfile_is_ordered_layered_and_metadata_last() {
        let first = feature(
            "first",
            BTreeMap::from([("message".to_owned(), FeatureValue::String("a'b".to_owned()))]),
        );
        let second = feature(
            "second",
            BTreeMap::from([("enabled".to_owned(), FeatureValue::Boolean(true))]),
        );
        let update = GeneratedUidGidUpdate::from_helper(
            b"ELF helper",
            &UidGidMutation {
                account: "dev".to_owned(),
                current_uid: 1000,
                current_gid: 1000,
                target_uid: 501,
                target_gid: 20,
            },
        )
        .expect("update material");
        let plan = GeneratedImagePlan::new_with_uid_update(
            "example.invalid/base@sha256:abc",
            &[first, second],
            &serde_json::json!([{"remoteUser":"dev"}]),
            &BTreeMap::from([("cdenv.retention".to_owned(), "workspace".to_owned())]),
            Some(&update),
        )
        .expect("plan");
        assert_eq!(
            std::str::from_utf8(plan.dockerfile()).expect("Dockerfile"),
            concat!(
                "FROM example.invalid/base@sha256:abc\n",
                "USER root\n",
                "COPY --chown=0:0 features/0/ /tmp/cdenv-feature-0/\n",
                "RUN cd /tmp/cdenv-feature-0 && MESSAGE='a'\"'\"'b' /bin/sh ./install.sh && cd / && rm -rf /tmp/cdenv-feature-0\n",
                "COPY --chown=0:0 features/1/ /tmp/cdenv-feature-1/\n",
                "RUN cd /tmp/cdenv-feature-1 && ENABLED=true /bin/sh ./install.sh && cd / && rm -rf /tmp/cdenv-feature-1\n",
                "COPY --chown=0:0 --chmod=0555 .cdenv/cdenv-agent /tmp/cdenv-agent\n",
                "COPY --chown=0:0 --chmod=0444 .cdenv/update-user.json /tmp/cdenv-update-user.json\n",
                "RUN /tmp/cdenv-agent update-user /tmp/cdenv-update-user.json && rm -f /tmp/cdenv-agent /tmp/cdenv-update-user.json\n",
                "LABEL devcontainer.metadata=\"[{\\\"remoteUser\\\":\\\"dev\\\"}]\"\n",
                "LABEL cdenv.retention=\"workspace\"\n",
            )
        );
        assert_eq!(plan.files().len(), 4);
    }

    #[test]
    fn uid_gid_planning_preserves_root_and_conflict_invariants() {
        let accounts = vec![
            LinuxAccount {
                name: "root".into(),
                uid: 0,
                gid: 0,
            },
            LinuxAccount {
                name: "dev".into(),
                uid: 1000,
                gid: 1000,
            },
        ];
        assert!(matches!(
            plan_uid_gid_update(true, "root", &accounts, 501, 20),
            Err(UidGidUpdateError::RootAccount)
        ));
        assert!(matches!(
            plan_uid_gid_update(true, "dev", &accounts, 0, 20),
            Err(UidGidUpdateError::Conflict { .. })
        ));
        assert_eq!(
            plan_uid_gid_update(true, "dev", &accounts, 501, 20)
                .expect("plan")
                .expect("changed")
                .target_gid,
            20
        );
    }
}
