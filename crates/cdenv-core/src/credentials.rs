//! Explicit host capability identifiers and exact HTTPS-origin authorization.
//!
//! These types contain policy, never credentials or host execution context. New
//! capabilities require new enum variants and explicit consent; unknown input
//! cannot deserialize as an existing grant.

use std::collections::BTreeSet;
use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::{Host, Url};

/// Maximum number of exact HTTPS origins granted to one workspace.
pub const MAX_HTTPS_ORIGINS: usize = 64;
/// Maximum bytes in an explicit origin before URL parsing.
pub const MAX_ORIGIN_BYTES: usize = 2048;

/// An independently consented host capability.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CredentialCapability {
    /// Lookup-only delegation to trusted host Git helpers.
    GitHttps,
    /// General selected SSH-agent access, including signing authority.
    SshAgent,
    /// Missing Git author name/email defaults, not signing configuration.
    GitIdentity,
}

impl CredentialCapability {
    /// Returns the stable public capability name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::GitHttps => "git-https",
            Self::SshAgent => "ssh-agent",
            Self::GitIdentity => "git-identity",
        }
    }

    /// Returns a value-free explanation of the explicitly delegated authority.
    #[must_use]
    pub const fn authority(self) -> &'static str {
        match self {
            Self::GitHttps => {
                "permits host Git credential lookups for the listed HTTPS origins; returned tokens enter container memory and can be copied by workspace code"
            }
            Self::SshAgent => {
                "permits general use of the selected host SSH agent, including signing; this is not Git-only or destination-scoped access"
            }
            Self::GitIdentity => {
                "permits host user.name/user.email to fill missing Git identity fields; does not grant signing or credential access"
            }
        }
    }
}

impl fmt::Display for CredentialCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for CredentialCapability {
    type Err = CredentialPolicyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "git-https" => Ok(Self::GitHttps),
            "ssh-agent" => Ok(Self::SshAgent),
            "git-identity" => Ok(Self::GitIdentity),
            _ => Err(CredentialPolicyError::UnknownCapability),
        }
    }
}

/// Exact canonical HTTPS scheme/host/effective-port authority.
///
/// DNS hosts use URL/IDNA canonicalization and a single trailing DNS root dot is
/// removed. IPv6 uses bracketed canonical notation. Port 443 is always explicit.
/// Private and single-label hosts are allowed; this is not Feature source policy.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct HttpsOrigin(String);

