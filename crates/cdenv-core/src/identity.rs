//! Validated identities shared by the host and container agent.

use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::str::FromStr;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

/// The reason an opaque string identity is invalid.
///
/// Opaque identities are non-empty ASCII tokens. Their first and last
/// characters are alphanumeric; `-`, `_`, and `.` are also accepted between
/// those boundaries.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum IdentityTokenError {
    /// The identity is empty.
    #[error("the value is empty")]
    Empty,
    /// The first character is not an ASCII letter or digit.
    #[error("the first character {character:?} is not an ASCII letter or digit")]
    InvalidStart {
        /// The invalid first character.
        character: char,
    },
    /// The final character is not an ASCII letter or digit.
    #[error("the final character {character:?} is not an ASCII letter or digit")]
    InvalidEnd {
        /// The invalid final character.
        character: char,
    },
    /// A character is outside the identity token alphabet.
    #[error(
        "character {character:?} at byte {index} is not an ASCII letter, digit, hyphen, underscore, or period"
    )]
    InvalidCharacter {
        /// The byte index of the invalid character.
        index: usize,
        /// The invalid character.
        character: char,
    },
}

fn validate_identity_token(value: &str) -> Result<(), IdentityTokenError> {
    let mut characters = value.chars();
    let Some(first) = characters.next() else {
        return Err(IdentityTokenError::Empty);
    };

    if !first.is_ascii_alphanumeric() {
        return Err(IdentityTokenError::InvalidStart { character: first });
    }

    for (index, character) in value.char_indices() {
        if !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')) {
            return Err(IdentityTokenError::InvalidCharacter { index, character });
        }
    }

    let Some(last) = value.chars().next_back() else {
        return Err(IdentityTokenError::Empty);
    };

    if !last.is_ascii_alphanumeric() {
        return Err(IdentityTokenError::InvalidEnd { character: last });
    }

    Ok(())
}

macro_rules! define_token_identity {
    (
        $(#[$type_meta:meta])*
        $type_name:ident,
        $error_name:ident,
        $error_doc:literal,
        $error_message:literal
    ) => {
        $(#[$type_meta])*
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $type_name(String);

        #[doc = $error_doc]
        #[derive(Clone, Debug, Error, PartialEq, Eq)]
        #[error($error_message)]
        pub struct $error_name {
            #[source]
            reason: IdentityTokenError,
        }

        impl $error_name {
            /// Returns the token rule that the value violated.
            #[must_use]
            pub const fn reason(&self) -> &IdentityTokenError {
                &self.reason
            }
        }

        impl $type_name {
            /// Parses and validates an identity.
            ///
            /// # Errors
            ///
            /// Returns an error when `value` is empty, has a non-alphanumeric
            /// boundary, or contains a character outside the identity token
            /// alphabet.
            pub fn parse(value: &str) -> Result<Self, $error_name> {
                Self::try_from(value)
            }

            /// Returns the validated identity text.
            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $type_name {
            fn as_ref(&self) -> &str {
                self.as_str()
            }
        }

        impl std::borrow::Borrow<str> for $type_name {
            fn borrow(&self) -> &str {
                self.as_str()
            }
        }

        impl fmt::Display for $type_name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $type_name {
            type Err = $error_name;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::parse(value)
            }
        }

        impl TryFrom<&str> for $type_name {
            type Error = $error_name;

            fn try_from(value: &str) -> Result<Self, Self::Error> {
                validate_identity_token(value).map_err(|reason| $error_name { reason })?;
                Ok(Self(value.to_owned()))
            }
        }

        impl TryFrom<String> for $type_name {
            type Error = $error_name;

            fn try_from(value: String) -> Result<Self, Self::Error> {
                validate_identity_token(&value).map_err(|reason| $error_name { reason })?;
                Ok(Self(value))
            }
        }

        impl Serialize for $type_name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $type_name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: Deserializer<'de>,
            {
                let value = String::deserialize(deserializer)?;
                Self::try_from(value).map_err(serde::de::Error::custom)
            }
        }
    };
}

define_token_identity!(
    /// A stable, installation-scoped identity.
    ///
    /// The value is intentionally opaque. Code may compare or serialize it but
    /// must not infer installation properties from its spelling.
    InstallationId,
    InstallationIdError,
    "An error returned when parsing an [`InstallationId`].",
    "invalid installation ID: {reason}"
);

define_token_identity!(
    /// A build identity shared by a host binary and its embedded agents.
    ///
    /// Build IDs are opaque and case-sensitive.
    AgentBuildId,
    AgentBuildIdError,
    "An error returned when parsing an [`AgentBuildId`].",
    "invalid agent build ID: {reason}"
);

define_token_identity!(
    /// The identity of a versioned compatibility profile.
    ///
    /// A profile ID identifies semantics; it is not display metadata from a
    /// `devcontainer.json` file.
    ProfileId,
    ProfileIdError,
    "An error returned when parsing a [`ProfileId`].",
    "invalid profile ID: {reason}"
);

/// A full Docker container ID.
///
/// Docker reports container IDs as 64 lowercase hexadecimal characters. This
/// type deliberately rejects abbreviated IDs so an identity comparison cannot
/// become ambiguous.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContainerId(String);

/// An error returned when parsing a [`ContainerId`].
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContainerIdError {
    /// The ID does not contain exactly 64 bytes.
    #[error(
        "a container ID must contain exactly 64 lowercase hexadecimal characters, found {length}"
    )]
    InvalidLength {
        /// The number of bytes in the supplied value.
        length: usize,
    },
    /// The ID contains a character outside lowercase hexadecimal.
    #[error("container ID character {character:?} at byte {index} is not lowercase hexadecimal")]
    InvalidCharacter {
        /// The byte index of the invalid character.
        index: usize,
        /// The invalid character.
        character: char,
    },
}

