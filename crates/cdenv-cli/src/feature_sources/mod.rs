//! Secure host-side retrieval, caching, and extraction of Dev Container Features.

use std::io;
use std::path::{Path, PathBuf};

use cdenv_devcontainer::{
    FeatureError, FeatureInstallIdentity, FeaturePackage, FeatureReference, LockedFeature,
};
use reqwest::Client;
use thiserror::Error;

/// Maximum redirects followed for Feature requests.
pub const MAX_FEATURE_REDIRECTS: usize = 5;
/// Maximum compressed Feature archive size (64 MiB; V1 support contract).
pub const MAX_FEATURE_BLOB_BYTES: u64 = 64 * 1024 * 1024;
/// Maximum manifest or token response size (1 MiB).
pub const MAX_FEATURE_METADATA_BYTES: u64 = 1024 * 1024;
/// Maximum files and directories accepted from one Feature (4096; V1 support contract).
pub const MAX_FEATURE_FILES: usize = 4096;
/// Maximum bytes extracted from one Feature (128 MiB; V1 support contract).
pub const MAX_FEATURE_EXTRACTED_BYTES: u64 = 128 * 1024 * 1024;
/// Maximum archive expansion relative to its compressed size.
pub const MAX_FEATURE_EXPANSION_RATIO: u64 = 100;
/// Maximum UTF-8 archive path length.
pub const MAX_FEATURE_PATH_BYTES: usize = 4096;

const OCI_MANIFEST: &str = "application/vnd.oci.image.manifest.v1+json";
const DOCKER_MANIFEST: &str = "application/vnd.docker.distribution.manifest.v2+json";
const OCI_INDEX: &str = "application/vnd.oci.image.index.v1+json";
const DOCKER_LIST: &str = "application/vnd.docker.distribution.manifest.list.v2+json";
const FEATURE_LAYER: &str = "application/vnd.devcontainers.layer.v1+tar";
const FEATURE_LAYER_GZIP: &str = "application/vnd.devcontainers.layer.v1+tar+gzip";

/// Resource limits for source retrieval and archive extraction.
#[derive(Clone, Copy, Debug)]
pub struct FeatureSourceLimits {
    /// Redirect count.
    pub redirects: usize,
    /// Downloaded blob bytes.
    pub blob_bytes: u64,
    /// Manifest/token bytes.
    pub metadata_bytes: u64,
    /// Archive entry count.
    pub files: usize,
    /// Extracted regular-file bytes.
    pub extracted_bytes: u64,
    /// Expansion ratio.
    pub expansion_ratio: u64,
    /// Archive path bytes.
    pub path_bytes: usize,
}

impl Default for FeatureSourceLimits {
    fn default() -> Self {
        Self {
            redirects: MAX_FEATURE_REDIRECTS,
            blob_bytes: MAX_FEATURE_BLOB_BYTES,
            metadata_bytes: MAX_FEATURE_METADATA_BYTES,
            files: MAX_FEATURE_FILES,
            extracted_bytes: MAX_FEATURE_EXTRACTED_BYTES,
            expansion_ratio: MAX_FEATURE_EXPANSION_RATIO,
            path_bytes: MAX_FEATURE_PATH_BYTES,
        }
    }
}

/// A verified Feature archive and its parsed metadata.
#[derive(Clone, Debug)]
pub struct VerifiedFeature {
    /// Package input for the pure Feature planner.
    pub package: FeaturePackage,
    /// Verified cache artifact/extraction directory, or the local source directory.
    pub artifact: PathBuf,
}

