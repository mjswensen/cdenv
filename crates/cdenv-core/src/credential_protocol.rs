//! Bounded, lookup-only Git credential protocol surface, version 1.
//!
//! Raw bodies can contain secrets even on `get`. Neither requests nor responses
//! implement general serialization or expose their contents through `Debug`.
//! Callers must use private pipes and must bound input before allocating it.

use std::collections::BTreeMap;
use std::fmt;
use std::io::{self, Read, Write};

use thiserror::Error;
use zeroize::Zeroizing;

use crate::credentials::HttpsOrigin;

/// Version of the supported username/password credential field surface.
pub const GIT_CREDENTIAL_SURFACE_VERSION: u32 = 1;
/// Maximum bytes in a complete request or result, including separators.
pub const MAX_CREDENTIAL_BYTES: usize = 32 * 1024;
/// Maximum bytes in any field, including its name and `=` separator.
pub const MAX_CREDENTIAL_FIELD_BYTES: usize = 8 * 1024;
/// Maximum number of fields in a credential message.
pub const MAX_CREDENTIAL_FIELDS: usize = 8;

/// Supported Git helper invocation, deliberately excluding arbitrary operations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialHelperOperation {
    /// A validated, authorized lookup may be delegated to host Git.
    Get,
    /// Git's approve/store path is a successful no-op, never host delegation.
    Store,
    /// Git's reject/erase path is a successful no-op, never host delegation.
    Erase,
}

impl std::str::FromStr for CredentialHelperOperation {
    type Err = CredentialProtocolError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "get" => Ok(Self::Get),
            "store" => Ok(Self::Store),
            "erase" => Ok(Self::Erase),
            _ => Err(CredentialProtocolError::Operation),
        }
    }
}

/// Validated HTTPS lookup context. Path and account remain distinct from policy.
pub struct GitCredentialRequest {
    origin: HttpsOrigin,
    host: Zeroizing<String>,
    path: Option<Zeroizing<String>>,
    username: Option<Zeroizing<String>>,
}

impl fmt::Debug for GitCredentialRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitCredentialRequest([REDACTED])")
    }
}

impl GitCredentialRequest {
    /// Validates explicit protocol/host/path/username fields only.
    ///
    /// # Errors
    ///
    /// Rejects unsupported fields (including URL, password, refresh tokens, and
    /// config instructions), duplicates, malformed framing, or non-HTTPS origins.
    pub fn parse(bytes: &[u8]) -> Result<Self, CredentialProtocolError> {
        let mut fields = parse_fields(bytes)?;
        let protocol = fields
            .remove("protocol")
            .ok_or(CredentialProtocolError::Context)?;
        let host = fields
            .remove("host")
            .ok_or(CredentialProtocolError::Context)?;
        if protocol != "https" || host.is_empty() || host.contains('/') {
            return Err(CredentialProtocolError::Context);
        }
        let origin = HttpsOrigin::parse(&format!("https://{host}"))
            .map_err(|_| CredentialProtocolError::Context)?;
        let path = fields.remove("path");
        let username = fields.remove("username");
        if !fields.is_empty() {
            return Err(CredentialProtocolError::UnsupportedField);
        }
        Ok(Self {
            origin,
            host: Zeroizing::new(host.to_owned()),
            path: path.map(|v| Zeroizing::new(v.to_owned())),
            username: username.map(|v| Zeroizing::new(v.to_owned())),
        })
    }

    /// Borrows the normalized origin for current-grant authorization.
    #[must_use]
    pub const fn origin(&self) -> &HttpsOrigin {
        &self.origin
    }

    /// Encodes only supported context into a private host-Git input pipe.
    ///
    /// # Errors
    ///
    /// Returns the writer's I/O failure; no payload is included in diagnostics.
    pub fn write_private(&self, output: &mut impl Write) -> io::Result<()> {
        output.write_all(b"protocol=https\n")?;
        write_field(output, "host", &self.host)?;
        if let Some(path) = &self.path {
            write_field(output, "path", path)?;
        }
        if let Some(username) = &self.username {
            write_field(output, "username", username)?;
        }
        output.write_all(b"\n")
    }
}

