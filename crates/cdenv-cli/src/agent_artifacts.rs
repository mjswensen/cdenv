//! Embedded Linux agent artifact selection and validation.

use cdenv_core::{AgentBuildId, ContainerArchitecture, ProtocolVersion};
use thiserror::Error;

include!(concat!(env!("OUT_DIR"), "/agent_artifacts.rs"));

const X86_64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cdenv-agent-x86_64"));
const AARCH64: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/cdenv-agent-aarch64"));

/// Identity expected from an embedded agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentArtifactIdentity {
    build_id: AgentBuildId,
    protocol_version: ProtocolVersion,
}

impl AgentArtifactIdentity {
    /// Creates the identity a selected artifact must report.
    #[must_use]
    pub const fn new(build_id: AgentBuildId, protocol_version: ProtocolVersion) -> Self {
        Self {
            build_id,
            protocol_version,
        }
    }

    /// Returns the shared build ID.
    #[must_use]
    pub const fn build_id(&self) -> &AgentBuildId {
        &self.build_id
    }

    /// Returns the agent protocol version.
    #[must_use]
    pub const fn protocol_version(&self) -> ProtocolVersion {
        self.protocol_version
    }
}

/// An embedded or injected provider of static Linux agent artifacts.
///
/// Callers must run the selected artifact's `version` command after upload and
/// compare its output with [`AgentArtifactIdentity`]; byte inspection cannot
/// prove an executable's runtime identity.
#[derive(Clone, Copy, Debug)]
pub struct AgentArtifactProvider {
    x86_64: &'static [u8],
    aarch64: &'static [u8],
}

/// Errors returned when agent artifacts are unavailable or invalid.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum AgentArtifactError {
    /// Development builds did not stage release artifacts.
    #[error("Linux agent artifacts are not staged; run `cargo xtask build`")]
    Unavailable,
    /// The artifact has no bytes.
    #[error("agent artifact for {architecture} is empty")]
    Empty {
        /// Architecture selected for the empty artifact.
        architecture: ContainerArchitecture,
    },
    /// The artifact is not a 64-bit little-endian ELF executable.
    #[error("agent artifact for {architecture} is not a 64-bit little-endian ELF executable")]
    InvalidElf {
        /// Architecture selected for the malformed artifact.
        architecture: ContainerArchitecture,
    },
    /// The ELF machine does not match the selected container architecture.
    #[error("agent artifact machine does not match {architecture}")]
    WrongArchitecture {
        /// Architecture selected for the mismatched artifact.
        architecture: ContainerArchitecture,
    },
    /// A program interpreter proves the artifact is dynamically linked.
    #[error("agent artifact for {architecture} is dynamically linked")]
    Dynamic {
        /// Architecture selected for the dynamically linked artifact.
        architecture: ContainerArchitecture,
    },
}

impl AgentArtifactProvider {
    /// Returns the build identity compiled into this host binary.
    ///
    /// # Errors
    ///
    /// Returns the validation error only when a caller supplied an invalid
    /// `CDENV_BUILD_ID` while compiling the host.
    pub fn embedded_identity() -> Result<AgentBuildId, cdenv_core::AgentBuildIdError> {
        AgentBuildId::parse(HOST_BUILD_ID)
    }

    /// Returns the provider compiled into this host binary.
    #[must_use]
    pub const fn embedded() -> Self {
        Self {
            x86_64: X86_64,
            aarch64: AARCH64,
        }
    }

    /// Creates a provider for tests and development tooling.
    #[must_use]
    pub const fn staged(x86_64: &'static [u8], aarch64: &'static [u8]) -> Self {
        Self { x86_64, aarch64 }
    }

