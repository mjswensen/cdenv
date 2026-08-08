use std::collections::BTreeMap;
use std::io::{Cursor, Read};
use std::path::{Component, Path};
use std::time::Duration;

use flate2::read::GzDecoder;
use futures_util::StreamExt;
use reqwest::header::{ACCEPT, AUTHORIZATION, CONTENT_LENGTH, HeaderMap, WWW_AUTHENTICATE};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::error::{Result, SpikeError, read};
use crate::profile::canonical_json;

const MANIFEST_LIMIT: usize = 1024 * 1024;
const BLOB_LIMIT: usize = 64 * 1024 * 1024;
const EXPANDED_LIMIT: usize = 128 * 1024 * 1024;
const ARCHIVE_FILE_LIMIT: usize = 4096;
const FEATURE_LAYER_MEDIA_TYPE: &str = "application/vnd.devcontainers.layer.v1+tar";
const OCI_MANIFEST_MEDIA_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const DOCKER_MANIFEST_MEDIA_TYPE: &str = "application/vnd.docker.distribution.manifest.v2+json";

#[derive(Debug, Clone)]
struct OciReference {
    original: String,
    registry: String,
    repository: String,
    selector: String,
}

#[derive(Debug, Deserialize)]
struct Manifest {
    #[serde(rename = "mediaType")]
    media_type: Option<String>,
    layers: Vec<Descriptor>,
    #[serde(default)]
    annotations: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
struct Descriptor {
    #[serde(rename = "mediaType")]
    media_type: String,
    digest: String,
    size: u64,
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    token: Option<String>,
    access_token: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ResolvedFeature {
    pub(crate) source: String,
    #[serde(rename = "manifestDigest")]
    pub(crate) manifest_digest: String,
    #[serde(rename = "blobDigest")]
    pub(crate) blob_digest: String,
    pub(crate) size: usize,
    pub(crate) version: String,
    #[serde(rename = "resolvedMetadata")]
    pub(crate) metadata: Value,
    #[serde(skip)]
    pub(crate) archive: Vec<u8>,
}

#[derive(Debug, Clone, Serialize)]
struct LockFeature {
    version: String,
    resolved: String,
    integrity: String,
    #[serde(rename = "dependsOn", skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
}

#[expect(
    clippy::too_many_lines,
    reason = "the disposable resolver spike intentionally exposes the end-to-end OCI transaction"
)]
pub(crate) async fn resolve_public_feature(
    reference: &str,
    cache: &Path,
) -> Result<ResolvedFeature> {
    let reference = OciReference::parse(reference)?;
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_mins(1))
        .redirect(verified_https_redirects(5))
        .build()
        .map_err(|source| SpikeError::Http {
            url: format!("https://{}", reference.registry),
            source,
        })?;
    let manifest_url = format!(
        "https://{}/v2/{}/manifests/{}",
        reference.registry, reference.repository, reference.selector
    );
    let accepts = format!("{OCI_MANIFEST_MEDIA_TYPE}, {DOCKER_MANIFEST_MEDIA_TYPE}");
    let initial = client
        .get(&manifest_url)
        .header(ACCEPT, &accepts)
        .send()
        .await
        .map_err(|source| SpikeError::Http {
            url: manifest_url.clone(),
            source,
        })?;
    let (response, bearer_token) = if initial.status() == reqwest::StatusCode::UNAUTHORIZED {
        let challenge = initial
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| SpikeError::OciResponse {
                url: manifest_url.clone(),
                detail: "registry returned 401 without a bearer challenge".to_string(),
            })?;
        let token = anonymous_bearer_token(&client, challenge).await?;
        let response = client
            .get(&manifest_url)
            .header(ACCEPT, accepts)
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .map_err(|source| SpikeError::Http {
                url: manifest_url.clone(),
                source,
            })?;
        (response, Some(token))
    } else {
        (initial, None)
    };
    if !response.status().is_success() {
        return Err(SpikeError::OciResponse {
            url: manifest_url,
            detail: format!("registry returned HTTP {}", response.status()),
        });
    }
    let response_headers = response.headers().clone();
    let manifest_bytes = bounded_body(response, MANIFEST_LIMIT, &manifest_url).await?;
    let computed_manifest_digest = digest(&manifest_bytes);
    if reference.selector.starts_with("sha256:") {
        verify_digest(&reference.selector, &manifest_bytes, "requested manifest")?;
    }
    if let Some(declared) = response_headers
        .get("docker-content-digest")
        .and_then(|value| value.to_str().ok())
    {
        verify_digest(declared, &manifest_bytes, "manifest")?;
    }
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).map_err(|error| SpikeError::OciResponse {
            url: manifest_url.clone(),
            detail: format!("manifest is not JSON: {error}"),
        })?;
    let media_type = manifest
        .media_type
        .as_deref()
        .unwrap_or(OCI_MANIFEST_MEDIA_TYPE);
    if !matches!(
        media_type,
        OCI_MANIFEST_MEDIA_TYPE | DOCKER_MANIFEST_MEDIA_TYPE
    ) {
        return Err(SpikeError::OciResponse {
            url: manifest_url,
            detail: format!("unsupported manifest media type `{media_type}`"),
        });
    }
    let feature_layers = manifest
        .layers
        .iter()
        .filter(|layer| layer.media_type == FEATURE_LAYER_MEDIA_TYPE)
        .collect::<Vec<_>>();
    if feature_layers.len() != 1 {
        return Err(SpikeError::OciResponse {
            url: manifest_url,
            detail: format!(
                "expected exactly one `{FEATURE_LAYER_MEDIA_TYPE}` layer, found {}",
                feature_layers.len()
            ),
        });
    }
    let layer = feature_layers[0];
    if layer.size > BLOB_LIMIT as u64 {
        return Err(SpikeError::OciResponse {
            url: manifest_url,
            detail: format!(
                "declared Feature layer size {} exceeds {BLOB_LIMIT}",
                layer.size
            ),
        });
    }
    let cache_path = cache.join(layer.digest.replace(':', "/"));
    let archive = if cache_path.is_file() {
        let bytes = read(&cache_path)?;
        verify_digest(&layer.digest, &bytes, "cached Feature blob")?;
        bytes
    } else {
        let blob_url = format!(
            "https://{}/v2/{}/blobs/{}",
            reference.registry, reference.repository, layer.digest
        );
        let mut request = client.get(&blob_url);
        if let Some(token) = &bearer_token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let response = request.send().await.map_err(|source| SpikeError::Http {
            url: blob_url.clone(),
            source,
        })?;
        if !response.status().is_success() {
            return Err(SpikeError::OciResponse {
                url: blob_url,
                detail: format!("registry returned HTTP {}", response.status()),
            });
        }
        let bytes = bounded_body(response, BLOB_LIMIT, &blob_url).await?;
        if bytes.len() as u64 != layer.size {
            return Err(SpikeError::OciResponse {
                url: blob_url,
                detail: format!(
                    "Feature blob size mismatch: declared {}, received {}",
                    layer.size,
                    bytes.len()
                ),
            });
        }
        verify_digest(&layer.digest, &bytes, "Feature blob")?;
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| SpikeError::Write {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        crate::error::write(&cache_path, &bytes)?;
        bytes
    };
    validate_archive(&archive)?;
    let metadata = manifest
        .annotations
        .get("dev.containers.metadata")
        .map(|metadata| serde_json::from_str(metadata))
        .transpose()
        .map_err(|error| SpikeError::OciResponse {
            url: manifest_url.clone(),
            detail: format!("`dev.containers.metadata` is invalid: {error}"),
        })?
        .unwrap_or(Value::Null);
    let version = metadata
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_string();

    Ok(ResolvedFeature {
        source: reference.original,
        manifest_digest: computed_manifest_digest,
        blob_digest: layer.digest.clone(),
        size: archive.len(),
        version,
        metadata,
        archive,
    })
}

