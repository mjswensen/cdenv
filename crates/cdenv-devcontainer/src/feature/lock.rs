//! Frozen-lock construction, strict decoding, canonical encoding, and staleness checks.
//! Consumes resolution values without running the graph or accessing sources.

#[cfg(test)]
mod boundary_tests;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::reference::{oci_resource_name, validate_sha256};
use super::{
    FeatureError, FeaturePackage, FeatureReference, FeatureRequest, FeatureValue, ResolvedFeatures,
};

/// Current deterministic Feature lockfile schema.
pub const FEATURE_LOCK_VERSION: u32 = 1;

/// Frozen Feature resolution, including roots and recursive dependencies.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FeatureLock {
    /// Lock schema version.
    pub version: u32,
    /// Configured root requests and their explicit options.
    pub requested: BTreeMap<String, BTreeMap<String, FeatureValue>>,
    /// All resolved records keyed by normalized request reference.
    pub features: BTreeMap<String, LockedFeature>,
}

impl Default for FeatureLock {
    fn default() -> Self {
        Self {
            version: FEATURE_LOCK_VERSION,
            requested: BTreeMap::new(),
            features: BTreeMap::new(),
        }
    }
}

impl FeatureLock {
    /// Builds a complete lock from one pure resolution and its verified package catalog.
    ///
    /// # Errors
    /// Returns [`FeatureError::InvalidLock`] when resolution data is incomplete.
    pub fn from_resolution(
        requests: &[FeatureRequest],
        resolved: &ResolvedFeatures,
        packages: &BTreeMap<FeatureReference, FeaturePackage>,
    ) -> Result<Self, FeatureError> {
        let requested = requests
            .iter()
            .map(|request| (request.reference.to_string(), request.options.clone()))
            .collect();
        let mut features = BTreeMap::new();
        for feature in &resolved.installation_order {
            let package = packages
                .get(&feature.reference)
                .ok_or(FeatureError::InvalidLock {
                    message: "resolved Feature is absent from its package catalog",
                })?;
            let integrity = package.integrity.clone().ok_or(FeatureError::InvalidLock {
                message: "resolved Feature has no content integrity",
            })?;
            validate_sha256(&integrity).map_err(|message| FeatureError::InvalidLock { message })?;
            let resolved_reference = package.digest.as_ref().map_or_else(
                || package.reference.to_string(),
                |digest| format!("{}@{digest}", oci_resource_name(package.reference.as_str())),
            );
            let depends_on = package
                .metadata
                .depends_on
                .iter()
                .map(|(reference, options)| (reference.to_string(), options.clone()))
                .collect();
            features.insert(
                feature.reference.to_string(),
                LockedFeature::new(
                    feature.metadata.version.clone(),
                    resolved_reference,
                    integrity,
                    feature.options.clone(),
                    depends_on,
                )?,
            );
        }
        Ok(Self {
            version: FEATURE_LOCK_VERSION,
            requested,
            features,
        })
    }

    /// Parses and structurally validates lockfile JSON.
    ///
    /// # Errors
    /// Returns [`FeatureError::InvalidLock`] for malformed, newer, or inconsistent content.
    pub fn parse(bytes: &[u8]) -> Result<Self, FeatureError> {
        let lock: Self = serde_json::from_slice(bytes).map_err(|_| FeatureError::InvalidLock {
            message: "lockfile is not valid strict JSON",
        })?;
        lock.validate_structure()?;
        Ok(lock)
    }