/// A validated, uncached username/password result, optionally with expiry.
pub struct GitCredentialResponse {
    username: Zeroizing<String>,
    password: Zeroizing<String>,
    expires_at: Option<u64>,
}

impl fmt::Debug for GitCredentialResponse {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("GitCredentialResponse([REDACTED])")
    }
}

impl GitCredentialResponse {
    /// Parses bounded host Git output and validates any echoed request context.
    ///
    /// Expired or incomplete results return `None`, not placeholder credentials.
    /// Unrecognized fields are not passed through to container Git.
    ///
    /// # Errors
    ///
    /// Returns value-free errors for malformed, unsupported, or conflicting fields.
    pub fn parse(
        bytes: &[u8],
        request: &GitCredentialRequest,
        now_unix_seconds: u64,
    ) -> Result<Option<Self>, CredentialProtocolError> {
        let mut fields = parse_fields(bytes)?;
        if fields
            .remove("protocol")
            .is_some_and(|value| value != "https")
            || fields.remove("host").is_some_and(|host| {
                host.contains('/')
                    || HttpsOrigin::parse(&format!("https://{host}")).as_ref()
                        != Ok(&request.origin)
            })
            || fields
                .remove("path")
                .is_some_and(|path| request.path.as_ref().map(|value| value.as_str()) != Some(path))
        {
            return Err(CredentialProtocolError::Context);
        }
        let username = fields.remove("username");
        let password = fields.remove("password");
        let expires_at = fields
            .remove("password_expiry_utc")
            .map(|value| {
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(CredentialProtocolError::Expiry);
                }
                value
                    .parse::<u64>()
                    .map_err(|_| CredentialProtocolError::Expiry)
            })
            .transpose()?;
        if !fields.is_empty() {
            return Err(CredentialProtocolError::UnsupportedField);
        }
        if expires_at.is_some_and(|expiry| expiry <= now_unix_seconds) {
            return Ok(None);
        }
        let (Some(username), Some(password)) = (username, password) else {
            return Ok(None);
        };
        if request
            .username
            .as_ref()
            .is_some_and(|expected| expected.as_str() != username)
        {
            return Err(CredentialProtocolError::Context);
        }
        Ok(Some(Self {
            username: Zeroizing::new(username.to_owned()),
            password: Zeroizing::new(password.to_owned()),
            expires_at,
        }))
    }

    /// Returns whether the result may still be released, immediately before writing.
    #[must_use]
    pub fn is_current(&self, now_unix_seconds: u64) -> bool {
        self.expires_at
            .is_none_or(|expiry| expiry > now_unix_seconds)
    }

    /// Writes supported result fields only to the private recipient.
    ///
    /// The broker must recheck grant revision and expiry before calling this.
    ///
    /// # Errors
    ///
    /// Returns an output I/O error without recording the body.
    pub fn write_private(&self, output: &mut impl Write) -> io::Result<()> {
        write_field(output, "username", &self.username)?;
        write_field(output, "password", &self.password)?;
        if let Some(expiry) = self.expires_at {
            writeln!(output, "password_expiry_utc={expiry}")?;
        }
        output.write_all(b"\n")
    }
}

fn write_field(output: &mut impl Write, name: &str, value: &str) -> io::Result<()> {
    output.write_all(name.as_bytes())?;
    output.write_all(b"=")?;
    output.write_all(value.as_bytes())?;
    output.write_all(b"\n")
}

/// Reads one Git helper body through a finite allocation bound.
///
/// The caller must also impose a transport deadline when reading from an
/// untrusted or potentially stalled peer. The returned storage zeroizes on drop.
///
/// # Errors
///
/// Returns a value-free size or I/O error, never raw input.
pub fn read_private_body(
    input: &mut impl Read,
) -> Result<Zeroizing<Vec<u8>>, CredentialProtocolError> {
    let mut bytes = Zeroizing::new(Vec::new());
    input
        .take((MAX_CREDENTIAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| CredentialProtocolError::Io)?;
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialProtocolError::Bounds);
    }
    Ok(bytes)
}