impl OciReference {
    fn parse(reference: &str) -> Result<Self> {
        if reference.contains("://") || reference.contains('@') && !reference.contains("@sha256:") {
            return Err(SpikeError::OciReference {
                reference: reference.to_string(),
                detail: "credentials, URL schemes, and non-sha256 digests are forbidden"
                    .to_string(),
            });
        }
        let (registry, remainder) =
            reference
                .split_once('/')
                .ok_or_else(|| SpikeError::OciReference {
                    reference: reference.to_string(),
                    detail: "a fully qualified public registry is required".to_string(),
                })?;
        if !registry.contains('.') && registry != "localhost" {
            return Err(SpikeError::OciReference {
                reference: reference.to_string(),
                detail: "a fully qualified registry hostname is required".to_string(),
            });
        }
        let (repository, selector) = if let Some((repository, digest)) = remainder.rsplit_once('@')
        {
            if !digest.starts_with("sha256:") {
                return Err(SpikeError::OciReference {
                    reference: reference.to_string(),
                    detail: "only sha256 digest selectors are supported".to_string(),
                });
            }
            (repository, digest)
        } else if let Some(last_slash) = remainder.rfind('/') {
            let tail = &remainder[last_slash + 1..];
            if let Some((name, tag)) = tail.rsplit_once(':') {
                let repository_end = last_slash + 1 + name.len();
                (&remainder[..repository_end], tag)
            } else {
                (remainder, "latest")
            }
        } else if let Some((repository, tag)) = remainder.rsplit_once(':') {
            (repository, tag)
        } else {
            (remainder, "latest")
        };
        if repository.is_empty()
            || repository
                .split('/')
                .any(|component| component.is_empty() || component == "." || component == "..")
        {
            return Err(SpikeError::OciReference {
                reference: reference.to_string(),
                detail: "repository path is invalid".to_string(),
            });
        }
        Ok(Self {
            original: reference.to_ascii_lowercase(),
            registry: registry.to_ascii_lowercase(),
            repository: repository.to_ascii_lowercase(),
            selector: selector.to_string(),
        })
    }
}