impl ContainerId {
    /// Parses a full Docker container ID.
    ///
    /// # Errors
    ///
    /// Returns [`ContainerIdError`] unless `value` is exactly 64 lowercase
    /// hexadecimal ASCII characters.
    pub fn parse(value: &str) -> Result<Self, ContainerIdError> {
        Self::try_from(value)
    }

    /// Returns the full validated container ID.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn validate_container_id(value: &str) -> Result<(), ContainerIdError> {
    if value.len() != 64 {
        return Err(ContainerIdError::InvalidLength {
            length: value.len(),
        });
    }

    for (index, character) in value.char_indices() {
        if !(character.is_ascii_digit() || matches!(character, 'a'..='f')) {
            return Err(ContainerIdError::InvalidCharacter { index, character });
        }
    }

    Ok(())
}

impl AsRef<str> for ContainerId {
    fn as_ref(&self) -> &str {
        self.as_str()
    }
}

impl std::borrow::Borrow<str> for ContainerId {
    fn borrow(&self) -> &str {
        self.as_str()
    }
}

impl fmt::Display for ContainerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ContainerId {
    type Err = ContainerIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for ContainerId {
    type Error = ContainerIdError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        validate_container_id(value)?;
        Ok(Self(value.to_owned()))
    }
}

impl TryFrom<String> for ContainerId {
    type Error = ContainerIdError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        validate_container_id(&value)?;
        Ok(Self(value))
    }
}

impl Serialize for ContainerId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ContainerId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value).map_err(serde::de::Error::custom)
    }
}

/// A positive workspace environment generation.
///
/// Generation 1 is the first provisioned environment. A successful replacement
/// increments the value; zero is never a persisted generation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GenerationId(NonZeroU64);

/// An error returned when constructing a zero [`GenerationId`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a generation ID must be greater than zero")]
pub struct GenerationIdError;

impl GenerationId {
    /// Creates a positive generation ID.
    ///
    /// # Errors
    ///
    /// Returns [`GenerationIdError`] when `value` is zero.
    pub const fn new(value: u64) -> Result<Self, GenerationIdError> {
        match NonZeroU64::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(GenerationIdError),
        }
    }

    /// Returns the positive integer generation.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

impl fmt::Display for GenerationId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

impl TryFrom<u64> for GenerationId {
    type Error = GenerationIdError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<GenerationId> for u64 {
    fn from(value: GenerationId) -> Self {
        value.get()
    }
}

impl Serialize for GenerationId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u64(self.get())
    }
}

impl<'de> Deserialize<'de> for GenerationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A positive host-to-agent protocol version.
///
/// The value represents protocol identity only. Whether a particular version is
/// supported is decided when peers compare their versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolVersion(NonZeroU32);

