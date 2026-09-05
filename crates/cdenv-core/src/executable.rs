//! Structural validation of release Linux agent executables.

use thiserror::Error;

/// A rejected Linux agent executable.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum StaticElfError {
    /// Missing or malformed ELF headers, segments, or executable payload.
    #[error("malformed ELF executable")]
    Malformed,
    /// ELF machine differs from the requested architecture.
    #[error("wrong ELF architecture")]
    WrongMachine,
    /// An interpreter or dynamic segment is present.
    #[error("dynamic ELF executable")]
    Dynamic,
}

/// Validates a little-endian ELF64 static executable for the given ELF machine.
///
/// This deliberately rejects dynamic segments, including static PIE, which the
/// release agent build does not produce. Runtime identity must be checked separately.
///
/// # Errors
///
/// Returns the structural, architecture, or linkage error found in the artifact.
pub fn validate_static_elf(bytes: &[u8], machine: u16) -> Result<(), StaticElfError> {
    use StaticElfError::Malformed;
    if bytes.len() < 64
        || &bytes[..7] != b"\x7fELF\x02\x01\x01"
        || !matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
        || bytes[20..24] != 1_u32.to_le_bytes()
        || bytes[24..32] == [0; 8]
        || bytes[52..54] != 64_u16.to_le_bytes()
    {
        return Err(Malformed);
    }
    if u16::from_le_bytes([bytes[18], bytes[19]]) != machine {
        return Err(StaticElfError::WrongMachine);
    }
    let offset = usize::try_from(u64::from_le_bytes(
        bytes[32..40].try_into().map_err(|_| Malformed)?,
    ))
    .map_err(|_| Malformed)?;
    let size = usize::from(u16::from_le_bytes([bytes[54], bytes[55]]));
    let count = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    let end = offset
        .checked_add(size.checked_mul(count).ok_or(Malformed)?)
        .ok_or(Malformed)?;
    if size != 56 || count == 0 || offset < 64 || end > bytes.len() {
        return Err(Malformed);
    }
    let entry = u64::from_le_bytes(bytes[24..32].try_into().map_err(|_| Malformed)?);
    let mut executable = false;
    for header in bytes[offset..end].chunks_exact(size) {
        let kind = u32::from_le_bytes(header[..4].try_into().map_err(|_| Malformed)?);
        if matches!(kind, 2 | 3) {
            return Err(StaticElfError::Dynamic);
        }
        if kind == 1 {
            let start = u64::from_le_bytes(header[8..16].try_into().map_err(|_| Malformed)?);
            let length = u64::from_le_bytes(header[32..40].try_into().map_err(|_| Malformed)?);
            let memory = u64::from_le_bytes(header[40..48].try_into().map_err(|_| Malformed)?);
            if length > memory
                || start
                    .checked_add(length)
                    .is_none_or(|end| end > bytes.len() as u64)
            {
                return Err(Malformed);
            }
            let address = u64::from_le_bytes(header[16..24].try_into().map_err(|_| Malformed)?);
            let end_address = address.checked_add(length).ok_or(Malformed)?;
            executable |= header[4] & 1 != 0 && (address..end_address).contains(&entry);
        }
    }
    if !executable {
        return Err(Malformed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0; 120];
        bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        bytes[16] = 2;
        bytes[18..20].copy_from_slice(&machine.to_le_bytes());
        bytes[20] = 1;
        bytes[24] = 1;
        bytes[32] = 64;
        bytes[52] = 64;
        bytes[54] = 56;
        bytes[56] = 1;
        bytes[64] = 1;
        bytes[68] = 1;
        bytes[96] = 120;
        bytes[104] = 120;
        bytes
    }

    #[test]
    fn accepts_both_static_agent_architectures() {
        for machine in [62, 183] {
            assert_eq!(validate_static_elf(&elf(machine), machine), Ok(()));
        }
    }

    #[test]
    fn rejects_interpreter_and_dynamic_segment_without_interpreter() {
        for kind in [2, 3] {
            let mut bytes = elf(62);
            bytes[64] = kind;
            assert_eq!(
                validate_static_elf(&bytes, 62),
                Err(StaticElfError::Dynamic)
            );
        }
    }

    #[test]
    fn rejects_truncated_missing_and_invalid_program_tables() {
        for length in 0..120 {
            assert!(validate_static_elf(&elf(62)[..length], 62).is_err());
        }
        for offset in [32, 52, 54, 56, 64, 68, 96, 104] {
            let mut bytes = elf(62);
            bytes[offset] = 0;
            assert!(validate_static_elf(&bytes, 62).is_err(), "offset {offset}");
        }
    }

    #[test]
    fn rejects_entry_point_outside_executable_segments() {
        let mut bytes = elf(62);
        bytes[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        assert_eq!(
            validate_static_elf(&bytes, 62),
            Err(StaticElfError::Malformed)
        );
    }

    #[test]
    fn rejects_wrong_machine() {
        assert_eq!(
            validate_static_elf(&elf(183), 62),
            Err(StaticElfError::WrongMachine)
        );
    }
}