async fn anonymous_bearer_token(client: &reqwest::Client, challenge: &str) -> Result<String> {
    let parameters = challenge
        .strip_prefix("Bearer ")
        .ok_or_else(|| SpikeError::OciResponse {
            url: "registry authentication challenge".to_string(),
            detail: "only anonymous Bearer authentication is supported".to_string(),
        })?;
    let fields = parse_auth_fields(parameters)?;
    let realm = fields.get("realm").ok_or_else(|| SpikeError::OciResponse {
        url: "registry authentication challenge".to_string(),
        detail: "Bearer challenge has no realm".to_string(),
    })?;
    let realm_url = url::Url::parse(realm).map_err(|error| SpikeError::OciResponse {
        url: realm.clone(),
        detail: format!("invalid bearer realm: {error}"),
    })?;
    if realm_url.scheme() != "https"
        || !realm_url.username().is_empty()
        || realm_url.password().is_some()
    {
        return Err(SpikeError::OciResponse {
            url: realm.clone(),
            detail: "Bearer realm must be credential-free HTTPS".to_string(),
        });
    }
    let mut request = client.get(realm_url);
    for key in ["service", "scope"] {
        if let Some(value) = fields.get(key) {
            request = request.query(&[(key, value)]);
        }
    }
    let response = request.send().await.map_err(|source| SpikeError::Http {
        url: realm.clone(),
        source,
    })?;
    if !response.status().is_success() {
        return Err(SpikeError::OciResponse {
            url: realm.clone(),
            detail: format!("token service returned HTTP {}", response.status()),
        });
    }
    let token: TokenResponse = response.json().await.map_err(|source| SpikeError::Http {
        url: realm.clone(),
        source,
    })?;
    token
        .token
        .or(token.access_token)
        .ok_or_else(|| SpikeError::OciResponse {
            url: realm.clone(),
            detail: "token response has neither `token` nor `access_token`".to_string(),
        })
}

