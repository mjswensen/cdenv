//! Persistent installation identity, SSH consent, and keyed fingerprints.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use cdenv_core::InstallationId;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Deserializer, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CdenvRoot, ManagedMode, ManagedPathError, ManagedPathKind, ManagedPathState, StorageError,
    atomic_write, ensure_private_directory, inspect_managed_path, tighten_managed_file,
};

/// The only installation-record schema understood by this build.
pub const INSTALLATION_SCHEMA_VERSION: u32 = 1;
const FINGERPRINT_KEY_LENGTH: usize = 32;

/// Persisted consent for adding cdenv's Include to user SSH configuration.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SshIncludeConsent {
    /// No choice has been recorded.
    #[default]
    Unknown,
    /// The user accepted automatic Include configuration.
    Accepted,
    /// The user declined automatic Include configuration.
    Declined,
}

/// Stable operational metadata for one cdenv root.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstallationRecord {
    schema_version: u32,
    installation_id: InstallationId,
    ssh_include_consent: SshIncludeConsent,
    fingerprint_key_id: String,
}

impl InstallationRecord {
    /// Returns the record schema version.
    #[must_use]
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the stable random installation ID.
    #[must_use]
    pub const fn installation_id(&self) -> &InstallationId {
        &self.installation_id
    }

    /// Returns the user's persisted SSH Include choice.
    #[must_use]
    pub const fn ssh_include_consent(&self) -> SshIncludeConsent {
        self.ssh_include_consent
    }
}

/// Why previously persisted plan fingerprints can no longer be trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FingerprintKeyUnknownReason {
    /// The independent key file is absent.
    Missing,
    /// The key bytes no longer match the installation record.
    Replaced,
}

/// The checked fingerprint capability for an installation.
pub enum FingerprintKeyState {
    /// Keyed digests can be computed safely.
    Available(FingerprintKey),
    /// Persisted fingerprints must be treated as unknown.
    Unknown(FingerprintKeyUnknownReason),
}

impl fmt::Debug for FingerprintKeyState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Available(key) => formatter.debug_tuple("Available").field(key).finish(),
            Self::Unknown(reason) => formatter.debug_tuple("Unknown").field(reason).finish(),
        }
    }
}

/// An in-memory capability for producing keyed plan digests.
///
/// The key has no byte accessor and deliberately implements neither serde
/// trait. Its `Debug` representation is always redacted.
pub struct FingerprintKey([u8; FINGERPRINT_KEY_LENGTH]);

impl fmt::Debug for FingerprintKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FingerprintKey([REDACTED])")
    }
}

impl FingerprintKey {
    /// Computes `HMAC-SHA256` over canonical plan bytes.
    ///
    /// The returned spelling is suitable for persisted fingerprint fields;
    /// neither the input nor key is retained by the result.
    #[must_use]
    pub fn digest(&self, canonical_plan: &[u8]) -> KeyedDigest {
        self.digest_plan(PlanFingerprintCategory::Uncategorized, [canonical_plan])
    }

    /// Computes a domain-separated fingerprint from ordered planner inputs.
    ///
    /// Each input is length-prefixed before hashing, so different input
    /// boundaries cannot produce the same canonical message. Callers pass
    /// borrowed canonical bytes; no unkeyed intermediate digest is exposed.
    #[must_use]
    pub fn digest_plan<'a>(
        &self,
        category: PlanFingerprintCategory,
        inputs: impl IntoIterator<Item = &'a [u8]>,
    ) -> KeyedDigest {
        type HmacSha256 = Hmac<Sha256>;
        let Ok(mut mac) = HmacSha256::new_from_slice(&self.0) else {
            unreachable!("HMAC-SHA256 accepts a fixed 32-byte key");
        };
        mac.update(b"cdenv-plan-fingerprint-v1\0");
        mac.update(category.domain().as_bytes());
        for input in inputs {
            mac.update(&(input.len() as u64).to_be_bytes());
            mac.update(input);
        }
        KeyedDigest(format!(
            "keyed:{}",
            hex::encode(mac.finalize().into_bytes())
        ))
    }

    /// Fingerprints every canonical category without exposing unkeyed digests or plan bytes.
    #[must_use]
    pub fn digest_immutable_plan(
        &self,
        plan: &cdenv_devcontainer::ImmutablePlan,
    ) -> cdenv_devcontainer::CategoryFingerprints<KeyedDigest> {
        plan.fingerprint_with(|category, bytes| {
            let category = match category {
                cdenv_devcontainer::PlanCategory::Build => PlanFingerprintCategory::Build,
                cdenv_devcontainer::PlanCategory::Create => PlanFingerprintCategory::Create,
                cdenv_devcontainer::PlanCategory::Runtime => PlanFingerprintCategory::Runtime,
                cdenv_devcontainer::PlanCategory::Lifecycle => PlanFingerprintCategory::Lifecycle,
            };
            self.digest_plan(category, [bytes])
        })
    }
}

