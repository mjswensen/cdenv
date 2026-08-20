//! Secure host-side retrieval, caching, and extraction of Dev Container Features.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use cdenv_devcontainer::{
    FeatureError, FeatureInstallIdentity, FeatureMetadata, FeaturePackage, FeatureReference,
    LockedFeature,
};
use futures_util::StreamExt;
use reqwest::header::{
    ACCEPT, AUTHORIZATION, CONTENT_LENGTH, CONTENT_TYPE, LOCATION, WWW_AUTHENTICATE,
};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;

/// Maximum redirects followed for Feature requests.
pub const MAX_FEATURE_REDIRECTS: usize = 5;
/// Maximum downloaded blob size (256 MiB).
pub const MAX_FEATURE_BLOB_BYTES: u64 = 256 * 1024 * 1024;
/// Maximum manifest or token response size (1 MiB).
pub const MAX_FEATURE_METADATA_BYTES: u64 = 1024 * 1024;
/// Maximum files and directories accepted from one Feature.
pub const MAX_FEATURE_FILES: usize = 10_000;
/// Maximum bytes extracted from one Feature (1 GiB).
pub const MAX_FEATURE_EXTRACTED_BYTES: u64 = 1024 * 1024 * 1024;
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
    /// Verified digest-addressed archive in the cache, or the local source directory.
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
                self.resolve_local(reference, value, configuration_directory)
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
            let verified =
                self.resolve_local(reference, reference.as_str(), configuration_directory)?;
            if verified.package.integrity.as_deref() != Some(&record.integrity)
                || verified.package.metadata.version != record.version
            {
                return Err(FeatureSourceError::Oci(
                    "local Feature differs from frozen lock".to_owned(),
                ));
            }
            return Ok(verified);
        }
        let path = self.cache_path(&record.integrity);
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
            artifact: path,
        })
    }

    fn resolve_local(
        &self,
        reference: &FeatureReference,
        value: &str,
        base: &Path,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        let canonical_base =
            fs::canonicalize(base).map_err(|source| FeatureSourceError::Cache {
                path: base.to_path_buf(),
                source,
            })?;
        let candidate = base.join(value.strip_prefix("./").unwrap_or(value));
        let source =
            fs::canonicalize(&candidate).map_err(|_| FeatureSourceError::LocalContainment {
                path: candidate.clone(),
            })?;
        let metadata =
            fs::symlink_metadata(&source).map_err(|_| FeatureSourceError::LocalContainment {
                path: source.clone(),
            })?;
        if !source.starts_with(&canonical_base)
            || !metadata.is_dir()
            || metadata.file_type().is_symlink()
        {
            return Err(FeatureSourceError::LocalContainment { path: source });
        }
        validate_local_tree(&source, &canonical_base, self.limits)?;
        let feature_metadata = read_feature_metadata(&source, self.limits.metadata_bytes)?;
        let integrity = hash_local_tree(&source)?;
        Ok(VerifiedFeature {
            package: FeaturePackage {
                reference: reference.clone(),
                identity: FeatureInstallIdentity::local(reference),
                metadata: feature_metadata,
                digest: None,
                integrity: Some(integrity),
            },
            artifact: source,
        })
    }

    async fn resolve_https(
        &self,
        reference: &FeatureReference,
        value: &str,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        let url = checked_https_url(value)?;
        let response = self.get_following(url, None, None).await?;
        require_success(&response)?;
        let (path, digest, _) = self.cache_response(response, None, None).await?;
        let extracted = extraction_directory(&self.blobs, &digest);
        extract_archive(&path, &extracted, self.limits)?;
        let metadata = read_feature_metadata(&extracted, self.limits.metadata_bytes)?;
        Ok(VerifiedFeature {
            package: FeaturePackage {
                reference: reference.clone(),
                identity: FeatureInstallIdentity::https_integrity(digest.clone())?,
                metadata,
                digest: None,
                integrity: Some(digest),
            },
            artifact: path,
        })
    }

    async fn resolve_oci(
        &self,
        reference: &FeatureReference,
        value: &str,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        let parsed = OciReference::parse(value)?;
        let accept = format!("{OCI_MANIFEST}, {DOCKER_MANIFEST}, {OCI_INDEX}, {DOCKER_LIST}");
        let mut manifest_url = parsed.manifest_url(&parsed.selector)?;
        let mut response = self
            .get_following(manifest_url.clone(), Some(&accept), None)
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            let challenge = response
                .headers()
                .get(WWW_AUTHENTICATE)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| {
                    FeatureSourceError::Authentication(
                        "registry requires unsupported private authentication".to_owned(),
                    )
                })?;
            let token = self.bearer_token(challenge).await?;
            response = self
                .get_following(manifest_url.clone(), Some(&accept), Some(&token))
                .await?;
        }
        require_success(&response)?;
        let (mut manifest, mut manifest_digest, media) =
            read_manifest(response, self.limits.metadata_bytes).await?;
        if matches!(media.as_str(), OCI_INDEX | DOCKER_LIST) {
            let index: IndexDocument = serde_json::from_slice(&manifest)
                .map_err(|error| FeatureSourceError::Oci(error.to_string()))?;
            let descriptor = index
                .manifests
                .into_iter()
                .find(|item| matches!(item.media_type.as_str(), OCI_MANIFEST | DOCKER_MANIFEST))
                .ok_or_else(|| {
                    FeatureSourceError::Oci(
                        "manifest list has no supported image manifest".to_owned(),
                    )
                })?;
            validate_digest(&descriptor.digest)?;
            manifest_url = parsed.manifest_url(&descriptor.digest)?;
            let selected = self
                .get_following(manifest_url, Some(&accept), None)
                .await?;
            require_success(&selected)?;
            let result = read_manifest(selected, self.limits.metadata_bytes).await?;
            manifest = result.0;
            manifest_digest = result.1;
        }
        if let Some(expected) = parsed.digest.as_deref() {
            verify_digest(expected, &manifest)?;
        }
        let document: ManifestDocument = serde_json::from_slice(&manifest)
            .map_err(|error| FeatureSourceError::Oci(error.to_string()))?;
        let layer = document
            .layers
            .into_iter()
            .find(|layer| {
                matches!(
                    layer.media_type.as_str(),
                    FEATURE_LAYER
                        | FEATURE_LAYER_GZIP
                        | "application/vnd.oci.image.layer.v1.tar"
                        | "application/vnd.oci.image.layer.v1.tar+gzip"
                )
            })
            .ok_or_else(|| {
                FeatureSourceError::Oci("manifest has no supported Feature layer".to_owned())
            })?;
        validate_digest(&layer.digest)?;
        let cached = self.cache_path(&layer.digest);
        let path = if verify_cached(&cached, &layer.digest, Some(layer.size))? {
            cached
        } else {
            let response = self
                .get_following(parsed.blob_url(&layer.digest)?, None, None)
                .await?;
            require_success(&response)?;
            self.cache_response(response, Some(&layer.digest), Some(layer.size))
                .await?
                .0
        };
        let extracted = extraction_directory(&self.blobs, &layer.digest);
        extract_archive(&path, &extracted, self.limits)?;
        let metadata = read_feature_metadata(&extracted, self.limits.metadata_bytes)?;
        Ok(VerifiedFeature {
            package: FeaturePackage {
                reference: reference.clone(),
                identity: FeatureInstallIdentity::oci_digest(manifest_digest.clone())?,
                metadata,
                digest: Some(manifest_digest),
                integrity: Some(layer.digest),
            },
            artifact: path,
        })
    }

    async fn bearer_token(&self, challenge: &str) -> Result<String, FeatureSourceError> {
        let fields = parse_bearer_challenge(challenge)?;
        let realm = checked_https_url(fields.realm)?;
        let mut request = self.client.get(realm.clone());
        if let Some(service) = fields.service {
            request = request.query(&[("service", service)]);
        }
        if let Some(scope) = fields.scope {
            request = request.query(&[("scope", scope)]);
        }
        let response = request
            .send()
            .await
            .map_err(|source| FeatureSourceError::Transport {
                url: realm.to_string(),
                source,
            })?;
        require_success(&response)?;
        let bytes = read_response(response, self.limits.metadata_bytes).await?;
        let token: TokenDocument = serde_json::from_slice(&bytes)
            .map_err(|error| FeatureSourceError::Authentication(error.to_string()))?;
        token
            .token
            .or(token.access_token)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| {
                FeatureSourceError::Authentication("token response omitted token".to_owned())
            })
    }

    async fn get_following(
        &self,
        mut url: Url,
        accept: Option<&str>,
        token: Option<&str>,
    ) -> Result<Response, FeatureSourceError> {
        for followed in 0..=self.limits.redirects {
            let mut request = self.client.get(url.clone());
            if let Some(value) = accept {
                request = request.header(ACCEPT, value);
            }
            if let Some(value) = token {
                request = request.header(AUTHORIZATION, format!("Bearer {value}"));
            }
            let response =
                request
                    .send()
                    .await
                    .map_err(|source| FeatureSourceError::Transport {
                        url: url.to_string(),
                        source,
                    })?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            if followed == self.limits.redirects {
                return Err(FeatureSourceError::Limit {
                    kind: "redirect count",
                    limit: self.limits.redirects as u64,
                });
            }
            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or_else(|| FeatureSourceError::Url {
                    url: url.to_string(),
                    message: "redirect omitted a valid Location",
                })?;
            url = checked_https_url(
                url.join(location)
                    .map_err(|_| FeatureSourceError::Url {
                        url: location.to_owned(),
                        message: "invalid redirect Location",
                    })?
                    .as_str(),
            )?;
        }
        unreachable!("redirect loop always returns")
    }

    async fn cache_response(
        &self,
        response: Response,
        expected_digest: Option<&str>,
        expected_size: Option<u64>,
    ) -> Result<(PathBuf, String, u64), FeatureSourceError> {
        if let Some(length) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            && length > self.limits.blob_bytes
        {
            return Err(FeatureSourceError::Limit {
                kind: "download bytes",
                limit: self.limits.blob_bytes,
            });
        }
        fs::create_dir_all(&self.blobs).map_err(|source| FeatureSourceError::Cache {
            path: self.blobs.clone(),
            source,
        })?;
        let temporary = self.blobs.join(format!(".download-{}", random_suffix()?));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| FeatureSourceError::Cache {
                path: temporary.clone(),
                source,
            })?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| FeatureSourceError::Transport {
                url: "<response-body>".to_owned(),
                source,
            })?;
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or(FeatureSourceError::Limit {
                    kind: "download bytes",
                    limit: self.limits.blob_bytes,
                })?;
            if size > self.limits.blob_bytes {
                let _ = fs::remove_file(&temporary);
                return Err(FeatureSourceError::Limit {
                    kind: "download bytes",
                    limit: self.limits.blob_bytes,
                });
            }
            hash.update(&chunk);
            output
                .write_all(&chunk)
                .map_err(|source| FeatureSourceError::Cache {
                    path: temporary.clone(),
                    source,
                })?;
        }
        output
            .sync_all()
            .map_err(|source| FeatureSourceError::Cache {
                path: temporary.clone(),
                source,
            })?;
        let digest = format!("sha256:{}", hex::encode(hash.finalize()));
        if let Some(expected) = expected_digest
            && digest != expected
        {
            let _ = fs::remove_file(&temporary);
            return Err(FeatureSourceError::Digest {
                expected: expected.to_owned(),
                actual: digest,
            });
        }
        if let Some(expected) = expected_size
            && size != expected
        {
            let _ = fs::remove_file(&temporary);
            return Err(FeatureSourceError::Oci(format!(
                "blob size mismatch: expected {expected}, observed {size}"
            )));
        }
        let destination = self.cache_path(&digest);
        if destination.exists() {
            let _ = fs::remove_file(&temporary);
            verify_cached(&destination, &digest, Some(size))?;
        } else if let Err(source) = fs::rename(&temporary, &destination) {
            if destination.exists() {
                let _ = fs::remove_file(&temporary);
                verify_cached(&destination, &digest, Some(size))?;
            } else {
                return Err(FeatureSourceError::Cache {
                    path: destination,
                    source,
                });
            }
        }
        Ok((destination, digest, size))
    }

    fn cache_path(&self, digest: &str) -> PathBuf {
        self.blobs.join(digest.trim_start_matches("sha256:"))
    }
}

