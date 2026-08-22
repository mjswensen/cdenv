//! OCI registry protocol and anonymous Bearer authentication.
use super::archive::extract_archive;
use super::cache::{digest_bytes, validate_digest, verify_digest};
use super::cache::{extraction_directory, verify_cached};
use super::http::checked_https_url;
use super::metadata::read_feature_metadata;
use super::{
    DOCKER_LIST, DOCKER_MANIFEST, FEATURE_LAYER, FEATURE_LAYER_GZIP, FeatureSourceResolver,
    OCI_INDEX, OCI_MANIFEST,
};
use super::{FeatureSourceError, FeatureSourceLimits, VerifiedFeature};
use cdenv_devcontainer::{FeatureInstallIdentity, FeaturePackage, FeatureReference};
use futures_util::StreamExt;
use reqwest::header::{CONTENT_TYPE, WWW_AUTHENTICATE};
use reqwest::{Client, Response, StatusCode};
use serde::Deserialize;
use url::Url;

impl FeatureSourceResolver {
    pub(super) async fn resolve_oci(
        &self,
        reference: &FeatureReference,
        value: &str,
    ) -> Result<VerifiedFeature, FeatureSourceError> {
        let parsed = OciReference::parse(value)?;
        let accept = format!("{OCI_MANIFEST}, {DOCKER_MANIFEST}, {OCI_INDEX}, {DOCKER_LIST}");
        let mut manifest_url = parsed.manifest_url(&parsed.selector)?;
        let mut token = None;
        let response = self
            .get_oci_response(manifest_url.clone(), Some(&accept), &mut token)
            .await?;
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
                .get_oci_response(manifest_url, Some(&accept), &mut token)
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
                .get_oci_response(parsed.blob_url(&layer.digest)?, None, &mut token)
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

    /// Sends a registry request, acquiring or refreshing only anonymous Bearer credentials.
    async fn get_oci_response(
        &self,
        url: Url,
        accept: Option<&str>,
        token: &mut Option<String>,
    ) -> Result<Response, FeatureSourceError> {
        let response = self
            .get_following(url.clone(), accept, token.as_deref())
            .await?;
        if response.status() != StatusCode::UNAUTHORIZED {
            return Ok(response);
        }
        let challenge = response
            .headers()
            .get(WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .ok_or_else(|| {
                FeatureSourceError::Authentication(
                    "registry requires anonymous Bearer authentication".to_owned(),
                )
            })?;
        *token = Some(bearer_token(&self.client, self.limits, challenge).await?);
        self.get_following(url, accept, token.as_deref()).await
    }
}

async fn bearer_token(
    client: &Client,
    limits: FeatureSourceLimits,
    challenge: &str,
) -> Result<String, FeatureSourceError> {
    let fields = parse_bearer_challenge(challenge)?;
    let realm = checked_https_url(fields.realm)?;
    let mut request = client.get(realm.clone());
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
    let bytes = read_response(response, limits.metadata_bytes).await?;
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
pub(super) async fn read_response(
    response: Response,
    limit: u64,
) -> Result<Vec<u8>, FeatureSourceError> {
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
pub(super) fn require_success(response: &Response) -> Result<(), FeatureSourceError> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bearer_challenge_accepts_anonymous_scope_without_credentials() {
        let fields = parse_bearer_challenge(
            "Bearer realm=\"https://tokens.example/token\",service=\"registry.example\",scope=\"repository:tool:pull\"",
        )
        .expect("anonymous Bearer challenge");

        assert_eq!(fields.realm, "https://tokens.example/token");
        assert_eq!(fields.service, Some("registry.example"));
        assert_eq!(fields.scope, Some("repository:tool:pull"));
    }

    #[test]
    fn bearer_challenge_rejects_private_and_malformed_authentication() {
        assert!(parse_bearer_challenge("Basic realm=\"private\"").is_err());
        assert!(parse_bearer_challenge("Bearer realm=unquoted").is_err());
    }
}
