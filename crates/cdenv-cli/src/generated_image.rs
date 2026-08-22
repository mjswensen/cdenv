//! Deterministic generated Feature-image material and UID/GID mutation planning.

use std::collections::BTreeMap;

use cdenv_devcontainer::{FeatureValue, ResolvedFeature};
use serde_json::Value;
use thiserror::Error;

use crate::GeneratedContextFile;

const METADATA_LABEL: &str = "devcontainer.metadata";

/// One Feature directory already copied into generated build material.
#[derive(Clone, Debug, PartialEq)]
pub struct GeneratedFeature {
    /// Deterministic resolved Feature, including effective options.
    pub resolved: ResolvedFeature,
    /// Files below the Feature directory, keyed by a safe slash-relative path.
    pub files: Vec<GeneratedContextFile>,
}

/// Immutable material for a Feature-derived Docker image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedImagePlan {
    dockerfile: String,
    files: Vec<GeneratedContextFile>,
}

/// A safe generated-image planning failure.
#[derive(Debug, Error, PartialEq, Eq)]
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
    #[expect(
        clippy::format_push_string,
        reason = "the short deterministic Dockerfile rendering remains directly readable"
    )]
    pub fn new(
        base_image: &str,
        features: &[GeneratedFeature],
        metadata: &Value,
        labels: &BTreeMap<String, String>,
    ) -> Result<Self, GeneratedImageError> {
        let metadata =
            serde_json::to_string(metadata).map_err(|_| GeneratedImageError::MetadataEncoding)?;
        let mut dockerfile = format!("FROM {base_image}\nLABEL {METADATA_LABEL}={metadata:?}\n");
        for (key, value) in labels {
            dockerfile.push_str(&format!("LABEL {key}={value:?}\n"));
        }
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
            dockerfile.push_str(&format!("COPY {directory}/ /tmp/cdenv-feature-{index}/\n"));
            dockerfile.push_str(&format!("RUN --mount=type=cache,target=/var/cache/cdenv-feature-{index} \\\n    cd /tmp/cdenv-feature-{index} && ./install.sh {}\n", feature_arguments(&feature.resolved.options)));
        }
        Ok(Self { dockerfile, files })
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

fn feature_arguments(options: &BTreeMap<String, FeatureValue>) -> String {
    options
        .iter()
        .map(|(name, value)| match value {
            FeatureValue::Boolean(value) => format!("{name}={value}"),
            FeatureValue::String(value) => format!("{name}={value:?}"),
        })
        .collect::<Vec<_>>()
        .join(" ")
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
    let account = accounts
        .iter()
        .find(|account| account.name == remote_user)
        .ok_or_else(|| UidGidUpdateError::MissingUser {
            user: remote_user.to_owned(),
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