#[derive(Debug)]
struct OciReference<'a> {
    registry: &'a str,
    repository: &'a str,
    selector: String,
    digest: Option<String>,
}
impl<'a> OciReference<'a> {
    fn parse(value: &'a str) -> Result<Self, FeatureSourceError> {
        let (name, digest) = value
            .split_once('@')
            .map_or((value, None), |(name, digest)| {
                (name, Some(digest.to_owned()))
            });
        let slash = name
            .find('/')
            .ok_or_else(|| FeatureSourceError::Oci("reference has no repository".to_owned()))?;
        let registry = &name[..slash];
        let remainder = &name[slash + 1..];
        let (repository, tag) = remainder
            .rsplit_once(':')
            .filter(|(left, _)| !left.ends_with('/'))
            .map_or((remainder, "latest"), |(repo, tag)| (repo, tag));
        Ok(Self {
            registry,
            repository,
            selector: digest.clone().unwrap_or_else(|| tag.to_owned()),
            digest,
        })
    }
    fn manifest_url(&self, selector: &str) -> Result<Url, FeatureSourceError> {
        checked_https_url(&format!(
            "https://{}/v2/{}/manifests/{selector}",
            self.registry, self.repository
        ))
    }
    fn blob_url(&self, digest: &str) -> Result<Url, FeatureSourceError> {
        checked_https_url(&format!(
            "https://{}/v2/{}/blobs/{digest}",
            self.registry, self.repository
        ))
    }
}