    /// Selects and validates the exact agent for a container architecture.
    ///
    /// # Errors
    ///
    /// Returns an error when no artifacts were embedded or the selected binary
    /// is empty, malformed, dynamically linked, or for another architecture.
    pub fn artifact(
        self,
        architecture: ContainerArchitecture,
    ) -> Result<&'static [u8], AgentArtifactError> {
        if !STAGED && self.x86_64.is_empty() && self.aarch64.is_empty() {
            return Err(AgentArtifactError::Unavailable);
        }
        let bytes = match architecture {
            ContainerArchitecture::X86_64 => self.x86_64,
            ContainerArchitecture::Aarch64 => self.aarch64,
        };
        validate_elf(bytes, architecture)?;
        Ok(bytes)
    }
}

fn validate_elf(
    bytes: &[u8],
    architecture: ContainerArchitecture,
) -> Result<(), AgentArtifactError> {
    if bytes.is_empty() {
        return Err(AgentArtifactError::Empty { architecture });
    }
    if bytes.len() < 64 || &bytes[..4] != b"\x7fELF" || bytes[4] != 2 || bytes[5] != 1 {
        return Err(AgentArtifactError::InvalidElf { architecture });
    }
    let machine = u16::from_le_bytes([bytes[18], bytes[19]]);
    let expected = match architecture {
        ContainerArchitecture::X86_64 => 62,
        ContainerArchitecture::Aarch64 => 183,
    };
    if machine != expected {
        return Err(AgentArtifactError::WrongArchitecture { architecture });
    }
    let program_offset = usize::try_from(u64::from_le_bytes(
        bytes[32..40]
            .try_into()
            .map_err(|_| AgentArtifactError::InvalidElf { architecture })?,
    ))
    .map_err(|_| AgentArtifactError::InvalidElf { architecture })?;
    let entry_size = usize::from(u16::from_le_bytes([bytes[54], bytes[55]]));
    let count = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    let table_end = program_offset
        .checked_add(
            entry_size
                .checked_mul(count)
                .ok_or(AgentArtifactError::InvalidElf { architecture })?,
        )
        .ok_or(AgentArtifactError::InvalidElf { architecture })?;
    if entry_size < 4 || table_end > bytes.len() {
        return Err(AgentArtifactError::InvalidElf { architecture });
    }
    if (0..count).any(|index| {
        u32::from_le_bytes(
            bytes[program_offset + index * entry_size..][..4]
                .try_into()
                .unwrap_or([0; 4]),
        ) == 3
    }) {
        return Err(AgentArtifactError::Dynamic { architecture });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn elf(machine: u16, interpreter: bool) -> Vec<u8> {
        let mut bytes = vec![0; 64 + 56];
        bytes[..4].copy_from_slice(b"\x7fELF");
        bytes[4] = 2;
        bytes[5] = 1;
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[32..40].copy_from_slice(&(64_u64).to_le_bytes());
        bytes[54..56].copy_from_slice(&(56_u16).to_le_bytes());
        bytes[56..58].copy_from_slice(&(1_u16).to_le_bytes());
        if interpreter {
            bytes[64..68].copy_from_slice(&(3_u32).to_le_bytes());
        }
        bytes
    }
    #[test]
    fn validation_rejects_bad_artifacts() {
        let arm = Box::leak(elf(183, false).into_boxed_slice());
        let empty = AgentArtifactProvider::staged(&[], arm);
        assert!(matches!(
            empty.artifact(ContainerArchitecture::X86_64),
            Err(AgentArtifactError::Empty { .. })
        ));
        let wrong =
            AgentArtifactProvider::staged(Box::leak(elf(183, false).into_boxed_slice()), arm);
        assert!(matches!(
            wrong.artifact(ContainerArchitecture::X86_64),
            Err(AgentArtifactError::WrongArchitecture { .. })
        ));
        let dynamic =
            AgentArtifactProvider::staged(Box::leak(elf(62, true).into_boxed_slice()), arm);
        assert!(matches!(
            dynamic.artifact(ContainerArchitecture::X86_64),
            Err(AgentArtifactError::Dynamic { .. })
        ));
    }
}