/// Layered failure from Feature transport, cache, or extraction.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FeatureSourceError {
    /// Pure reference or metadata validation failed.
    #[error(transparent)]
    Feature(#[from] FeatureError),
    /// A URL or redirect violates the public verified-HTTPS policy.
    #[error("unsafe Feature URL `{url}`: {message}")]
    Url {
        /// Rejected URL.
        url: String,
        /// Policy violation.
        message: &'static str,
    },
    /// HTTP transport failed.
    #[error("Feature request failed for {url}: {source}")]
    Transport {
        /// Requested URL.
        url: String,
        /// Transport failure.
        #[source]
        source: reqwest::Error,
    },
    /// Registry authentication is private, malformed, or unsuccessful.
    #[error("public OCI authentication failed: {0}")]
    Authentication(String),
    /// Registry response is invalid.
    #[error("invalid OCI response: {0}")]
    Oci(String),
    /// A configured I/O bound was exceeded.
    #[error("Feature {kind} exceeds its limit of {limit}")]
    Limit {
        /// Bounded resource.
        kind: &'static str,
        /// Configured maximum.
        limit: u64,
    },
    /// Downloaded or cached content did not match its SHA-256 digest.
    #[error("Feature digest mismatch: expected {expected}, observed {actual}")]
    Digest {
        /// Required digest.
        expected: String,
        /// Computed digest.
        actual: String,
    },
    /// Cache filesystem operation failed.
    #[error("Feature cache operation failed for {path:?}: {source}")]
    Cache {
        /// Cache path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Local Feature containment failed.
    #[error("local Feature is not a contained regular directory: {path:?}")]
    LocalContainment {
        /// Rejected source path.
        path: PathBuf,
    },
    /// Archive input is malformed or unsafe.
    #[error("unsafe Feature archive entry {path:?}: {message}")]
    Archive {
        /// Rejected entry path.
        path: PathBuf,
        /// Rejection reason.
        message: &'static str,
    },
    /// Archive I/O failed.
    #[error("cannot process Feature archive {path:?}: {source}")]
    ArchiveIo {
        /// Archive or output path.
        path: PathBuf,
        /// I/O failure.
        #[source]
        source: io::Error,
    },
    /// `devcontainer-feature.json` is absent or unreadable.
    #[error("cannot read Feature metadata {path:?}: {source}")]
    Metadata {
        /// Metadata path.
        path: PathBuf,
        /// I/O failure.
        #[source]
        source: io::Error,
    },
    /// Feature metadata JSON is invalid.
    #[error("invalid Feature metadata JSON in {path:?}: {source}")]
    MetadataJson {
        /// Metadata path.
        path: PathBuf,
        /// JSON failure.
        #[source]
        source: serde_json::Error,
    },
}

/// Public, credential-free Feature source resolver.
#[derive(Clone, Debug)]
pub struct FeatureSourceResolver {
    client: Client,
    blobs: PathBuf,
    limits: FeatureSourceLimits,
}

mod archive;
mod cache;
mod http;
mod local;
mod metadata;
mod oci;

pub use archive::extract_archive;

use self::{
    cache::{cache_path, extraction_directory, verify_cached},
    local::resolve_local,
    metadata::read_feature_metadata,
};

impl FeatureSourceResolver {
    /// Creates a resolver using rustls verification and manual bounded redirects.
    ///
    /// # Errors
    /// Returns a transport error if the HTTP client cannot be constructed.
    pub fn new(blobs: impl Into<PathBuf>) -> Result<Self, FeatureSourceError> {
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|source| FeatureSourceError::Transport {
                url: "<client>".to_owned(),
                source,
            })?;
        Ok(Self {
            client,
            blobs: blobs.into(),
            limits: FeatureSourceLimits::default(),
        })
    }

    /// Overrides resource limits, primarily for focused tests.
    #[must_use]
    pub const fn with_limits(mut self, limits: FeatureSourceLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Resolves one normalized source. Network artifacts are returned as verified cache blobs;
    /// local sources are canonicalized below `configuration_directory`.
    ///
    /// # Errors
    /// Returns a typed transport, cache, containment, digest, or metadata failure.
    pub async fn resolve(
        &self,
        reference: &FeatureReference,
        configuration_directory: &Path,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        match reference {
            FeatureReference::Local(value) => {
                resolve_local(reference, value, configuration_directory, self.limits)
            }
            FeatureReference::Https(value) => self.resolve_https(reference, value).await,
            FeatureReference::Oci(value) => self.resolve_oci(reference, value).await,
        }
    }

    /// Resolves a frozen record from verified cache content without network access.
    ///
    /// Local records are re-read and compared by integrity. OCI and HTTPS records require the
    /// exact digest-addressed blob to exist and pass a fresh digest check.
    ///
    /// # Errors
    /// Returns a cache, integrity, metadata, or frozen-record mismatch.
    pub fn resolve_locked(
        &self,
        reference: &FeatureReference,
        record: &LockedFeature,
        configuration_directory: &Path,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        if matches!(reference, FeatureReference::Local(_)) {
            let verified = resolve_local(
                reference,
                reference.as_str(),
                configuration_directory,
                self.limits,
            )?;
            if verified.package.integrity.as_deref() != Some(&record.integrity)
                || verified.package.metadata.version != record.version
            {
                return Err(FeatureSourceError::Oci(
                    "local Feature differs from frozen lock".to_owned(),
                ));
            }
            return Ok(verified);
        }
        let path = cache_path(&self.blobs, &record.integrity);
        if !verify_cached(&path, &record.integrity, None)? {
            return Err(FeatureSourceError::Cache {
                path,
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "frozen Feature blob is not cached",
                ),
            });
        }
        let extracted = extraction_directory(&self.blobs, &record.integrity);
        extract_archive(&path, &extracted, self.limits)?;
        let metadata = read_feature_metadata(&extracted, self.limits.metadata_bytes)?;
        if metadata.version != record.version {
            return Err(FeatureSourceError::Oci(
                "cached Feature version differs from frozen lock".to_owned(),
            ));
        }
        let (identity, digest) = match reference {
            FeatureReference::Oci(_) => {
                let digest = record
                    .resolved
                    .rsplit_once('@')
                    .map(|(_, digest)| digest.to_owned())
                    .ok_or_else(|| {
                        FeatureSourceError::Oci("frozen OCI resolution has no digest".to_owned())
                    })?;
                (
                    FeatureInstallIdentity::oci_digest(digest.clone())?,
                    Some(digest),
                )
            }
            FeatureReference::Https(_) => (
                FeatureInstallIdentity::https_integrity(record.integrity.clone())?,
                None,
            ),
            FeatureReference::Local(_) => unreachable!("local returned above"),
        };
        Ok(VerifiedFeature {
            package: FeaturePackage {
                reference: reference.clone(),
                identity,
                metadata,
                digest,
                integrity: Some(record.integrity.clone()),
            },
            artifact: extracted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_published_v1_feature_source_limits() {
        let limits = FeatureSourceLimits::default();
        assert_eq!(limits.blob_bytes, 64 * 1024 * 1024);
        assert_eq!(limits.extracted_bytes, 128 * 1024 * 1024);
        assert_eq!(limits.files, 4096);
        assert_eq!(limits.metadata_bytes, 1024 * 1024);
        assert!(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../docs/cdenv-devcontainer-v1-support.md"
        ))
        .contains(
            "feature-source-limits: compressed=67108864 expanded=134217728 entries=4096 metadata=1048576"
        ));
    }
}
