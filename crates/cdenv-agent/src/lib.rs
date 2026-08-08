//! Testable container-side boundary for `cdenv-agent`.
//!
//! Distribution targets are Linux. Platform-specific process and PTY behavior
//! remains isolated behind compile-time platform modules.

use thiserror::Error;

/// An error reported when the agent is invoked on an unsupported host.
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum PlatformError {
    /// The current operating system cannot run the distributed agent.
    #[error("cdenv-agent supports Linux only; current operating system is `{operating_system}`")]
    UnsupportedOperatingSystem {
        /// Rust's name for the current operating system.
        operating_system: &'static str,
    },
}

/// Verifies that the current host can run the agent.
///
/// # Errors
///
/// Returns [`PlatformError::UnsupportedOperatingSystem`] on non-Linux hosts.
pub fn ensure_supported_platform() -> Result<(), PlatformError> {
    platform::ensure_supported()
}

#[cfg(target_os = "linux")]
mod platform {
    use super::PlatformError;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "the implementation shares a fallible API with unsupported-host stubs"
    )]
    pub(super) const fn ensure_supported() -> Result<(), PlatformError> {
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
mod platform {
    use super::PlatformError;

    pub(super) const fn ensure_supported() -> Result<(), PlatformError> {
        Err(PlatformError::UnsupportedOperatingSystem {
            operating_system: std::env::consts::OS,
        })
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    #[test]
    fn ensure_supported_platform_accepts_linux() {
        assert_eq!(super::ensure_supported_platform(), Ok(()));
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn ensure_supported_platform_rejects_non_linux_hosts() {
        assert_eq!(
            super::ensure_supported_platform(),
            Err(super::PlatformError::UnsupportedOperatingSystem {
                operating_system: std::env::consts::OS,
            })
        );
    }
}