/// A domain separating one immutable effective-plan category.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PlanFingerprintCategory {
    /// Compatibility mode for hashing one already-canonical byte sequence.
    Uncategorized,
    /// Image, Features, metadata, and build options.
    Build,
    /// Container creation options, mounts, user, and environment.
    Create,
    /// Remote environment, forwarding, and attach behavior.
    Runtime,
    /// Immutable generation-owned lifecycle commands.
    Lifecycle,
}

impl PlanFingerprintCategory {
    const fn domain(self) -> &'static str {
        match self {
            Self::Uncategorized => "uncategorized\0",
            Self::Build => "build\0",
            Self::Create => "create\0",
            Self::Runtime => "runtime\0",
            Self::Lifecycle => "lifecycle\0",
        }
    }
}

/// A malformed persisted keyed digest.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("a keyed digest must be `keyed:` followed by 64 lowercase hexadecimal characters")]
pub struct KeyedDigestError;

/// A keyed digest safe to serialize in persisted state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct KeyedDigest(String);

impl KeyedDigest {
    /// Parses an opaque keyed digest without exposing digest bytes.
    ///
    /// # Errors
    ///
    /// Returns [`KeyedDigestError`] unless the value has the exact persisted
    /// keyed-fingerprint spelling.
    pub fn parse(value: &str) -> Result<Self, KeyedDigestError> {
        let Some(hex) = value.strip_prefix("keyed:") else {
            return Err(KeyedDigestError);
        };
        if hex.len() != 64
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(KeyedDigestError);
        }
        Ok(Self(value.to_owned()))
    }

    /// Returns the `keyed:<hex>` representation.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for KeyedDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Display for KeyedDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// A loaded installation and its checked fingerprint capability.
#[derive(Debug)]
pub struct Installation {
    record: InstallationRecord,
    fingerprint_key: FingerprintKeyState,
}

impl Installation {
    /// Creates a new installation or securely reopens an existing one.
    ///
    /// A missing or replaced key on an existing installation is not silently
    /// regenerated: [`FingerprintKeyState::Unknown`] tells mutating callers to
    /// invalidate old plan fingerprints.
    ///
    /// # Errors
    ///
    /// Returns [`InstallationError`] for unsafe paths, I/O, invalid JSON,
    /// unsupported schemas, or unavailable randomness.
    pub fn open_or_create(root: &CdenvRoot) -> Result<Self, InstallationError> {
        ensure_private_directory(root.as_path())?;
        let installation_path = root.installation_file();
        match inspect_managed_path(&installation_path, ManagedPathKind::File, current_user_id())? {
            ManagedPathState::Missing => Self::create(root),
            ManagedPathState::Valid => Self::load(root),
        }
    }

    /// Returns persistent operational installation metadata.
    #[must_use]
    pub const fn record(&self) -> &InstallationRecord {
        &self.record
    }

    /// Returns the checked keyed-digest capability.
    #[must_use]
    pub const fn fingerprint_key(&self) -> &FingerprintKeyState {
        &self.fingerprint_key
    }

    /// Atomically records a new SSH Include consent choice.
    ///
    /// # Errors
    ///
    /// Returns [`InstallationError`] if serialization or durable replacement
    /// fails. The in-memory record changes only after persistence succeeds.
    pub fn set_ssh_include_consent(
        &mut self,
        root: &CdenvRoot,
        consent: SshIncludeConsent,
    ) -> Result<(), InstallationError> {
        let mut replacement = self.record.clone();
        replacement.ssh_include_consent = consent;
        write_record(&root.installation_file(), &replacement)?;
        self.record = replacement;
        Ok(())
    }

    fn create(root: &CdenvRoot) -> Result<Self, InstallationError> {
        let key = random_key()?;
        let record = InstallationRecord {
            schema_version: INSTALLATION_SCHEMA_VERSION,
            installation_id: random_installation_id()?,
            ssh_include_consent: SshIncludeConsent::Unknown,
            fingerprint_key_id: key_id(&key.0),
        };
        atomic_write(&root.fingerprint_key(), &key.0, ManagedMode::PrivateFile)?;
        write_record(&root.installation_file(), &record)?;
        Ok(Self {
            record,
            fingerprint_key: FingerprintKeyState::Available(key),
        })
    }

