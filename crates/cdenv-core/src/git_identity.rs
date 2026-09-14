//! Bounded author-identity metadata carried by the credential broker.
//!
//! Name and email are data, not command lines or authentication material. The
//! two fields are optional independently so host configuration gaps remain
//! advisory and never prevent transport readiness.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Maximum encoded identity metadata accepted by either side of the bridge.
pub const MAX_GIT_IDENTITY_BYTES: usize = 16 * 1024;
/// Maximum UTF-8 bytes in one Git identity value.
pub const MAX_GIT_IDENTITY_FIELD_BYTES: usize = 8 * 1024;

/// Optional host Git author defaults.
#[derive(Clone, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GitIdentityMetadata {
    name: Option<String>,
    email: Option<String>,
}

impl std::fmt::Debug for GitIdentityMetadata {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitIdentityMetadata")
            .field("name", &self.name.as_ref().map(|_| "[PRESENT]"))
            .field("email", &self.email.as_ref().map(|_| "[PRESENT]"))
            .finish()
    }
}

impl GitIdentityMetadata {
    /// Constructs independently optional, validated identity fields.
    ///
    /// # Errors
    ///
    /// Rejects oversized values and control characters, including line breaks
    /// and NUL, which cannot safely enter the supported Git configuration grammar.
    pub fn new(name: Option<String>, email: Option<String>) -> Result<Self, GitIdentityError> {
        validate_field(name.as_deref())?;
        validate_field(email.as_deref())?;
        Ok(Self { name, email })
    }

    /// Borrows the optional author name.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Borrows the optional author email.
    #[must_use]
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    /// Encodes the closed metadata schema for private transport or owned storage.
    ///
    /// # Errors
    ///
    /// Returns a value-free validation error for malformed or oversized fields.
    pub fn encode(&self) -> Result<Vec<u8>, GitIdentityError> {
        validate_field(self.name())?;
        validate_field(self.email())?;
        let bytes = serde_json::to_vec(self).map_err(|_| GitIdentityError::Encoding)?;
        if bytes.len() > MAX_GIT_IDENTITY_BYTES {
            return Err(GitIdentityError::Bounds);
        }
        Ok(bytes)
    }

    /// Decodes and validates the closed metadata schema.
    ///
    /// # Errors
    ///
    /// Rejects unknown fields, malformed JSON, controls, and bounds violations.
    pub fn decode(bytes: &[u8]) -> Result<Self, GitIdentityError> {
        if bytes.len() > MAX_GIT_IDENTITY_BYTES {
            return Err(GitIdentityError::Bounds);
        }
        let metadata =
            serde_json::from_slice::<Self>(bytes).map_err(|_| GitIdentityError::Encoding)?;
        validate_field(metadata.name())?;
        validate_field(metadata.email())?;
        Ok(metadata)
    }
}

fn validate_field(value: Option<&str>) -> Result<(), GitIdentityError> {
    if value.is_some_and(|value| {
        value.len() > MAX_GIT_IDENTITY_FIELD_BYTES || value.chars().any(char::is_control)
    }) {
        Err(GitIdentityError::InvalidField)
    } else {
        Ok(())
    }
}

/// Value-free identity metadata validation failures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum GitIdentityError {
    /// One value exceeded the supported field or aggregate bound.
    #[error("Git identity metadata exceeded its bound")]
    Bounds,
    /// One value contains unsupported control input.
    #[error("Git identity metadata contains unsupported control input")]
    InvalidField,
    /// The closed metadata schema was malformed or contained unknown fields.
    #[error("invalid Git identity metadata")]
    Encoding,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_are_independently_optional_and_debug_is_value_free() {
        let metadata =
            GitIdentityMetadata::new(Some("Secret Name".to_owned()), None).expect("metadata");
        let decoded =
            GitIdentityMetadata::decode(&metadata.encode().expect("encode")).expect("decode");
        assert_eq!(decoded.name(), Some("Secret Name"));
        assert_eq!(decoded.email(), None);
        assert!(!format!("{decoded:?}").contains("Secret Name"));
    }

    #[test]
    fn controls_bounds_and_unknown_fields_are_rejected_without_echoing_values() {
        for value in ["line\nbreak", "nul\0value", "escape\u{1b}value"] {
            let error = GitIdentityMetadata::new(Some(value.to_owned()), None)
                .expect_err("control rejected");
            assert!(!error.to_string().contains(value));
        }
        assert!(
            GitIdentityMetadata::new(Some("x".repeat(MAX_GIT_IDENTITY_FIELD_BYTES + 1)), None)
                .is_err()
        );
        assert!(
            GitIdentityMetadata::decode(br#"{"name":null,"email":null,"key":"value"}"#).is_err()
        );
    }
}
