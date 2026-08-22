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
        let mut authorization = token;
        for followed in 0..=self.limits.redirects {
            let mut request = self.client.get(url.clone());
            if let Some(value) = accept {
                request = request.header(ACCEPT, value);
            }
            if let Some(value) = authorization {
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
            let redirect = checked_https_url(
                url.join(location)
                    .map_err(|_| FeatureSourceError::Url {
                        url: location.to_owned(),
                        message: "invalid redirect Location",
                    })?
                    .as_str(),
            )?;
            if !same_origin(&url, &redirect) {
                authorization = None;
            }
            url = redirect;
        }
        unreachable!("redirect loop always returns")
    }
}

/// Returns whether two request URLs have the same scheme, host, and effective port.
///
/// Redirected bearer credentials are only retained within this origin boundary.
fn same_origin(left: &Url, right: &Url) -> bool {
    left.scheme() == right.scheme()
        && left.host_str().is_some_and(|host| {
            right
                .host_str()
                .is_some_and(|other| host.eq_ignore_ascii_case(other))
        })
        && left.port_or_known_default() == right.port_or_known_default()
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn same_origin_requires_scheme_host_and_effective_port_to_match() {
        let origin = Url::parse("https://registry.example:443/v2/tool").expect("origin URL");
        let same = Url::parse("https://REGISTRY.example/v2/blob").expect("same-origin URL");
        let different_port = Url::parse("https://registry.example:8443/blob").expect("port URL");
        let different_scheme = Url::parse("http://registry.example/blob").expect("scheme URL");
        let different_host = Url::parse("https://token.example/blob").expect("host URL");

        assert!(same_origin(&origin, &same));
        assert!(!same_origin(&origin, &different_port));
        assert!(!same_origin(&origin, &different_scheme));
        assert!(!same_origin(&origin, &different_host));
    }
}