#[derive(Deserialize)]
struct ManifestDocument {
    layers: Vec<Descriptor>,
}
#[derive(Deserialize)]
struct IndexDocument {
    manifests: Vec<Descriptor>,
}
#[derive(Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
}
#[derive(Deserialize)]
struct TokenDocument {
    token: Option<String>,
    access_token: Option<String>,
}
struct BearerFields<'a> {
    realm: &'a str,
    service: Option<&'a str>,
    scope: Option<&'a str>,
}

fn parse_bearer_challenge(value: &str) -> Result<BearerFields<'_>, FeatureSourceError> {
    let body = value
        .strip_prefix("Bearer ")
        .or_else(|| value.strip_prefix("bearer "))
        .ok_or_else(|| {
            FeatureSourceError::Authentication(
                "registry did not offer anonymous Bearer authentication".to_owned(),
            )
        })?;
    let mut realm = None;
    let mut service = None;
    let mut scope = None;
    for field in body.split(',') {
        let (name, value) = field.trim().split_once('=').ok_or_else(|| {
            FeatureSourceError::Authentication("malformed Bearer challenge".to_owned())
        })?;
        let value = value
            .trim()
            .strip_prefix('"')
            .and_then(|v| v.strip_suffix('"'))
            .ok_or_else(|| {
                FeatureSourceError::Authentication("malformed Bearer challenge value".to_owned())
            })?;
        match name {
            "realm" => realm = Some(value),
            "service" => service = Some(value),
            "scope" => scope = Some(value),
            _ => {}
        }
    }
    Ok(BearerFields {
        realm: realm.ok_or_else(|| {
            FeatureSourceError::Authentication("Bearer challenge omitted realm".to_owned())
        })?,
        service,
        scope,
    })
}