fn parse_fields(bytes: &[u8]) -> Result<BTreeMap<&str, &str>, CredentialProtocolError> {
    if bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(CredentialProtocolError::Bounds);
    }
    let text = std::str::from_utf8(bytes).map_err(|_| CredentialProtocolError::Framing)?;
    // Git permits EOF after the last LF as well as the usual blank terminator.
    if !text.is_empty() && !text.ends_with('\n') {
        return Err(CredentialProtocolError::Framing);
    }
    let mut fields = BTreeMap::new();
    let mut terminated = false;
    for line in text.split_terminator('\n') {
        if terminated {
            return Err(CredentialProtocolError::Framing);
        }
        if line.is_empty() {
            terminated = true;
            continue;
        }
        if line.len() > MAX_CREDENTIAL_FIELD_BYTES || fields.len() == MAX_CREDENTIAL_FIELDS {
            return Err(CredentialProtocolError::Bounds);
        }
        if line.chars().any(char::is_control) {
            return Err(CredentialProtocolError::Framing);
        }
        let (key, value) = line
            .split_once('=')
            .ok_or(CredentialProtocolError::Framing)?;
        if key.is_empty() || fields.insert(key, value).is_some() {
            return Err(CredentialProtocolError::Framing);
        }
    }
    Ok(fields)
}