    /// Encodes deterministic pretty JSON with one trailing newline.
    ///
    /// # Errors
    /// Returns [`FeatureError::InvalidLock`] if serialization unexpectedly fails.
    pub fn to_json_bytes(&self) -> Result<Vec<u8>, FeatureError> {
        self.validate_structure()?;
        let mut bytes = serde_json::to_vec_pretty(self).map_err(|_| FeatureError::InvalidLock {
            message: "lockfile cannot be serialized",
        })?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Compares a frozen lock with a newly verified resolution.
    ///
    /// # Errors
    /// Returns [`FeatureError::StaleLock`] when configuration, dependencies, options, versions,
    /// digests, or integrity changed.
    pub fn validate_frozen(
        &self,
        requests: &[FeatureRequest],
        resolved: &ResolvedFeatures,
        packages: &BTreeMap<FeatureReference, FeaturePackage>,
    ) -> Result<(), FeatureError> {
        let current = Self::from_resolution(requests, resolved, packages)?;
        if *self == current {
            Ok(())
        } else {
            Err(FeatureError::StaleLock)
        }
    }

    /// Checks only configured root requests without source access.
    ///
    /// This is suitable for existing-container `up`: a mismatch can be warned about without
    /// contacting a registry, while create/rebuild still use [`Self::validate_frozen`].
    ///
    /// # Errors
    /// Returns [`FeatureError::StaleLock`] when root references or explicit options differ.
    pub fn validate_requested(&self, requests: &[FeatureRequest]) -> Result<(), FeatureError> {
        let requested = requests
            .iter()
            .map(|request| (request.reference.to_string(), request.options.clone()))
            .collect::<BTreeMap<_, _>>();
        if requested == self.requested {
            Ok(())
        } else {
            Err(FeatureError::StaleLock)
        }
    }

    fn validate_structure(&self) -> Result<(), FeatureError> {
        if self.version != FEATURE_LOCK_VERSION {
            return Err(FeatureError::InvalidLock {
                message: "unsupported lockfile version",
            });
        }
        for record in self.features.values() {
            if record.version.is_empty() || record.resolved.is_empty() {
                return Err(FeatureError::InvalidLock {
                    message: "lock record has an empty version or resolution",
                });
            }
            validate_sha256(&record.integrity)
                .map_err(|message| FeatureError::InvalidLock { message })?;
        }
        Ok(())
    }
}

/// One exact Feature lock record.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedFeature {
    /// Exact metadata version.
    pub version: String,
    /// Digest-qualified OCI reference, HTTPS URL, or contained local reference.
    pub resolved: String,
    /// SHA-256 of the install content.
    pub integrity: String,
    /// Effective options including defaults.
    pub options: BTreeMap<String, FeatureValue>,
    /// Hard dependencies and their requested options.
    pub depends_on: BTreeMap<String, BTreeMap<String, FeatureValue>>,
}

impl LockedFeature {
    /// Validates and creates a lock record without filesystem I/O.
    ///
    /// # Errors
    /// Returns [`FeatureError::InvalidLock`] when required values are invalid.
    pub fn new(
        version: String,
        resolved: String,
        integrity: String,
        options: BTreeMap<String, FeatureValue>,
        depends_on: BTreeMap<String, BTreeMap<String, FeatureValue>>,
    ) -> Result<Self, FeatureError> {
        if version.is_empty() || resolved.is_empty() {
            return Err(FeatureError::InvalidLock {
                message: "lock version and resolution cannot be empty",
            });
        }
        validate_sha256(&integrity).map_err(|message| FeatureError::InvalidLock { message })?;
        Ok(Self {
            version,
            resolved,
            integrity,
            options,
            depends_on,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::resolve_features;
    use super::super::test_support::{catalog, package, reference};
    use super::*;
    use serde_json::json;

    #[test]
    fn lock_round_trip_is_byte_deterministic_and_preserves_options() {
        let package = package(
            "tool",
            json!({
                "id":"tool", "version":"1.2.3",
                "options":{"enabled":{"type":"boolean","default":true}}
            }),
        );
        let mut package = package;
        package.integrity = Some(format!("sha256:{}", "a".repeat(64)));
        let packages = catalog(vec![package]);
        let requests = vec![FeatureRequest::new(reference("tool"))];
        let resolved = resolve_features(&requests, &packages, &[]).expect("resolution");
        let lock = FeatureLock::from_resolution(&requests, &resolved, &packages).expect("lock");
        let first = lock.to_json_bytes().expect("JSON");
        let parsed = FeatureLock::parse(&first).expect("parse");
        assert_eq!(first, parsed.to_json_bytes().expect("same JSON"));
        assert_eq!(parsed, lock);
    }
}