async fn read_manifest(
    response: Response,
    limit: u64,
) -> Result<(Vec<u8>, String, String), FeatureSourceError> {
    let media = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or(OCI_MANIFEST)
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if !matches!(
        media.as_str(),
        OCI_MANIFEST | DOCKER_MANIFEST | OCI_INDEX | DOCKER_LIST
    ) {
        return Err(FeatureSourceError::Oci(format!(
            "unsupported manifest media type `{media}`"
        )));
    }
    let bytes = read_response(response, limit).await?;
    let digest = digest_bytes(&bytes);
    Ok((bytes, digest, media))
}
async fn read_response(response: Response, limit: u64) -> Result<Vec<u8>, FeatureSourceError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Err(FeatureSourceError::Limit {
            kind: "response bytes",
            limit,
        });
    }
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| FeatureSourceError::Transport {
            url: "<response-body>".to_owned(),
            source,
        })?;
        if (bytes.len() as u64).saturating_add(chunk.len() as u64) > limit {
            return Err(FeatureSourceError::Limit {
                kind: "response bytes",
                limit,
            });
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn require_success(response: &Response) -> Result<(), FeatureSourceError> {
    if response.status().is_success() {
        Ok(())
    } else if matches!(
        response.status(),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
    ) {
        Err(FeatureSourceError::Authentication(format!(
            "registry returned {}",
            response.status()
        )))
    } else {
        Err(FeatureSourceError::Oci(format!(
            "server returned {}",
            response.status()
        )))
    }
}

fn checked_https_url(value: &str) -> Result<Url, FeatureSourceError> {
    let url = Url::parse(value).map_err(|_| FeatureSourceError::Url {
        url: value.to_owned(),
        message: "invalid URL",
    })?;
    if url.scheme() != "https" {
        return Err(FeatureSourceError::Url {
            url: value.to_owned(),
            message: "verified HTTPS is required",
        });
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FeatureSourceError::Url {
            url: value.to_owned(),
            message: "URL credentials are forbidden",
        });
    }
    let host = url.host_str().ok_or_else(|| FeatureSourceError::Url {
        url: value.to_owned(),
        message: "public host is required",
    })?;
    let lowercase = host.to_ascii_lowercase();
    let private_name = lowercase == "localhost"
        || lowercase.ends_with(".localhost")
        || lowercase.strip_suffix(".local").is_some()
        || !lowercase.contains('.');
    let private_ip = lowercase
        .parse::<IpAddr>()
        .is_ok_and(|address| match address {
            IpAddr::V4(value) => value.is_private() || value.is_loopback() || value.is_link_local(),
            IpAddr::V6(value) => {
                value.is_loopback() || value.is_unique_local() || value.is_unicast_link_local()
            }
        });
    if private_name || private_ip {
        return Err(FeatureSourceError::Url {
            url: value.to_owned(),
            message: "private or local hosts are forbidden",
        });
    }
    Ok(url)
}
fn validate_digest(value: &str) -> Result<(), FeatureSourceError> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| FeatureSourceError::Oci("only SHA-256 digests are supported".to_owned()))?;
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(FeatureSourceError::Oci(
            "malformed SHA-256 digest".to_owned(),
        ))
    }
}
fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
fn verify_digest(expected: &str, bytes: &[u8]) -> Result<(), FeatureSourceError> {
    let actual = digest_bytes(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(FeatureSourceError::Digest {
            expected: expected.to_owned(),
            actual,
        })
    }
}
fn verify_cached(path: &Path, digest: &str, size: Option<u64>) -> Result<bool, FeatureSourceError> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(FeatureSourceError::Cache {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !file
        .metadata()
        .map_err(|source| FeatureSourceError::Cache {
            path: path.to_path_buf(),
            source,
        })?
        .is_file()
    {
        return Err(FeatureSourceError::Cache {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidData, "cache entry is not regular"),
        });
    }
    let mut reader = BufReader::new(file);
    let mut hash = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|source| FeatureSourceError::Cache {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        observed += count as u64;
        hash.update(&buffer[..count]);
    }
    let actual = format!("sha256:{}", hex::encode(hash.finalize()));
    if actual != digest {
        return Err(FeatureSourceError::Digest {
            expected: digest.to_owned(),
            actual,
        });
    }
    if size.is_some_and(|expected| expected != observed) {
        return Err(FeatureSourceError::Oci(
            "cached blob size mismatch".to_owned(),
        ));
    }
    Ok(true)
}
fn random_suffix() -> Result<String, FeatureSourceError> {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| FeatureSourceError::Cache {
        path: PathBuf::from("<random>"),
        source: io::Error::other(error.to_string()),
    })?;
    Ok(hex::encode(bytes))
}
fn extraction_directory(blobs: &Path, digest: &str) -> PathBuf {
    blobs
        .parent()
        .unwrap_or(blobs)
        .join("extracted")
        .join(digest.trim_start_matches("sha256:"))
}