/// Value-free failures safe to display for even malicious raw credential input.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum CredentialProtocolError {
    /// Only Git get/store/erase helper invocations are accepted.
    #[error("unsupported Git credential helper operation")]
    Operation,
    /// A whole message, field, or field count exceeded its bound.
    #[error("Git credential message exceeded its bound")]
    Bounds,
    /// Input cannot be represented by the supported field grammar.
    #[error("malformed Git credential message")]
    Framing,
    /// A secret, URL, config instruction, or unknown field was provided.
    #[error(
        "unsupported Git credential field; only the version-1 username/password surface is supported"
    )]
    UnsupportedField,
    /// Origin or account context is missing, conflicting, or unsupported.
    #[error("invalid or conflicting HTTPS credential context")]
    Context,
    /// Expiry was neither a positive nor zero decimal Unix timestamp.
    #[error("invalid Git credential expiry")]
    Expiry,
    /// Private transport input failed; details are deliberately suppressed.
    #[error("cannot read private Git credential input")]
    Io,
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &[u8] =
        b"protocol=https\nhost=git.internal:8443\npath=team/repo.git\nusername=alice\n\n";

    #[test]
    fn actual_path_and_account_round_trip_without_collapsing_context() {
        for (path, user) in [("one/repo.git", "alice"), ("two/repo.git", "bob")] {
            let input =
                format!("protocol=https\nhost=git.internal:8443\npath={path}\nusername={user}\n\n");
            let request = GitCredentialRequest::parse(input.as_bytes()).expect("request");
            let mut recipient = Vec::new();
            request.write_private(&mut recipient).expect("pipe");
            assert_eq!(recipient, input.as_bytes());
        }
    }

    #[test]
    fn raw_context_and_responses_have_redacted_debug_forms() {
        let request = GitCredentialRequest::parse(
            b"protocol=https\nhost=git.internal\npath=SECRET-PATH\nusername=SECRET-USER\n\n",
        )
        .expect("request");
        let response = GitCredentialResponse::parse(
            b"username=SECRET-USER\npassword=SECRET-TOKEN\n\n",
            &request,
            100,
        )
        .expect("response")
        .expect("available");
        assert!(!format!("{request:?} {response:?}").contains("SECRET"));
        let mut recipient = Vec::new();
        response
            .write_private(&mut recipient)
            .expect("private pipe");
        assert!(recipient.windows(12).any(|part| part == b"SECRET-TOKEN"));
    }

    #[test]
    fn requests_reject_secrets_unknown_fields_conflicts_and_execution_context() {
        for input in [
            "password=SECRET",
            "url=https://a:b@git.internal",
            "protocol=https",
            "path=duplicate",
            "helper=!touch /tmp/SECRET",
            "config=/tmp/SECRET",
            "environment=SECRET",
            "cwd=/SECRET",
            "oauth_refresh_token=SECRET",
            "capability[]=authtype",
            "username=other",
            "wwwauth[]=SECRET",
        ] {
            let mut bytes = REQUEST[..REQUEST.len() - 1].to_vec();
            bytes.extend_from_slice(input.as_bytes());
            bytes.extend_from_slice(b"\n\n");
            let error = GitCredentialRequest::parse(&bytes).expect_err("rejected");
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
    }

    #[test]
    fn malformed_and_truncated_inputs_fail_closed() {
        for input in [
            b"protocol=https\nhost=git.internal".as_slice(),
            b"protocol=https\nhost=git.internal\n\npassword=SECRET\n",
            b"protocol=https\nhost=git.internal\0\n\n",
            b"protocol=https\nhost=git.internal\r\n\n",
            b"protocol=http\nhost=git.internal\n\n",
            b"protocol=https\nhost=git.internal/path\n\n",
        ] {
            assert!(GitCredentialRequest::parse(input).is_err());
        }
    }

    #[test]
    fn byte_field_and_count_bounds_are_enforced() {
        let at_limit = vec![b'x'; MAX_CREDENTIAL_BYTES];
        assert_eq!(
            read_private_body(&mut at_limit.as_slice())
                .expect("at bound")
                .len(),
            MAX_CREDENTIAL_BYTES
        );
        assert_eq!(
            read_private_body(&mut vec![0; MAX_CREDENTIAL_BYTES + 1].as_slice())
                .expect_err("over bound"),
            CredentialProtocolError::Bounds
        );
        let field = format!("path={}\n", "x".repeat(MAX_CREDENTIAL_FIELD_BYTES - 5));
        assert!(parse_fields(field.as_bytes()).is_ok());
        assert_eq!(
            parse_fields(
                format!("path={}x\n", "x".repeat(MAX_CREDENTIAL_FIELD_BYTES - 5)).as_bytes()
            )
            .expect_err("field bound"),
            CredentialProtocolError::Bounds
        );
        let mut fields = String::new();
        for n in 0..MAX_CREDENTIAL_FIELDS {
            use std::fmt::Write as _;
            writeln!(&mut fields, "key{n}=value").expect("fixture field");
        }
        assert!(parse_fields(fields.as_bytes()).is_ok());
        assert_eq!(
            parse_fields(format!("{fields}extra=value\n").as_bytes()).expect_err("field count"),
            CredentialProtocolError::Bounds
        );
    }

    #[test]
    fn expired_and_incomplete_results_never_become_placeholder_credentials() {
        let request = GitCredentialRequest::parse(REQUEST).expect("request");
        for result in [
            b"username=alice\n\n".as_slice(),
            b"username=alice\npassword=SECRET\npassword_expiry_utc=100\n\n",
            b"\n",
        ] {
            assert!(
                GitCredentialResponse::parse(result, &request, 100)
                    .expect("valid result")
                    .is_none()
            );
        }
        let response = GitCredentialResponse::parse(
            b"username=alice\npassword=SECRET\npassword_expiry_utc=101\n\n",
            &request,
            100,
        )
        .expect("valid")
        .expect("not expired");
        assert!(!response.is_current(101));
    }

    #[test]
    fn host_results_cannot_rewrite_context_or_supply_unrecognized_fields() {
        let request = GitCredentialRequest::parse(REQUEST).expect("request");
        for extra in [
            "host=other.internal",
            "protocol=http",
            "path=another/repo",
            "oauth_refresh_token=SECRET",
            "password_expiry_utc=-1",
        ] {
            let body = format!("username=alice\npassword=SECRET\n{extra}\n\n");
            let error = GitCredentialResponse::parse(body.as_bytes(), &request, 100)
                .expect_err("reject result");
            assert!(!format!("{error:?} {error}").contains("SECRET"));
        }
    }

    #[test]
    fn store_and_erase_are_not_lookup_operations() {
        assert_eq!("store".parse(), Ok(CredentialHelperOperation::Store));
        assert_eq!("erase".parse(), Ok(CredentialHelperOperation::Erase));
        assert!("fill".parse::<CredentialHelperOperation>().is_err());
    }
}