/// An error returned when constructing a zero [`ProtocolVersion`].
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("a protocol version must be greater than zero")]
pub struct ProtocolVersionError;

impl ProtocolVersion {
    /// Creates a positive protocol version.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolVersionError`] when `value` is zero.
    pub const fn new(value: u32) -> Result<Self, ProtocolVersionError> {
        match NonZeroU32::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(ProtocolVersionError),
        }
    }

    /// Returns the positive integer protocol version.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

impl fmt::Display for ProtocolVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(formatter)
    }
}

impl TryFrom<u32> for ProtocolVersion {
    type Error = ProtocolVersionError;

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<ProtocolVersion> for u32 {
    fn from(value: ProtocolVersion) -> Self {
        value.get()
    }
}

impl Serialize for ProtocolVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_u32(self.get())
    }
}

impl<'de> Deserialize<'de> for ProtocolVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// A supported Linux container architecture.
///
/// Parsing accepts only Docker's and Rust's exact names for x86-64 and ARM64.
/// Serialization uses Rust's canonical architecture name.
///
/// # Examples
///
/// ```
/// use cdenv_core::ContainerArchitecture;
///
/// let architecture: ContainerArchitecture = "amd64".parse()?;
/// assert_eq!(architecture.musl_target(), "x86_64-unknown-linux-musl");
/// # Ok::<(), cdenv_core::UnsupportedContainerArchitecture>(())
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ContainerArchitecture {
    /// The x86-64 architecture (`amd64` or `x86_64`).
    X86_64,
    /// The 64-bit ARM architecture (`arm64` or `aarch64`).
    Aarch64,
}

/// An error returned for a container architecture that cdenv cannot provision.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error(
    "unsupported container architecture {architecture:?}; supported aliases are `amd64`, `x86_64`, `arm64`, and `aarch64`"
)]
pub struct UnsupportedContainerArchitecture {
    architecture: String,
}

impl UnsupportedContainerArchitecture {
    /// Returns the rejected architecture text.
    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

impl ContainerArchitecture {
    /// Parses an exact Docker or Rust architecture alias.
    ///
    /// # Errors
    ///
    /// Returns [`UnsupportedContainerArchitecture`] for every alias other than
    /// `amd64`, `x86_64`, `arm64`, and `aarch64`.
    pub fn parse(value: &str) -> Result<Self, UnsupportedContainerArchitecture> {
        Self::try_from(value)
    }

    /// Returns Rust's canonical architecture name.
    #[must_use]
    pub const fn canonical_name(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }

    /// Returns Docker's canonical architecture name.
    #[must_use]
    pub const fn docker_name(self) -> &'static str {
        match self {
            Self::X86_64 => "amd64",
            Self::Aarch64 => "arm64",
        }
    }

    /// Returns the exact static Linux agent target for this architecture.
    #[must_use]
    pub const fn musl_target(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64-unknown-linux-musl",
            Self::Aarch64 => "aarch64-unknown-linux-musl",
        }
    }
}

impl fmt::Display for ContainerArchitecture {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.canonical_name())
    }
}

impl FromStr for ContainerArchitecture {
    type Err = UnsupportedContainerArchitecture;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

impl TryFrom<&str> for ContainerArchitecture {
    type Error = UnsupportedContainerArchitecture;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "amd64" | "x86_64" => Ok(Self::X86_64),
            "arm64" | "aarch64" => Ok(Self::Aarch64),
            _ => Err(UnsupportedContainerArchitecture {
                architecture: value.to_owned(),
            }),
        }
    }
}

impl Serialize for ContainerArchitecture {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.canonical_name())
    }
}

