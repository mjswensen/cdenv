//! Testable host-application boundary for `cdenv`.

use std::process::ExitCode;

/// Runs the host application and returns its process exit status.
#[must_use]
pub const fn run() -> ExitCode {
    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::run;

    #[test]
    fn run_returns_success_for_the_baseline_invocation() {
        assert_eq!(run(), std::process::ExitCode::SUCCESS);
    }
}
