//! Reference normalization and content identity validation; no source access.

use std::fmt;
use std::net::IpAddr;

use url::Url;

use super::FeatureError;

/// A normalized, supported Feature reference.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureReference {
    /// A fully qualified anonymous OCI reference.
    Oci(String),
    /// An unauthenticated HTTPS tarball URL.
    Https(String),
    /// A directory contained below the selected configuration directory.
    Local(String),
}

impl FeatureReference {
    /// Normalizes a supported Feature reference without performing I/O.
    ///
    /// # Errors
    ///
    /// Rejects credentials, insecure or private endpoints, traversal, legacy identifiers, and
    /// malformed OCI, HTTPS, or local forms.
    pub fn parse(input: &str) -> Result<Self, FeatureError> {
        if input.starts_with("./") {
            return normalize_local(input).map(Self::Local);
        }
        if input.starts_with("https://") {
            return normalize_https(input).map(Self::Https);
        }
        if input.contains("://") || input.starts_with("http:") {
            return Err(FeatureError::UnsupportedReference {
                reference: input.to_owned(),
                reason: "Feature transport must use verified HTTPS".to_owned(),
            });
        }
        normalize_oci(input).map(Self::Oci)
    }

    /// Returns the canonical reference used for lookup and lock keys.
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Oci(value) | Self::Https(value) | Self::Local(value) => value,
        }
    }

    pub(super) fn resource_name(&self) -> &str {
        match self {
            Self::Oci(value) => oci_resource_name(value),
            Self::Https(value) | Self::Local(value) => value,
        }
    }

    pub(super) fn selector(&self) -> &str {
        match self {
            Self::Oci(value) => value
                .strip_prefix(oci_resource_name(value))
                .unwrap_or_default(),
            Self::Https(_) | Self::Local(_) => "",
        }
    }
}

impl fmt::Display for FeatureReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

fn normalize_local(input: &str) -> Result<String, FeatureError> {
    let mut parts = Vec::new();
    for part in input[2..].split('/') {
        match part {
            "" | "." => {}
            ".." => {
                return Err(unsupported(
                    input,
                    "local Feature escapes its configuration directory",
                ));
            }
            value if value.contains('\0') || value.contains('\\') => {
                return Err(unsupported(
                    input,
                    "local Feature path contains an unsupported component",
                ));
            }
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        return Err(unsupported(
            input,
            "local Feature path must name a directory",
        ));
    }
    Ok(format!("./{}", parts.join("/")))
}

fn normalize_https(input: &str) -> Result<String, FeatureError> {
    let mut url = Url::parse(input).map_err(|error| unsupported(input, error.to_string()))?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(unsupported(
            input,
            "Feature URL credentials are unsupported",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| unsupported(input, "Feature URL requires a public host"))?;
    reject_private_host(input, host)?;
    if url.fragment().is_some() {
        return Err(unsupported(input, "Feature URL fragments are unsupported"));
    }
    url.set_host(Some(&host.to_ascii_lowercase()))
        .map_err(|_| unsupported(input, "Feature URL host is invalid"))?;
    Ok(url.to_string())
}

fn normalize_oci(input: &str) -> Result<String, FeatureError> {
    if input.is_empty() || input.chars().any(char::is_whitespace) {
        return Err(unsupported(
            input,
            "OCI Feature reference is empty or contains whitespace",
        ));
    }
    if input.contains('@') && !input.contains("@sha256:") {
        return Err(unsupported(
            input,
            "OCI credentials and non-SHA-256 digests are unsupported",
        ));
    }
    let (name_and_tag, digest) = input
        .split_once('@')
        .map_or((input, None), |(name, digest)| (name, Some(digest)));
    if let Some(digest) = digest {
        validate_sha256(digest).map_err(|reason| unsupported(input, reason))?;
    }
    let slash = name_and_tag
        .find('/')
        .ok_or_else(|| unsupported(input, "OCI Feature reference must be fully qualified"))?;
    let registry = &name_and_tag[..slash];
    reject_private_host(input, registry.split(':').next().unwrap_or(registry))?;
    if !(registry.contains('.') || registry.contains(':')) {
        return Err(unsupported(
            input,
            "OCI Feature reference must include a registry host",
        ));
    }
    let repository_and_tag = &name_and_tag[slash + 1..];
    let last = repository_and_tag.rsplit('/').next().unwrap_or_default();
    let has_tag = last.contains(':');
    let repository = if has_tag {
        repository_and_tag
            .rsplit_once(':')
            .map_or(repository_and_tag, |(name, _)| name)
    } else {
        repository_and_tag
    };
    if repository.is_empty()
        || repository.split('/').any(|part| {
            part.is_empty()
                || !part.bytes().all(|byte| {
                    byte.is_ascii_lowercase()
                        || byte.is_ascii_digit()
                        || matches!(byte, b'.' | b'_' | b'-')
                })
        })
    {
        return Err(unsupported(
            input,
            "OCI repository must be lowercase and fully qualified",
        ));
    }
    if has_tag && last.rsplit_once(':').is_some_and(|(_, tag)| tag.is_empty()) {
        return Err(unsupported(input, "OCI Feature tag cannot be empty"));
    }
    let mut normalized = format!("{}/{repository_and_tag}", registry.to_ascii_lowercase());
    if digest.is_none() && !has_tag {
        normalized.push_str(":latest");
    }
    if let Some(digest) = digest {
        normalized.push('@');
        normalized.push_str(digest);
    }
    Ok(normalized)
}

fn reject_private_host(reference: &str, host: &str) -> Result<(), FeatureError> {
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
        return Err(unsupported(
            reference,
            "private or local Feature endpoints are unsupported",
        ));
    }
    Ok(())
}

fn unsupported(reference: &str, reason: impl Into<String>) -> FeatureError {
    FeatureError::UnsupportedReference {
        reference: reference.to_owned(),
        reason: reason.into(),
    }
}

pub(super) fn validate_sha256(value: &str) -> Result<(), &'static str> {
    let Some(hex) = value.strip_prefix("sha256:") else {
        return Err("digest must use SHA-256");
    };
    if hex.len() != 64
        || !hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err("digest must contain 64 lowercase hexadecimal characters");
    }
    Ok(())
}