fn verified_https_redirects(maximum: usize) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| {
        if attempt.previous().len() > maximum {
            attempt.error(format!("redirect chain exceeds {maximum} hops"))
        } else if attempt.url().scheme() != "https"
            || !attempt.url().username().is_empty()
            || attempt.url().password().is_some()
        {
            attempt.error("redirect target must be credential-free HTTPS")
        } else {
            attempt.follow()
        }
    })
}

fn parse_auth_fields(input: &str) -> Result<BTreeMap<String, String>> {
    let mut output = BTreeMap::new();
    for field in input.split(',') {
        let (key, value) = field
            .trim()
            .split_once('=')
            .ok_or_else(|| SpikeError::OciResponse {
                url: "registry authentication challenge".to_string(),
                detail: format!("invalid Bearer field `{field}`"),
            })?;
        let value = value
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
            .ok_or_else(|| SpikeError::OciResponse {
                url: "registry authentication challenge".to_string(),
                detail: format!("Bearer field `{key}` is not quoted"),
            })?;
        output.insert(key.to_string(), value.to_string());
    }
    Ok(output)
}

async fn bounded_body(response: reqwest::Response, limit: usize, url: &str) -> Result<Vec<u8>> {
    check_declared_size(response.headers(), limit, url)?;
    let mut stream = response.bytes_stream();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|source| SpikeError::Http {
            url: url.to_string(),
            source,
        })?;
        if output.len().saturating_add(chunk.len()) > limit {
            return Err(SpikeError::OciResponse {
                url: url.to_string(),
                detail: format!("response exceeds {limit} bytes"),
            });
        }
        output.extend_from_slice(&chunk);
    }
    Ok(output)
}

fn check_declared_size(headers: &HeaderMap, limit: usize, url: &str) -> Result<()> {
    if let Some(size) = headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        && size > limit as u64
    {
        return Err(SpikeError::OciResponse {
            url: url.to_string(),
            detail: format!("declared response size {size} exceeds {limit}"),
        });
    }
    Ok(())
}

