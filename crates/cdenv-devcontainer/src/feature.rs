//! Pure Dev Container Feature model.
//!
//! `reference` owns normalization and identity validation; `metadata` owns metadata and options;
//! `resolution` owns graph ordering; `lock` owns frozen-lock encoding and validation.
//! These boundaries perform no I/O. Source transport, authentication, caching, and extraction
//! remain the responsibility of CLI adapters.

use std::collections::BTreeMap;

use thiserror::Error;

mod lock;
mod metadata;
mod reference;
mod resolution;

pub use lock::{FEATURE_LOCK_VERSION, FeatureLock, LockedFeature};
pub use metadata::{FeatureContributions, FeatureMetadata, FeatureOption, FeatureValue};
pub use reference::{FeatureInstallIdentity, FeatureReference};
pub use resolution::resolve_features;

/// An already fetched Feature supplied to the pure resolver.
#[derive(Clone, Debug, PartialEq)]
pub struct FeaturePackage {
    /// The reference by which this package can be requested.
    pub reference: FeatureReference,
    /// Exact install-content identity.
    pub identity: FeatureInstallIdentity,
    /// Validated Feature metadata.
    pub metadata: FeatureMetadata,
    /// Resolved OCI manifest digest, when applicable.
    pub digest: Option<String>,
    /// Downloaded artifact integrity, when applicable.
    pub integrity: Option<String>,
}

/// One root Feature request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FeatureRequest {
    /// Requested reference.
    pub reference: FeatureReference,
    /// Explicit options (defaults are applied by the resolver).
    pub options: BTreeMap<String, FeatureValue>,
}

impl FeatureRequest {
    /// Creates a request with no explicit options.
    #[must_use]
    pub fn new(reference: FeatureReference) -> Self {
        Self {
            reference,
            options: BTreeMap::new(),
        }
    }
}

/// A resolved Feature in deterministic installation order.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedFeature {
    /// Canonical source used for installation.
    pub reference: FeatureReference,
    /// Exact install-content identity.
    pub identity: FeatureInstallIdentity,
    /// Metadata and contributions, represented exactly once.
    pub metadata: FeatureMetadata,
    /// Effective options including defaults.
    pub options: BTreeMap<String, FeatureValue>,
}

/// Complete deterministic resolution result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ResolvedFeatures {
    /// Features in installation order. Every identity occurs exactly once.
    pub installation_order: Vec<ResolvedFeature>,
}

/// Pure Feature model or dependency resolution failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum FeatureError {
    /// The reference is unsupported at the model boundary.
    #[error("unsupported Feature reference `{reference}`: {reason}")]
    UnsupportedReference {
        /// Rejected input.
        reference: String,
        /// Rejection reason.
        reason: String,
    },
    /// Feature metadata is invalid.
    #[error("invalid Feature metadata at {path}: {message}")]
    InvalidMetadata {
        /// JSON property path.
        path: String,
        /// Validation detail.
        message: String,
    },
    /// A content identity is malformed.
    #[error("invalid Feature identity `{value}`: {message}")]
    InvalidIdentity {
        /// Rejected identity.
        value: String,
        /// Validation detail.
        message: &'static str,
    },
    /// A package required by the graph was not injected.
    #[error("Feature package `{reference}` is missing while resolving {path}")]
    MissingPackage {
        /// Missing reference.
        reference: FeatureReference,
        /// Dependency path which requested it.
        path: String,
    },
    /// The package catalog key and package reference disagree.
    #[error("Feature catalog key `{key}` contains package `{package}`")]
    CatalogMismatch {
        /// Catalog lookup key.
        key: FeatureReference,
        /// Reference declared by the package.
        package: FeatureReference,
    },
    /// An unknown option was supplied.
    #[error("Feature `{feature}` has no option `{option}` while resolving {path}")]
    UnknownOption {
        /// Feature identifier.
        feature: String,
        /// Unknown option name.
        option: String,
        /// Dependency path.
        path: String,
    },
    /// An option has the wrong type or is outside its enum.
    #[error(
        "Feature `{feature}` option `{option}` has invalid value `{value}` while resolving {path}"
    )]
    InvalidOption {
        /// Feature identifier.
        feature: String,
        /// Invalid option name.
        option: String,
        /// Rejected value.
        value: FeatureValue,
        /// Dependency path.
        path: String,
    },
    /// Equal content was requested with conflicting effective options.
    #[error(
        "conflicting option `{option}` for {identity:?}: first requested by {first_path}; conflicting request at {second_path}"
    )]
    ConflictingOptions {
        /// Shared install identity.
        identity: FeatureInstallIdentity,
        /// First differing option.
        option: String,
        /// Path of the first request.
        first_path: String,
        /// Path of the conflicting request.
        second_path: String,
    },
    /// An override is malformed or does not identify an installed Feature.
    #[error("invalid overrideFeatureInstallOrder entry `{reference}`: {message}")]
    InvalidOverride {
        /// Rejected override entry.
        reference: String,
        /// Validation detail.
        message: &'static str,
    },
    /// Hard or retained soft edges contain a cycle.
    #[error(
        "Feature dependency cycle: {cycle}; remove a dependsOn/installsAfter edge or change the requested Features"
    )]
    DependencyCycle {
        /// Closed identity path demonstrating the cycle.
        cycle: String,
    },
    /// A lock domain value is malformed.
    #[error("invalid Feature lock value: {message}")]
    InvalidLock {
        /// Validation detail.
        message: &'static str,
    },
    /// A frozen lock differs from the current configuration or verified resolution.
    #[error("Feature lockfile is stale or inconsistent; run `cdenv lock`")]
    StaleLock,
}

fn render_path(path: &[FeatureReference]) -> String {
    path.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" -> ")
}

#[cfg(test)]
mod test_support {
    use super::*;
    use serde_json::Value;

    pub(super) fn reference(id: &str) -> FeatureReference {
        FeatureReference::parse(&format!("./features/{id}")).expect("valid local reference")
    }

    #[expect(
        clippy::needless_pass_by_value,
        reason = "keeps JSON fixture calls concise"
    )]
    pub(super) fn package(id: &str, metadata: Value) -> FeaturePackage {
        let reference = reference(id);
        FeaturePackage {
            identity: FeatureInstallIdentity::local(&reference),
            reference,
            metadata: FeatureMetadata::from_value(&metadata).expect("valid fixture metadata"),
            digest: None,
            integrity: None,
        }
    }

    pub(super) fn catalog(
        packages: Vec<FeaturePackage>,
    ) -> BTreeMap<FeatureReference, FeaturePackage> {
        packages
            .into_iter()
            .map(|package| (package.reference.clone(), package))
            .collect()
    }

    pub(super) fn ids(result: &ResolvedFeatures) -> Vec<&str> {
        result
            .installation_order
            .iter()
            .map(|feature| feature.metadata.id.as_str())
            .collect()
    }
}