    fn load(root: &CdenvRoot) -> Result<Self, InstallationError> {
        let installation_path = root.installation_file();
        let bytes = read_managed_file(&installation_path)?
            .ok_or_else(|| InstallationError::MissingRecord(installation_path.clone()))?;
        let record: InstallationRecord =
            serde_json::from_slice(&bytes).map_err(|source| InstallationError::Parse {
                path: installation_path.clone(),
                source,
            })?;
        if record.schema_version != INSTALLATION_SCHEMA_VERSION {
            return Err(InstallationError::UnsupportedSchema {
                path: installation_path,
                found: record.schema_version,
                supported: INSTALLATION_SCHEMA_VERSION,
            });
        }
        tighten_managed_file(&root.installation_file(), ManagedMode::PrivateFile)?;

        let fingerprint_key = match read_managed_file(&root.fingerprint_key())? {
            None => FingerprintKeyState::Unknown(FingerprintKeyUnknownReason::Missing),
            Some(bytes) => {
                tighten_managed_file(&root.fingerprint_key(), ManagedMode::PrivateFile)?;
                if bytes.len() != FINGERPRINT_KEY_LENGTH
                    || key_id(&bytes) != record.fingerprint_key_id
                {
                    FingerprintKeyState::Unknown(FingerprintKeyUnknownReason::Replaced)
                } else {
                    let mut key = [0_u8; FINGERPRINT_KEY_LENGTH];
                    key.copy_from_slice(&bytes);
                    FingerprintKeyState::Available(FingerprintKey(key))
                }
            }
        };
        Ok(Self {
            record,
            fingerprint_key,
        })
    }
}

/// A failure to create or load installation metadata.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum InstallationError {
    /// Secure filesystem storage failed.
    #[error(transparent)]
    Storage(#[from] StorageError),
    /// Managed-path inspection failed.
    #[error(transparent)]
    ManagedPath(#[from] ManagedPathError),
    /// Reading an inspected managed file failed.
    #[error("cannot read managed installation file {path:?}: {source}")]
    Read {
        /// The file path.
        path: PathBuf,
        /// The operating-system error.
        #[source]
        source: io::Error,
    },
    /// Installation JSON is invalid.
    #[error("cannot parse installation record {path:?}: {source}")]
    Parse {
        /// The record path.
        path: PathBuf,
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// Installation JSON cannot be encoded.
    #[error("cannot serialize installation record: {source}")]
    Serialize {
        /// The JSON error.
        #[source]
        source: serde_json::Error,
    },
    /// A concurrently removed record disappeared after inspection.
    #[error("installation record disappeared while opening it: {0:?}")]
    MissingRecord(PathBuf),
    /// The record belongs to an unsupported schema.
    #[error(
        "installation record {path:?} uses schema {found}, but this build supports schema {supported}"
    )]
    UnsupportedSchema {
        /// The record path.
        path: PathBuf,
        /// The encountered schema.
        found: u32,
        /// The supported schema.
        supported: u32,
    },
    /// Cryptographic randomness was unavailable.
    #[error("cannot generate installation identity material: {source}")]
    Random {
        /// The random-source failure.
        #[source]
        source: getrandom::Error,
    },
    /// Generated random identity text violated the domain invariant.
    #[error("generated installation ID was invalid: {source}")]
    GeneratedIdentity {
        /// The identity parser failure.
        #[source]
        source: cdenv_core::InstallationIdError,
    },
}

fn write_record(path: &Path, record: &InstallationRecord) -> Result<(), InstallationError> {
    let mut bytes = serde_json::to_vec_pretty(record)
        .map_err(|source| InstallationError::Serialize { source })?;
    bytes.push(b'\n');
    atomic_write(path, &bytes, ManagedMode::PrivateFile)?;
    Ok(())
}

fn read_managed_file(path: &Path) -> Result<Option<Vec<u8>>, InstallationError> {
    match inspect_managed_path(path, ManagedPathKind::File, current_user_id())? {
        ManagedPathState::Missing => Ok(None),
        ManagedPathState::Valid => {
            fs::read(path)
                .map(Some)
                .map_err(|source| InstallationError::Read {
                    path: path.to_path_buf(),
                    source,
                })
        }
    }
}