impl<'de> Deserialize<'de> for ContainerArchitecture {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_from(value.as_str()).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AgentBuildId, ContainerArchitecture, ContainerId, ContainerIdError, GenerationId,
        IdentityTokenError, InstallationId, ProfileId, ProtocolVersion,
    };

    const CONTAINER_ID: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn installation_id_accepts_a_safe_opaque_token() {
        let result = InstallationId::parse("018f0f87-70d8-7c52-a4d8-1f4fd1c45e30");

        assert_eq!(
            result.map(|identity| identity.to_string()),
            Ok("018f0f87-70d8-7c52-a4d8-1f4fd1c45e30".to_owned())
        );
    }

    #[test]
    fn agent_build_id_rejects_an_empty_value() {
        let error = AgentBuildId::parse("").expect_err("an empty build ID must fail");

        assert_eq!(error.reason(), &IdentityTokenError::Empty);
    }

    #[test]
    fn profile_id_accepts_the_v1_profile_identity() {
        let result = ProfileId::parse("cdenv-devcontainer-v1");

        assert_eq!(
            result.map(|identity| identity.to_string()),
            Ok("cdenv-devcontainer-v1".to_owned())
        );
    }

    #[test]
    fn profile_id_rejects_unicode() {
        let error =
            ProfileId::parse("cdenv-dévelopment-v1").expect_err("a non-ASCII profile ID must fail");

        assert!(
            matches!(error.reason(), IdentityTokenError::InvalidCharacter { .. }),
            "unexpected profile ID error: {error:?}"
        );
    }

    #[test]
    fn identity_token_rejects_a_non_alphanumeric_start() {
        let error = AgentBuildId::parse("-build1")
            .expect_err("a build ID with a punctuation boundary must fail");

        assert_eq!(
            error.reason(),
            &IdentityTokenError::InvalidStart { character: '-' }
        );
    }

    #[test]
    fn identity_token_rejects_a_non_alphanumeric_end() {
        let error = AgentBuildId::parse("build1.")
            .expect_err("a build ID with a punctuation boundary must fail");

        assert_eq!(
            error.reason(),
            &IdentityTokenError::InvalidEnd { character: '.' }
        );
    }

    #[test]
    fn installation_id_serde_round_trip_preserves_the_value() {
        let identity = InstallationId::parse("018f0f87-70d8-7c52-a4d8-1f4fd1c45e30")
            .expect("the test installation identity should be valid");
        let json = serde_json::to_string(&identity).expect("serialization should succeed");
        let decoded: InstallationId =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(decoded, identity);
    }

    #[test]
    fn installation_id_serde_rejects_an_invalid_value() {
        let result = serde_json::from_str::<InstallationId>(r#""line\nbreak""#);

        assert!(result.is_err());
    }

    #[test]
    fn agent_build_id_serde_round_trip_preserves_the_value() {
        let identity = AgentBuildId::parse("release_0.1.0-a1")
            .expect("the test build identity should be valid");
        let json = serde_json::to_string(&identity).expect("serialization should succeed");
        let decoded: AgentBuildId =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(decoded, identity);
    }

    #[test]
    fn agent_build_id_serde_rejects_an_invalid_value() {
        let result = serde_json::from_str::<AgentBuildId>(r#""build/id""#);

        assert!(result.is_err());
    }

    #[test]
    fn profile_id_serde_round_trip_preserves_the_value() {
        let identity = ProfileId::parse("cdenv-devcontainer-v1")
            .expect("the test profile identity should be valid");
        let json = serde_json::to_string(&identity).expect("serialization should succeed");
        let decoded: ProfileId =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(decoded, identity);
    }

    #[test]
    fn profile_id_serde_rejects_an_invalid_value() {
        let result = serde_json::from_str::<ProfileId>(r#""cdenv profile""#);

        assert!(result.is_err());
    }

    #[test]
    fn container_id_accepts_a_full_lowercase_sha256_identifier() {
        let result = ContainerId::parse(CONTAINER_ID);

        assert_eq!(
            result.map(|identity| identity.to_string()),
            Ok(CONTAINER_ID.to_owned())
        );
    }

    #[test]
    fn container_id_rejects_an_abbreviated_identifier() {
        let error =
            ContainerId::parse("0123456789ab").expect_err("an abbreviated container ID must fail");

        assert_eq!(error, ContainerIdError::InvalidLength { length: 12 });
    }

    #[test]
    fn container_id_rejects_uppercase_hexadecimal() {
        let mut value = CONTAINER_ID.to_owned();
        value.replace_range(4..5, "A");
        let error = ContainerId::parse(&value)
            .expect_err("uppercase hexadecimal in a container ID must fail");

        assert_eq!(
            error,
            ContainerIdError::InvalidCharacter {
                index: 4,
                character: 'A'
            }
        );
    }

    #[test]
    fn container_id_serde_round_trip_preserves_the_value() {
        let identity =
            ContainerId::parse(CONTAINER_ID).expect("the test container ID should be valid");
        let json = serde_json::to_string(&identity).expect("serialization should succeed");
        let decoded: ContainerId =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!(decoded, identity);
    }

    #[test]
    fn container_id_serde_rejects_invalid_serialized_text() {
        let result = serde_json::from_str::<ContainerId>(r#""not-a-container-id""#);

        assert!(result.is_err());
    }

    #[test]
    fn generation_id_accepts_one() {
        assert_eq!(GenerationId::new(1).map(GenerationId::get), Ok(1));
    }

    #[test]
    fn generation_id_rejects_zero() {
        assert!(GenerationId::new(0).is_err());
    }

    #[test]
    fn generation_id_serde_round_trip_is_numeric() {
        let generation = GenerationId::new(3).expect("three is a valid generation");
        let json = serde_json::to_string(&generation).expect("serialization should succeed");
        let decoded: GenerationId =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!((json, decoded), ("3".to_owned(), generation));
    }

    #[test]
    fn generation_id_serde_rejects_zero() {
        assert!(serde_json::from_str::<GenerationId>("0").is_err());
    }

    #[test]
    fn protocol_version_accepts_one() {
        assert_eq!(ProtocolVersion::new(1).map(ProtocolVersion::get), Ok(1));
    }

    #[test]
    fn protocol_version_rejects_zero() {
        assert!(ProtocolVersion::new(0).is_err());
    }

    #[test]
    fn protocol_version_serde_round_trip_is_numeric() {
        let version = ProtocolVersion::new(1).expect("one is a valid protocol version");
        let json = serde_json::to_string(&version).expect("serialization should succeed");
        let decoded: ProtocolVersion =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!((json, decoded), ("1".to_owned(), version));
    }

    #[test]
    fn protocol_version_serde_rejects_zero() {
        assert!(serde_json::from_str::<ProtocolVersion>("0").is_err());
    }

    #[test]
    fn architecture_maps_amd64_to_the_x86_64_musl_target() {
        let architecture =
            ContainerArchitecture::parse("amd64").expect("amd64 is a supported architecture alias");

        assert_eq!(architecture.musl_target(), "x86_64-unknown-linux-musl");
    }

    #[test]
    fn architecture_maps_x86_64_to_the_x86_64_musl_target() {
        let architecture = ContainerArchitecture::parse("x86_64")
            .expect("x86_64 is a supported architecture alias");

        assert_eq!(architecture.musl_target(), "x86_64-unknown-linux-musl");
    }

    #[test]
    fn architecture_maps_arm64_to_the_aarch64_musl_target() {
        let architecture =
            ContainerArchitecture::parse("arm64").expect("arm64 is a supported architecture alias");

        assert_eq!(architecture.musl_target(), "aarch64-unknown-linux-musl");
    }

    #[test]
    fn architecture_maps_aarch64_to_the_aarch64_musl_target() {
        let architecture = ContainerArchitecture::parse("aarch64")
            .expect("aarch64 is a supported architecture alias");

        assert_eq!(architecture.musl_target(), "aarch64-unknown-linux-musl");
    }

    #[test]
    fn architecture_rejects_every_unlisted_alias() {
        for alias in ["x64", "armv8", "AMD64", "riscv64", " amd64"] {
            assert!(
                ContainerArchitecture::parse(alias).is_err(),
                "unlisted alias {alias:?} should fail"
            );
        }
    }

    #[test]
    fn architecture_serde_round_trip_uses_the_canonical_name() {
        let architecture =
            ContainerArchitecture::parse("arm64").expect("arm64 is a supported architecture alias");
        let json = serde_json::to_string(&architecture).expect("serialization should succeed");
        let decoded: ContainerArchitecture =
            serde_json::from_str(&json).expect("deserialization should succeed");

        assert_eq!((json, decoded), (r#""aarch64""#.to_owned(), architecture));
    }

    #[test]
    fn architecture_serde_rejects_an_unsupported_alias() {
        assert!(serde_json::from_str::<ContainerArchitecture>(r#""riscv64""#).is_err());
    }
}
