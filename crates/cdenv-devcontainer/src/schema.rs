//! Compile-time-pinned Dev Container schema identity.

/// Public compatibility profile name.
pub const PROFILE_REVISION: &str = "cdenv-devcontainer-v1";
/// Pinned upstream Dev Container specification commit.
pub const SPECIFICATION_COMMIT: &str = "c95ffeed1d059abfe9ffbe79762dc2fa4e7c2421";
/// SHA-256 of the unmodified pinned base schema.
pub const BASE_SCHEMA_SHA256: &str =
    "a0883c0405ff433db188849d458fb20b9c0d73e0ba1a6e44c1d83f3b485408dd";

const BASE_SCHEMA: &[u8] = include_bytes!("../vendor/devContainer.base.schema.json");

const _: () = assert!(PROFILE_REVISION.len() == 21);
const _: () = assert!(SPECIFICATION_COMMIT.len() == 40);
const _: () = assert!(BASE_SCHEMA_SHA256.len() == 64);

/// Returns the unmodified schema embedded in this build.
///
/// Runtime code must use these bytes and must never follow `$schema` URIs from
/// user configuration.
#[must_use]
pub const fn base_schema() -> &'static [u8] {
    BASE_SCHEMA
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn embedded_schema_checksum_matches_the_profile_contract() {
        assert_eq!(
            hex::encode(Sha256::digest(base_schema())),
            BASE_SCHEMA_SHA256
        );
    }

    #[test]
    fn embedded_schema_is_valid_json() {
        let schema: serde_json::Value =
            serde_json::from_slice(base_schema()).expect("vendored schema should be valid JSON");
        assert!(schema.is_object());
    }
}