fn random_key() -> Result<FingerprintKey, InstallationError> {
    let mut bytes = [0_u8; FINGERPRINT_KEY_LENGTH];
    getrandom::fill(&mut bytes).map_err(|source| InstallationError::Random { source })?;
    Ok(FingerprintKey(bytes))
}

fn random_installation_id() -> Result<InstallationId, InstallationError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(|source| InstallationError::Random { source })?;
    InstallationId::parse(&hex::encode(bytes))
        .map_err(|source| InstallationError::GeneratedIdentity { source })
}

fn key_id(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(unix)]
#[expect(
    clippy::unnecessary_wraps,
    reason = "the cross-platform ownership API uses None on non-Unix hosts"
)]
fn current_user_id() -> Option<u32> {
    Some(nix::unistd::geteuid().as_raw())
}

#[cfg(not(unix))]
const fn current_user_id() -> Option<u32> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RootEnvironment, RootResolutionError};

    struct NoEnvironment;
    impl RootEnvironment for NoEnvironment {
        fn cdenv_home(&self) -> Option<std::ffi::OsString> {
            None
        }
        fn home_dir(&self) -> Option<PathBuf> {
            None
        }
    }

    fn test_root(directory: &Path) -> Result<CdenvRoot, RootResolutionError> {
        CdenvRoot::resolve(Some(directory), &NoEnvironment)
    }

    #[test]
    fn reopening_preserves_identity_consent_and_keyed_digest() {
        let temporary = tempfile::tempdir().expect("temporary parent should exist");
        let root = test_root(&temporary.path().join("cdenv")).expect("root should validate");
        let mut first =
            Installation::open_or_create(&root).expect("installation should initialize");
        first
            .set_ssh_include_consent(&root, SshIncludeConsent::Accepted)
            .expect("consent should persist");
        let first_digest = match first.fingerprint_key() {
            FingerprintKeyState::Available(key) => key.digest(b"canonical plan"),
            FingerprintKeyState::Unknown(reason) => {
                panic!("new key unexpectedly unknown: {reason:?}")
            }
        };
        let identity = first.record().installation_id().clone();

        let reopened = Installation::open_or_create(&root).expect("installation should reopen");
        let reopened_digest = match reopened.fingerprint_key() {
            FingerprintKeyState::Available(key) => key.digest(b"canonical plan"),
            FingerprintKeyState::Unknown(reason) => {
                panic!("stored key unexpectedly unknown: {reason:?}")
            }
        };
        assert_eq!(
            (
                reopened.record().installation_id(),
                reopened.record().ssh_include_consent(),
                reopened_digest,
            ),
            (&identity, SshIncludeConsent::Accepted, first_digest)
        );
    }

    #[test]
    fn missing_key_marks_fingerprints_unknown_without_regeneration() {
        let temporary = tempfile::tempdir().expect("temporary parent should exist");
        let root = test_root(&temporary.path().join("cdenv")).expect("root should validate");
        Installation::open_or_create(&root).expect("installation should initialize");
        fs::remove_file(root.fingerprint_key()).expect("key should be removed");

        let reopened = Installation::open_or_create(&root).expect("installation should reopen");

        assert!(matches!(
            reopened.fingerprint_key(),
            FingerprintKeyState::Unknown(FingerprintKeyUnknownReason::Missing)
        ));
    }

    #[test]
    fn replaced_key_marks_fingerprints_unknown() {
        let temporary = tempfile::tempdir().expect("temporary parent should exist");
        let root = test_root(&temporary.path().join("cdenv")).expect("root should validate");
        Installation::open_or_create(&root).expect("installation should initialize");
        atomic_write(
            &root.fingerprint_key(),
            &[42; FINGERPRINT_KEY_LENGTH],
            ManagedMode::PrivateFile,
        )
        .expect("replacement key should be installed");

        let reopened = Installation::open_or_create(&root).expect("installation should reopen");

        assert!(matches!(
            reopened.fingerprint_key(),
            FingerprintKeyState::Unknown(FingerprintKeyUnknownReason::Replaced)
        ));
    }

    #[test]
    fn debug_output_never_contains_key_bytes() {
        let key = FingerprintKey([0xab; FINGERPRINT_KEY_LENGTH]);
        assert_eq!(format!("{key:?}"), "FingerprintKey([REDACTED])");
    }

    #[test]
    fn keyed_digest_deserialization_rejects_unkeyed_or_malformed_values() {
        for value in [
            r#""sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa""#,
            r#""keyed:short""#,
            r#""keyed:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA""#,
        ] {
            assert!(serde_json::from_str::<KeyedDigest>(value).is_err());
        }
    }
}