/// Safely extracts a gzip-compressed or plain tar archive into a newly-created directory.
///
/// # Errors
/// Rejects traversal, links, special files, duplicates, and configured bounds.
pub fn extract_archive(
    archive: &Path,
    destination: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    if destination.exists() {
        return Ok(());
    }
    let parent = destination
        .parent()
        .ok_or_else(|| FeatureSourceError::Archive {
            path: destination.to_path_buf(),
            message: "destination has no parent",
        })?;
    fs::create_dir_all(parent).map_err(|source| FeatureSourceError::ArchiveIo {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = parent.join(format!(".extract-{}", random_suffix()?));
    fs::create_dir(&temporary).map_err(|source| FeatureSourceError::ArchiveIo {
        path: temporary.clone(),
        source,
    })?;
    let result = extract_into(archive, &temporary, limits);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary, destination) {
        if destination.is_dir() {
            let _ = fs::remove_dir_all(&temporary);
        } else {
            let _ = fs::remove_dir_all(&temporary);
            return Err(FeatureSourceError::ArchiveIo {
                path: destination.to_path_buf(),
                source,
            });
        }
    }
    Ok(())
}
#[expect(
    clippy::too_many_lines,
    reason = "the streaming archive state and all bound checks remain visible together"
)]
fn extract_into(
    archive: &Path,
    destination: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    let compressed = fs::metadata(archive)
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?
        .len()
        .max(1);
    let file = File::open(archive).map_err(|source| FeatureSourceError::ArchiveIo {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut magic = [0_u8; 2];
    let mut buffered = BufReader::new(file);
    let count = buffered
        .read(&mut magic)
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
    buffered =
        BufReader::new(
            File::open(archive).map_err(|source| FeatureSourceError::ArchiveIo {
                path: archive.to_path_buf(),
                source,
            })?,
        );
    let reader: Box<dyn Read> = if count == 2 && magic == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(buffered))
    } else {
        Box::new(buffered)
    };
    let mut tar = tar::Archive::new(reader);
    let entries = tar
        .entries()
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
    let mut seen = BTreeSet::new();
    let mut files = 0_usize;
    let mut extracted = 0_u64;
    for entry in entries {
        let mut entry = entry.map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
        files += 1;
        if files > limits.files {
            return Err(FeatureSourceError::Limit {
                kind: "archive entries",
                limit: limits.files as u64,
            });
        }
        let path = entry
            .path()
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: archive.to_path_buf(),
                source,
            })?
            .into_owned();
        validate_archive_path(&path, limits.path_bytes)?;
        if !seen.insert(path.clone()) {
            return Err(FeatureSourceError::Archive {
                path,
                message: "duplicate archive path",
            });
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(FeatureSourceError::Archive {
                path,
                message: "links and special entry types are unsupported",
            });
        }
        let target = destination.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|source| FeatureSourceError::ArchiveIo {
                path: target,
                source,
            })?;
            continue;
        }
        let size = entry.size();
        extracted = extracted
            .checked_add(size)
            .ok_or(FeatureSourceError::Limit {
                kind: "extracted bytes",
                limit: limits.extracted_bytes,
            })?;
        if extracted > limits.extracted_bytes
            || extracted > compressed.saturating_mul(limits.expansion_ratio)
        {
            return Err(FeatureSourceError::Limit {
                kind: "extracted bytes/ratio",
                limit: limits
                    .extracted_bytes
                    .min(compressed.saturating_mul(limits.expansion_ratio)),
            });
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| FeatureSourceError::ArchiveIo {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: target.clone(),
                source,
            })?;
        io::copy(&mut entry, &mut output).map_err(|source| FeatureSourceError::ArchiveIo {
            path: target,
            source,
        })?;
    }
    Ok(())
}
fn validate_archive_path(path: &Path, max: usize) -> Result<(), FeatureSourceError> {
    let text = path.to_str().ok_or_else(|| FeatureSourceError::Archive {
        path: path.to_path_buf(),
        message: "path is not UTF-8",
    })?;
    if text.len() > max {
        return Err(FeatureSourceError::Limit {
            kind: "archive path bytes",
            limit: max as u64,
        });
    }
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FeatureSourceError::Archive {
            path: path.to_path_buf(),
            message: "path is absolute or traversing",
        });
    }
    Ok(())
}