pub(crate) fn digest(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

pub(crate) fn verify_digest(expected: &str, bytes: &[u8], object: &str) -> Result<()> {
    let actual = digest(bytes);
    if expected != actual {
        return Err(SpikeError::Archive(format!(
            "{object} digest mismatch: expected `{expected}`, got `{actual}`"
        )));
    }
    Ok(())
}

pub(crate) fn validate_archive(bytes: &[u8]) -> Result<()> {
    let expanded = decompress_bounded(bytes)?;
    let mut archive = tar::Archive::new(Cursor::new(&expanded));
    let mut count = 0_usize;
    let mut total = 0_u64;
    for entry in archive
        .entries()
        .map_err(|error| SpikeError::Archive(format!("invalid tar stream: {error}")))?
    {
        let entry =
            entry.map_err(|error| SpikeError::Archive(format!("invalid tar entry: {error}")))?;
        count += 1;
        if count > ARCHIVE_FILE_LIMIT {
            return Err(SpikeError::Archive(format!(
                "archive exceeds {ARCHIVE_FILE_LIMIT} entries"
            )));
        }
        total = total.saturating_add(entry.size());
        if total > EXPANDED_LIMIT as u64 {
            return Err(SpikeError::Archive(format!(
                "archive entries exceed {EXPANDED_LIMIT} bytes"
            )));
        }
        let path = entry
            .path()
            .map_err(|error| SpikeError::Archive(format!("invalid entry path: {error}")))?;
        validate_relative_path(&path)?;
        let entry_type = entry.header().entry_type();
        if !(entry_type.is_file() || entry_type.is_dir() || entry_type.is_symlink()) {
            return Err(SpikeError::Archive(format!(
                "unsupported archive entry type for `{}`",
                path.display()
            )));
        }
        if entry_type.is_symlink() {
            let target = entry
                .link_name()
                .map_err(|error| SpikeError::Archive(format!("invalid symlink target: {error}")))?
                .ok_or_else(|| {
                    SpikeError::Archive(format!("symlink `{}` has no target", path.display()))
                })?;
            let parent = path.parent().unwrap_or_else(|| Path::new(""));
            validate_joined_path(parent, &target)?;
        }
    }
    Ok(())
}

fn decompress_bounded(bytes: &[u8]) -> Result<Vec<u8>> {
    let reader: Box<dyn Read> = if bytes.starts_with(&[0x1f, 0x8b]) {
        Box::new(GzDecoder::new(Cursor::new(bytes)))
    } else {
        Box::new(Cursor::new(bytes))
    };
    let mut output = Vec::new();
    reader
        .take((EXPANDED_LIMIT + 1) as u64)
        .read_to_end(&mut output)
        .map_err(|error| SpikeError::Archive(format!("decompression failed: {error}")))?;
    if output.len() > EXPANDED_LIMIT {
        return Err(SpikeError::Archive(format!(
            "expanded archive exceeds {EXPANDED_LIMIT} bytes"
        )));
    }
    Ok(output)
}

fn validate_relative_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(SpikeError::Archive(format!(
            "entry path `{}` is not relative",
            path.display()
        )));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(SpikeError::Archive(format!(
            "entry path `{}` traverses outside extraction root",
            path.display()
        )));
    }
    Ok(())
}

fn validate_joined_path(parent: &Path, target: &Path) -> Result<()> {
    if target.is_absolute() {
        return Err(SpikeError::Archive(format!(
            "symlink target `{}` is absolute",
            target.display()
        )));
    }
    let mut depth = parent
        .components()
        .filter(|component| matches!(component, Component::Normal(_)))
        .count();
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(SpikeError::Archive(format!(
                    "symlink target `{}` escapes extraction root",
                    target.display()
                )));
            }
        }
    }
    Ok(())
}

pub(crate) fn extract_archive(bytes: &[u8], destination: &Path) -> Result<()> {
    validate_archive(bytes)?;
    let expanded = decompress_bounded(bytes)?;
    std::fs::create_dir_all(destination).map_err(|source| SpikeError::Write {
        path: destination.to_path_buf(),
        source,
    })?;
    let mut archive = tar::Archive::new(Cursor::new(expanded));
    for entry in archive
        .entries()
        .map_err(|error| SpikeError::Archive(format!("invalid tar stream: {error}")))?
    {
        let mut entry =
            entry.map_err(|error| SpikeError::Archive(format!("invalid tar entry: {error}")))?;
        if entry.header().entry_type().is_symlink() {
            continue;
        }
        entry.unpack_in(destination).map_err(|error| {
            SpikeError::Archive(format!("could not extract regular entry: {error}"))
        })?;
    }
    let mut archive = tar::Archive::new(Cursor::new(decompress_bounded(bytes)?));
    for entry in archive
        .entries()
        .map_err(|error| SpikeError::Archive(format!("invalid tar stream: {error}")))?
    {
        let mut entry =
            entry.map_err(|error| SpikeError::Archive(format!("invalid tar entry: {error}")))?;
        if entry.header().entry_type().is_symlink() {
            entry.unpack_in(destination).map_err(|error| {
                SpikeError::Archive(format!("could not extract symlink: {error}"))
            })?;
        }
    }
    Ok(())
}

