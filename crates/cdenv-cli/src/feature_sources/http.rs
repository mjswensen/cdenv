//! Verified HTTPS requests and explicit redirect policy.
use super::archive::extract_archive;
use super::cache::extraction_directory;
use super::metadata::read_feature_metadata;
use super::oci::require_success;
use super::{FeatureSourceError, FeatureSourceResolver, VerifiedFeature};
use cdenv_devcontainer::{FeatureInstallIdentity, FeaturePackage, FeatureReference};
use reqwest::Response;
use reqwest::header::{ACCEPT, AUTHORIZATION, LOCATION};
use std::net::IpAddr;
use url::Url;

impl FeatureSourceResolver {
    pub(super) async fn resolve_https(
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

    pub(super) async fn get_following(
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
}

pub(super) fn checked_https_url(value: &str) -> Result<Url, FeatureSourceError> {
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