impl HttpsOrigin {
    /// Parses an explicit origin without accepting URL userinfo or path context.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTPS schemes, userinfo, non-root paths, query/fragment,
    /// wildcards, ambiguous URL syntax, control characters, and oversized input.
    pub fn parse(value: &str) -> Result<Self, CredentialPolicyError> {
        if value.len() > MAX_ORIGIN_BYTES
            || value.chars().any(|c| c.is_control() || c.is_whitespace())
            || value.contains(['\\', '%', '*', '?', '#', '@'])
        {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        let (scheme, authority) = value
            .split_once("://")
            .ok_or(CredentialPolicyError::InvalidOrigin)?;
        if !scheme.eq_ignore_ascii_case("https") {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        let authority = authority.strip_suffix('/').unwrap_or(authority);
        if authority.is_empty() || authority.contains('/') || authority.ends_with(':') {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        let url = Url::parse(value).map_err(|_| CredentialPolicyError::InvalidOrigin)?;
        let host = match url.host().ok_or(CredentialPolicyError::InvalidOrigin)? {
            Host::Domain(host) => {
                let host = host.strip_suffix('.').unwrap_or(host);
                if host.is_empty()
                    || host.split('.').any(|label| {
                        label.is_empty()
                            || label.len() > 63
                            || label.starts_with('-')
                            || label.ends_with('-')
                            || !label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                    || host.len() > 253
                {
                    return Err(CredentialPolicyError::InvalidOrigin);
                }
                host.to_owned()
            }
            Host::Ipv4(host) => {
                // WHATWG accepts shorthand/octal IPv4. Reject rather than silently
                // granting a different-looking endpoint such as 127.1 or 0177.0.0.1.
                let input_host = authority.split(':').next().unwrap_or_default();
                if input_host != host.to_string() {
                    return Err(CredentialPolicyError::InvalidOrigin);
                }
                host.to_string()
            }
            Host::Ipv6(host) => format!("[{host}]"),
        };
        let port = url
            .port_or_known_default()
            .ok_or(CredentialPolicyError::InvalidOrigin)?;
        if port == 0 {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        Ok(Self(format!("https://{host}:{port}")))
    }

    /// Extracts an origin from original, already credential-sanitized host metadata.
    ///
    /// This must never be called on a container-provided remote to widen a grant.
    ///
    /// # Errors
    ///
    /// Rejects non-HTTPS sources and sources that still contain credential-bearing
    /// or ambiguous URL components.
    pub fn from_repository_source(value: &str) -> Result<Self, CredentialPolicyError> {
        if value.chars().any(|c| c.is_control() || c.is_whitespace()) || value.contains('\\') {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        let url = Url::parse(value).map_err(|_| CredentialPolicyError::InvalidOrigin)?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(CredentialPolicyError::InvalidOrigin);
        }
        // Parse the original authority, not a repaired WHATWG representation.
        let (_, remainder) = value
            .split_once("://")
            .ok_or(CredentialPolicyError::InvalidOrigin)?;
        let authority = remainder
            .split('/')
            .next()
            .ok_or(CredentialPolicyError::InvalidOrigin)?;
        Self::parse(&format!("https://{authority}"))
    }

    /// Borrows the canonical origin, including its effective port.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HttpsOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for HttpsOrigin {
    type Err = CredentialPolicyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<String> for HttpsOrigin {
    type Error = CredentialPolicyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<HttpsOrigin> for String {
    fn from(value: HttpsOrigin) -> Self {
        value.0
    }
}

/// Persistable host SSH-agent selector, never a container-chosen endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SshAgentSelector(String);

impl SshAgentSelector {
    /// Selects automatic refresh from explicit host mutating invocations only.
    #[must_use]
    pub fn automatic() -> Self {
        Self("auto".to_owned())
    }

    /// Returns a fixed host socket path, or `None` for automatic selection.
    #[must_use]
    pub fn explicit_path(&self) -> Option<&str> {
        (self.0 != "auto").then_some(self.0.as_str())
    }
}

impl FromStr for SshAgentSelector {
    type Err = CredentialPolicyError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value == "auto" {
            return Ok(Self::automatic());
        }
        if !value.starts_with('/')
            || value.len() > 4096
            || value.chars().any(char::is_control)
            || value
                .split('/')
                .skip(1)
                .any(|part| matches!(part, "" | "." | ".."))
        {
            return Err(CredentialPolicyError::InvalidSocketSelector);
        }
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for SshAgentSelector {
    type Error = CredentialPolicyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        value.parse()
    }
}

impl From<SshAgentSelector> for String {
    fn from(value: SshAgentSelector) -> Self {
        value.0
    }
}

/// Independently granted capabilities; absence is always disabled.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CredentialGrants {
    git_https: Option<BTreeSet<HttpsOrigin>>,
    ssh_agent: Option<SshAgentSelector>,
    git_identity: bool,
}

impl CredentialGrants {
    /// Returns whether the named capability is explicitly enabled.
    #[must_use]
    pub const fn enabled(&self, capability: CredentialCapability) -> bool {
        match capability {
            CredentialCapability::GitHttps => self.git_https.is_some(),
            CredentialCapability::SshAgent => self.ssh_agent.is_some(),
            CredentialCapability::GitIdentity => self.git_identity,
        }
    }

    /// Returns the exact allowlist, distinguishing disabled from enabled-empty.
    #[must_use]
    pub const fn https_origins(&self) -> Option<&BTreeSet<HttpsOrigin>> {
        self.git_https.as_ref()
    }

    /// Returns the configured selector, without resolving or connecting it.
    #[must_use]
    pub const fn ssh_selector(&self) -> Option<&SshAgentSelector> {
        self.ssh_agent.as_ref()
    }

    /// Checks exact-origin authority, independent of repository path or username.
    #[must_use]
    pub fn allows(&self, origin: &HttpsOrigin) -> bool {
        self.git_https
            .as_ref()
            .is_some_and(|origins| origins.contains(origin))
    }

    /// Validates the aggregate bound after decoding host policy.
    ///
    /// # Errors
    ///
    /// Returns an error when a persisted allowlist exceeds the release limit.
    pub fn validate(&self) -> Result<(), CredentialPolicyError> {
        if self
            .git_https
            .as_ref()
            .is_some_and(|origins| origins.len() > MAX_HTTPS_ORIGINS)
        {
            return Err(CredentialPolicyError::TooManyOrigins);
        }
        Ok(())
    }

    /// Enables HTTPS or adds explicit origins; an empty first enable is refused.
    ///
    /// # Errors
    ///
    /// Requires an initial origin and enforces the aggregate bound transactionally.
    pub fn enable_https(&mut self, origins: &[HttpsOrigin]) -> Result<(), CredentialPolicyError> {
        if self.git_https.is_none() && origins.is_empty() {
            return Err(CredentialPolicyError::OriginsRequired);
        }
        let mut desired = self.git_https.clone().unwrap_or_default();
        desired.extend(origins.iter().cloned());
        if desired.len() > MAX_HTTPS_ORIGINS {
            return Err(CredentialPolicyError::TooManyOrigins);
        }
        self.git_https = Some(desired);
        Ok(())
    }

    /// Changes an enabled HTTPS allowlist without implicitly enabling it.
    ///
    /// # Errors
    ///
    /// Rejects a disabled capability, an empty adjustment, or too many origins.
    pub fn adjust_origins(
        &mut self,
        origins: &[HttpsOrigin],
        allow: bool,
    ) -> Result<(), CredentialPolicyError> {
        if self.git_https.is_none() {
            return Err(CredentialPolicyError::HttpsDisabled);
        }
        if origins.is_empty() {
            return Err(CredentialPolicyError::OriginsRequired);
        }
        if allow {
            self.enable_https(origins)
        } else {
            if let Some(granted) = &mut self.git_https {
                granted.retain(|origin| !origins.contains(origin));
            }
            Ok(())
        }
    }

    /// Enables exactly the SSH-agent capability with an explicit host selector.
    pub fn enable_ssh_agent(&mut self, selector: SshAgentSelector) {
        self.ssh_agent = Some(selector);
    }

    /// Enables only author identity defaults, not credentials or signing.
    pub const fn enable_identity(&mut self) {
        self.git_identity = true;
    }

    /// Revokes a capability and discards its authority, including origin grants.
    pub fn disable(&mut self, capability: CredentialCapability) {
        match capability {
            CredentialCapability::GitHttps => self.git_https = None,
            CredentialCapability::SshAgent => self.ssh_agent = None,
            CredentialCapability::GitIdentity => self.git_identity = false,
        }
    }

    /// Returns whether every currently supported capability is disabled.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.git_https.is_none() && self.ssh_agent.is_none() && !self.git_identity
    }
}

/// Value-free permission validation errors, safe even for raw secret-bearing input.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CredentialPolicyError {
    /// No unknown capability can inherit an existing grant.
    #[error("unknown credential capability; choose git-https, ssh-agent, or git-identity")]
    UnknownCapability,
    /// The input is not an unambiguous exact HTTPS origin.
    #[error("expected an exact HTTPS origin without userinfo, path, query, fragment, or wildcard")]
    InvalidOrigin,
    /// No origin can be inferred for this explicit first enable.
    #[error(
        "first git-https enable requires explicit --host HTTPS_ORIGIN or original HTTPS repository metadata"
    )]
    OriginsRequired,
    /// Adjustments must not implicitly enable a capability.
    #[error("git-https is disabled; explicitly enable it before adjusting origins")]
    HttpsDisabled,
    /// The per-workspace allowlist exceeds the finite release limit.
    #[error("a workspace may grant at most 64 HTTPS origins")]
    TooManyOrigins,
    /// The SSH selector is neither automatic nor an absolute lexical path.
    #[error(
        "SSH-agent selector must be auto or an absolute host socket path without traversal or control characters"
    )]
    InvalidSocketSelector,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_canonicalize_dns_idna_default_ports_and_ipv6() {
        for (input, expected) in [
            ("HTTPS://GitHub.COM/", "https://github.com:443"),
            ("https://git.intranet.:443", "https://git.intranet:443"),
            (
                "https://bücher.example:8443/",
                "https://xn--bcher-kva.example:8443",
            ),
            ("https://[2001:0db8::1]", "https://[2001:db8::1]:443"),
            ("https://127.0.0.1:8443", "https://127.0.0.1:8443"),
        ] {
            assert_eq!(
                HttpsOrigin::parse(input).expect("origin").as_str(),
                expected
            );
        }
    }

    #[test]
    fn origins_reject_ambiguous_or_secret_bearing_input_without_echoing_it() {
        for input in [
            "https://user:SECRET@example.com",
            "https://@example.com",
            "https://example.com/repo",
            "https://example.com/..",
            "https://example.com//",
            "https://example.com?SECRET",
            "https://example.com#SECRET",
            "https://*.example.com",
            "https://example.com:",
            "https://example.com:0",
            "https://127.1",
            "https://0177.0.0.1",
            "https:///example.com",
            "http://example.com",
            "https://ex%61mple.com",
            "https://example.com\nSECRET",
            " https://example.com",
            "https://example.com\\SECRET",
            "https://a..b",
        ] {
            let error = HttpsOrigin::parse(input).expect_err("invalid origin");
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
    }

    #[test]
    fn unknown_capabilities_and_fields_fail_closed() {
        assert!("gpg-agent".parse::<CredentialCapability>().is_err());
        assert!(serde_json::from_str::<CredentialCapability>("\"git-signing\"").is_err());
        assert!(
            serde_json::from_str::<CredentialGrants>(
                r#"{"gitHttps":null,"sshAgent":null,"gitIdentity":false,"gpgAgent":true}"#
            )
            .is_err()
        );
    }

    #[test]
    fn permissions_are_independent_and_disable_discards_https_grants() {
        let origin = HttpsOrigin::parse("https://git.internal").expect("origin");
        let mut grants = CredentialGrants::default();
        grants
            .enable_https(std::slice::from_ref(&origin))
            .expect("enable");
        grants.enable_identity();
        assert!(!grants.enabled(CredentialCapability::SshAgent));
        grants.disable(CredentialCapability::GitHttps);
        assert!(!grants.allows(&origin));
        assert!(grants.enabled(CredentialCapability::GitIdentity));
        assert!(grants.enable_https(&[]).is_err());
    }

    #[test]
    fn allow_never_enables_and_omitted_origins_never_expand() {
        let origin = HttpsOrigin::parse("https://git.internal").expect("origin");
        let mut grants = CredentialGrants::default();
        assert!(
            grants
                .adjust_origins(std::slice::from_ref(&origin), true)
                .is_err()
        );
        grants
            .enable_https(std::slice::from_ref(&origin))
            .expect("enable");
        grants.enable_https(&[]).expect("idempotent");
        assert_eq!(grants.https_origins().expect("enabled").len(), 1);
        grants.adjust_origins(&[origin], false).expect("deny");
        grants
            .enable_https(&[])
            .expect("empty enabled allowlist remains empty");
        assert_eq!(grants.https_origins().expect("enabled").len(), 0);
    }

    #[test]
    fn origin_limit_is_transactional() {
        let origins = (0..MAX_HTTPS_ORIGINS)
            .map(|n| HttpsOrigin::parse(&format!("https://git-{n}.internal")).expect("origin"))
            .collect::<Vec<_>>();
        let mut grants = CredentialGrants::default();
        grants.enable_https(&origins).expect("at limit");
        let before = grants.clone();
        assert_eq!(
            grants.enable_https(&[HttpsOrigin::parse("https://extra.internal").expect("origin")]),
            Err(CredentialPolicyError::TooManyOrigins)
        );
        assert_eq!(grants, before);
    }

    #[test]
    fn original_source_derivation_does_not_guess_transports_or_accept_secrets() {
        assert_eq!(
            HttpsOrigin::from_repository_source("https://git.internal:8443/team/repo.git")
                .expect("source")
                .as_str(),
            "https://git.internal:8443"
        );
        for source in [
            "git@git.internal:team/repo.git",
            "/local/repo",
            "ssh://git.internal/repo",
            "https://u:p@git.internal/repo",
            "https://git.internal/repo?secret",
        ] {
            assert!(HttpsOrigin::from_repository_source(source).is_err());
        }
    }

    #[test]
    fn socket_selectors_are_explicit_and_nontraversing() {
        assert_eq!("auto".parse(), Ok(SshAgentSelector::automatic()));
        assert_eq!(
            "/tmp/agent path"
                .parse::<SshAgentSelector>()
                .expect("explicit selector")
                .explicit_path(),
            Some("/tmp/agent path")
        );
        for value in [
            "",
            "relative",
            "/",
            "/tmp/../agent",
            "/tmp/./agent",
            "/tmp//agent",
            "/tmp/agent\n",
        ] {
            assert!(value.parse::<SshAgentSelector>().is_err());
        }
    }
}