pub(crate) async fn download_https_feature(
    url: &str,
    trusted_ca: Option<&Path>,
) -> Result<Vec<u8>> {
    let parsed = url::Url::parse(url).map_err(|error| SpikeError::OciResponse {
        url: url.to_string(),
        detail: format!("invalid HTTPS Feature URL: {error}"),
    })?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(SpikeError::OciResponse {
            url: url.to_string(),
            detail: "Feature URL must be credential-free HTTPS".to_string(),
        });
    }
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(verified_https_redirects(5));
    if let Some(path) = trusted_ca {
        let certificate =
            reqwest::Certificate::from_pem(&read(path)?).map_err(|source| SpikeError::Http {
                url: url.to_string(),
                source,
            })?;
        builder = builder.add_root_certificate(certificate);
    }
    let client = builder.build().map_err(|source| SpikeError::Http {
        url: url.to_string(),
        source,
    })?;
    let response = client
        .get(parsed)
        .send()
        .await
        .map_err(|source| SpikeError::Http {
            url: url.to_string(),
            source,
        })?;
    if !response.status().is_success() {
        return Err(SpikeError::OciResponse {
            url: url.to_string(),
            detail: format!("server returned HTTP {}", response.status()),
        });
    }
    let bytes = bounded_body(response, BLOB_LIMIT, url).await?;
    validate_archive(&bytes)?;
    Ok(bytes)
}

pub(crate) fn lockfile(feature: &ResolvedFeature) -> Result<Vec<u8>> {
    let dependencies = feature
        .metadata
        .get("dependsOn")
        .and_then(Value::as_object)
        .map(|values| values.keys().map(|key| key.to_ascii_lowercase()).collect())
        .unwrap_or_default();
    let id_without_selector = feature
        .source
        .split('@')
        .next()
        .unwrap_or(&feature.source)
        .rsplit_once(':')
        .map_or(feature.source.as_str(), |(id, _)| id);
    let record = LockFeature {
        version: feature.version.clone(),
        resolved: format!("{}@{}", id_without_selector, feature.manifest_digest),
        integrity: feature.blob_digest.clone(),
        depends_on: dependencies,
    };
    canonical_json(&serde_json::json!({
        "features": {
            feature.source.to_ascii_lowercase(): record
        }
    }))
}