fn validate_local_tree(
    root: &Path,
    base: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| FeatureSourceError::Cache {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| FeatureSourceError::Cache {
                path: directory.clone(),
                source,
            })?;
            count += 1;
            if count > limits.files {
                return Err(FeatureSourceError::Limit {
                    kind: "local files",
                    limit: limits.files as u64,
                });
            }
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| FeatureSourceError::Cache {
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(FeatureSourceError::LocalContainment { path: entry.path() });
            }
            if metadata.is_dir() {
                let canonical = fs::canonicalize(entry.path())
                    .map_err(|_| FeatureSourceError::LocalContainment { path: entry.path() })?;
                if !canonical.starts_with(base) {
                    return Err(FeatureSourceError::LocalContainment { path: canonical });
                }
                pending.push(canonical);
            } else if metadata.is_file() {
                bytes += metadata.len();
                if bytes > limits.extracted_bytes {
                    return Err(FeatureSourceError::Limit {
                        kind: "local bytes",
                        limit: limits.extracted_bytes,
                    });
                }
            } else {
                return Err(FeatureSourceError::LocalContainment { path: entry.path() });
            }
        }
    }
    Ok(())
}
fn hash_local_tree(root: &Path) -> Result<String, FeatureSourceError> {
    fn visit(root: &Path, directory: &Path, hash: &mut Sha256) -> Result<(), FeatureSourceError> {
        let mut entries = fs::read_dir(directory)
            .map_err(|source| FeatureSourceError::Cache {
                path: directory.to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| FeatureSourceError::Cache {
                path: directory.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| FeatureSourceError::LocalContainment { path: path.clone() })?;
            let metadata = entry
                .metadata()
                .map_err(|source| FeatureSourceError::Cache {
                    path: path.clone(),
                    source,
                })?;
            hash.update((relative.as_os_str().len() as u64).to_be_bytes());
            hash.update(relative.as_os_str().as_encoded_bytes());
            hash.update([u8::from(metadata.is_dir())]);
            if metadata.is_dir() {
                visit(root, &path, hash)?;
            } else {
                let mut file = File::open(&path).map_err(|source| FeatureSourceError::Cache {
                    path: path.clone(),
                    source,
                })?;
                let mut buffer = [0_u8; 8192];
                loop {
                    let count =
                        file.read(&mut buffer)
                            .map_err(|source| FeatureSourceError::Cache {
                                path: path.clone(),
                                source,
                            })?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, root, &mut hash)?;
    Ok(format!("sha256:{}", hex::encode(hash.finalize())))
}

fn read_feature_metadata(root: &Path, limit: u64) -> Result<FeatureMetadata, FeatureSourceError> {
    let path = root.join("devcontainer-feature.json");
    let file = File::open(&path).map_err(|source| FeatureSourceError::Metadata {
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| FeatureSourceError::Metadata {
            path: path.clone(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(FeatureSourceError::Limit {
            kind: "Feature metadata bytes",
            limit,
        });
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|source| FeatureSourceError::MetadataJson { path, source })?;
    Ok(FeatureMetadata::from_value(&value)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn archive(path: &Path, name: &str) {
        let file = File::create(path).expect("archive");
        let mut tar = tar::Builder::new(file);
        let bytes = br#"{"id":"fixture","version":"1"}"#;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, Cursor::new(bytes))
            .expect("entry");
        tar.finish().expect("finish");
    }

    #[test]
    fn extraction_accepts_metadata_and_rejects_traversal() {
        let temporary = tempfile::tempdir().expect("temp");
        let good = temporary.path().join("good.tar");
        archive(&good, "devcontainer-feature.json");
        extract_archive(
            &good,
            &temporary.path().join("out"),
            FeatureSourceLimits::default(),
        )
        .expect("safe archive");
        assert!(
            temporary
                .path()
                .join("out/devcontainer-feature.json")
                .is_file()
        );
        let bad = temporary.path().join("bad.tar");
        let file = File::create(&bad).expect("archive");
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.as_mut_bytes()[..10].copy_from_slice(b"../escape\0");
        header.set_cksum();
        builder.append(&header, Cursor::new(b"x")).expect("entry");
        builder.finish().expect("finish");
        assert!(
            extract_archive(
                &bad,
                &temporary.path().join("bad-out"),
                FeatureSourceLimits::default()
            )
            .is_err()
        );
        assert!(!temporary.path().join("escape").exists());
    }

    #[test]
    fn local_source_must_be_contained_and_link_free() {
        let temporary = tempfile::tempdir().expect("temp");
        let feature = temporary.path().join("features/tool");
        fs::create_dir_all(&feature).expect("dir");
        fs::write(
            feature.join("devcontainer-feature.json"),
            br#"{"id":"tool","version":"1"}"#,
        )
        .expect("metadata");
        let resolver =
            FeatureSourceResolver::new(temporary.path().join("cache")).expect("resolver");
        let reference = FeatureReference::parse("./features/tool").expect("reference");
        let result = resolver
            .resolve_local(&reference, reference.as_str(), temporary.path())
            .expect("local");
        assert_eq!(result.package.metadata.id, "tool");
    }
}
