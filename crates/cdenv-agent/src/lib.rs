//! Testable container-side boundary for `cdenv-agent`.
//!
//! Distribution targets are Linux. Platform-specific process and PTY behavior
//! remains isolated behind compile-time platform modules.

use serde::Serialize;
use thiserror::Error;

/// The protocol spoken by this agent release.
pub const PROTOCOL_VERSION: u32 = 1;

/// The build identity supplied by the release build pipeline.
///
/// Development builds intentionally use a stable nonempty value so the agent
/// remains runnable without `xtask` staging.
pub const BUILD_ID: &str = match option_env!("CDENV_AGENT_BUILD_ID") {
    Some(build_id) if !build_id.is_empty() => build_id,
    _ => "development",
};

/// Machine-readable identity emitted by `cdenv-agent version`.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Version<'a> {
    /// Executable name.
    pub name: &'static str,
    /// Cargo package version.
    pub version: &'static str,
    /// Host-to-agent compatibility protocol.
    pub protocol_version: u32,
    /// Build pipeline identity.
    pub build_id: &'a str,
}

/// Returns this executable's machine-readable identity.
#[must_use]
pub const fn version() -> Version<'static> {
    Version {
        name: "cdenv-agent",
        version: env!("CARGO_PKG_VERSION"),
        protocol_version: PROTOCOL_VERSION,
        build_id: BUILD_ID,
    }
}

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
        reason = "shared with unsupported-host stubs"
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
    #[test]
    fn version_has_the_machine_contract() {
        let identity = super::version();
        assert_eq!(identity.name, "cdenv-agent");
        assert_eq!(identity.protocol_version, 1);
        assert!(!identity.build_id.is_empty());
    }

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