pub(crate) fn archive_metadata(bytes: &[u8]) -> Result<Value> {
    let expanded = decompress_bounded(bytes)?;
    let mut archive = tar::Archive::new(Cursor::new(expanded));
    for entry in archive
        .entries()
        .map_err(|error| SpikeError::Archive(format!("invalid tar stream: {error}")))?
    {
        let entry =
            entry.map_err(|error| SpikeError::Archive(format!("invalid tar entry: {error}")))?;
        let path = entry
            .path()
            .map_err(|error| SpikeError::Archive(format!("invalid entry path: {error}")))?;
        if path
            .file_name()
            .is_some_and(|name| name == "devcontainer-feature.json")
        {
            let mut bytes = Vec::new();
            entry
                .take((1024 * 1024 + 1) as u64)
                .read_to_end(&mut bytes)
                .map_err(|error| {
                    SpikeError::Archive(format!("could not read Feature metadata: {error}"))
                })?;
            if bytes.len() > 1024 * 1024 {
                return Err(SpikeError::Archive(
                    "Feature metadata exceeds 1048576 bytes".to_string(),
                ));
            }
            return serde_json::from_slice(&bytes).map_err(|error| {
                SpikeError::Archive(format!("Feature metadata is invalid JSON: {error}"))
            });
        }
    }
    Err(SpikeError::Archive(
        "archive contains no devcontainer-feature.json".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn archive_with_entry(path: &str, contents: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut output);
            let mut header = tar::Header::new_gnu();
            header.set_size(contents.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, contents)
                .expect("fixture tar should build");
            builder.finish().expect("fixture tar should finish");
        }
        output
    }

    #[test]
    fn corrupt_digest_fails_with_expected_and_actual_values() {
        let error = verify_digest(
            "sha256:0000000000000000000000000000000000000000000000000000000000000000",
            b"different",
            "fixture",
        )
        .expect_err("digest should fail");

        assert!(error.to_string().contains("digest mismatch"));
    }

    #[test]
    fn archive_parent_traversal_is_rejected() {
        let mut output = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut output);
            let mut header = tar::Header::new_gnu();
            header.set_size(1);
            header.set_mode(0o644);
            let malicious_path = b"../escape";
            header.as_mut_bytes()[..malicious_path.len()].copy_from_slice(malicious_path);
            header.set_cksum();
            builder
                .append(&header, Cursor::new(b"x"))
                .expect("fixture tar should build");
            builder.finish().expect("fixture tar should finish");
        }

        let error = validate_archive(&output).expect_err("traversal should fail");

        assert!(
            error
                .to_string()
                .contains("traverses outside extraction root")
        );
    }

    #[test]
    fn escaping_symlink_is_rejected() {
        let mut output = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut output);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_path("link").expect("path should be valid");
            header
                .set_link_name("../escape")
                .expect("target should be valid");
            header.set_cksum();
            builder
                .append(&header, std::io::empty())
                .expect("fixture tar should build");
            builder.finish().expect("fixture tar should finish");
        }

        let error = validate_archive(&output).expect_err("symlink should fail");

        assert!(error.to_string().contains("escapes extraction root"));
    }

    #[test]
    fn regular_feature_archive_is_accepted() {
        let archive = archive_with_entry(
            "devcontainer-feature.json",
            br#"{"id":"fixture","version":"1.0.0"}"#,
        );

        let result = validate_archive(&archive);

        assert!(result.is_ok(), "archive validation failed: {result:?}");
    }

    #[test]
    fn bearer_challenge_fields_are_parsed_without_credentials() {
        let fields = parse_auth_fields(
            r#"realm="https://ghcr.io/token",service="ghcr.io",scope="repository:a/b:pull""#,
        )
        .expect("challenge should parse");

        assert_eq!(fields["scope"], "repository:a/b:pull");
    }

    #[test]
    fn private_oci_reference_with_userinfo_is_rejected() {
        let error = OciReference::parse("user@example.test/owner/feature:1")
            .expect_err("userinfo should fail");

        assert!(error.to_string().contains("credentials"));
    }

    #[test]
    fn feature_archive_metadata_is_bounded_and_readable() {
        let archive = archive_with_entry(
            "devcontainer-feature.json",
            br#"{"id":"fixture","version":"1.0.0"}"#,
        );

        let metadata = archive_metadata(&archive).expect("metadata should parse");

        assert_eq!(metadata["id"], "fixture");
    }

    #[test]
    fn oversized_archive_entry_is_rejected_before_extraction() {
        let mut header = tar::Header::new_gnu();
        header.set_size(EXPANDED_LIMIT as u64 + 1);
        header.set_mode(0o644);
        header.set_path("oversized").expect("path should be valid");
        header.set_cksum();
        let mut archive = header.as_bytes().to_vec();
        archive.extend_from_slice(&[0_u8; 1024]);

        let error = validate_archive(&archive).expect_err("oversized entry should fail");

        assert!(error.to_string().contains("exceed"));
    }

    #[test]
    fn oversized_feature_metadata_is_rejected() {
        let archive = archive_with_entry("devcontainer-feature.json", &vec![b'x'; 1024 * 1024 + 1]);

        let error = archive_metadata(&archive).expect_err("oversized metadata should fail");

        assert!(error.to_string().contains("exceeds 1048576 bytes"));
    }

    #[test]
    fn gzip_archive_is_accepted_after_bounded_decompression() {
        let archive = archive_with_entry(
            "devcontainer-feature.json",
            br#"{"id":"fixture","version":"1.0.0"}"#,
        );
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(&archive).expect("gzip write should work");
        let compressed = encoder.finish().expect("gzip should finish");

        let result = validate_archive(&compressed);

        assert!(result.is_ok(), "archive validation failed: {result:?}");
    }
}