pub(super) fn oci_resource_name(value: &str) -> &str {
    let without_digest = value.split('@').next().unwrap_or(value);
    let slash = without_digest.rfind('/').unwrap_or(0);
    without_digest[slash + 1..]
        .find(':')
        .map_or(without_digest, |offset| {
            &without_digest[..slash + 1 + offset]
        })
}

/// Content identity established by a future transport adapter.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FeatureInstallIdentity {
    /// OCI manifest digest.
    OciDigest(String),
    /// SHA-256 integrity of an HTTPS tarball.
    HttpsIntegrity(String),
    /// Canonical checkout-contained local source.
    Local(String),
}

impl FeatureInstallIdentity {
    /// Creates an OCI identity after validating the digest.
    ///
    /// # Errors
    ///
    /// Returns [`FeatureError::InvalidIdentity`] for a malformed digest.
    pub fn oci_digest(digest: impl Into<String>) -> Result<Self, FeatureError> {
        let digest = digest.into();
        validate_sha256(&digest).map_err(|message| FeatureError::InvalidIdentity {
            value: digest.clone(),
            message,
        })?;
        Ok(Self::OciDigest(digest))
    }
    /// Creates an HTTPS identity after validating artifact integrity.
    ///
    /// # Errors
    ///
    /// Returns [`FeatureError::InvalidIdentity`] for malformed integrity.
    pub fn https_integrity(integrity: impl Into<String>) -> Result<Self, FeatureError> {
        let integrity = integrity.into();
        validate_sha256(&integrity).map_err(|message| FeatureError::InvalidIdentity {
            value: integrity.clone(),
            message,
        })?;
        Ok(Self::HttpsIntegrity(integrity))
    }
    /// Creates a source-unique local identity.
    #[must_use]
    pub fn local(reference: &FeatureReference) -> Self {
        Self::Local(reference.as_str().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_references_reject_username_only_and_password_only_credentials() {
        for reference in [
            "https://user@example.com/feature.tgz",
            "https://:secret@example.com/feature.tgz",
        ] {
            assert!(FeatureReference::parse(reference).is_err());
        }
    }

    #[test]
    fn references_normalize_and_reject_unsupported_transport_forms() {
        assert_eq!(
            FeatureReference::parse("GHCR.IO/devcontainers/features/git:1")
                .expect("public OCI reference")
                .as_str(),
            "ghcr.io/devcontainers/features/git:1"
        );
        assert_eq!(
            FeatureReference::parse("./features//git/./")
                .expect("contained local reference")
                .as_str(),
            "./features/git"
        );
        for invalid in [
            "http://example.com/feature.tgz",
            "https://user:secret@example.com/feature.tgz",
            "https://localhost/feature.tgz",
            "https://127.0.0.1/feature.tgz",
            "https://10.0.0.1/feature.tgz",
            "https://169.254.169.254/feature.tgz",
            "https://host.local/feature.tgz",
            "https://example.com/feature.tgz#fragment",
            "./features/../secret",
            "./features\\\\secret",
            "./features/\0secret",
            "./",
            "git",
            "localhost/features/git:1",
            "ghcr.io/Uppercase/git:1",
            "ghcr.io/features/git:",
            "ghcr.io/features/git@sha256:abc",
            "ghcr.io/features/git@sha512:abc",
        ] {
            assert!(matches!(
                FeatureReference::parse(invalid),
                Err(FeatureError::UnsupportedReference { .. })
            ));
        }
    }
}
