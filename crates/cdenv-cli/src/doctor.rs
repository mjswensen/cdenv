//! Read-only local diagnostic checks for `cdenv doctor`.

use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::{Command, ExitCode};

use serde::Serialize;

use crate::{CdenvRoot, Installation, OutputFormat, render_json_success};

/// The outcome of one independent doctor check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum DoctorOutcome {
    /// The required invariant held.
    Pass,
    /// The check found advisory drift.
    Warn,
    /// A required invariant failed.
    Fail,
    /// The invariant could not be checked in this environment.
    NotCheckable,
}

impl DoctorOutcome {
    const fn required_failure(self) -> bool {
        matches!(self, Self::Fail)
    }

    const fn as_str(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Warn => "warn",
            Self::Fail => "fail",
            Self::NotCheckable => "not-checkable",
        }
    }
}

/// One deterministic, redacted doctor diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorCheck {
    category: &'static str,
    outcome: DoctorOutcome,
    message: String,
}

impl DoctorCheck {
    fn new(category: &'static str, outcome: DoctorOutcome, message: impl Into<String>) -> Self {
        Self {
            category,
            outcome,
            message: message.into(),
        }
    }
}

/// Versioned payload emitted by `cdenv doctor --json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Returns the checks in their stable presentation order.
    #[must_use]
    pub fn checks(&self) -> &[DoctorCheck] {
        &self.checks
    }

    /// Returns whether any required invariant failed.
    #[must_use]
    pub fn has_required_failure(&self) -> bool {
        self.checks
            .iter()
            .any(|check| check.outcome.required_failure())
    }
}

/// Collects independent checks without mutating the root, Docker, or processes.
#[must_use]
pub fn doctor_report(root: &CdenvRoot) -> DoctorReport {
    let mut checks = vec![
        root_check(root.as_path()),
        installation_check(root),
        directory_check("workspace-state", &root.workspaces_dir()),
        executable_check("git", "git", &["--version"]),
        executable_check("openssh", "ssh", &["-V"]),
        executable_check("docker", "docker", &["version"]),
        executable_check("compose", "docker", &["compose", "version"]),
        ssh_config_check(&root.ssh().config()),
    ];
    match crate::credentials::credential_reports(root) {
        Ok(reports) if reports.is_empty() => checks.push(DoctorCheck::new(
            "credentials",
            DoctorOutcome::Pass,
            "no credential permissions; no backend inspection performed",
        )),
        Ok(reports) => checks.extend(reports.into_iter().map(|report| {
            DoctorCheck::new(
                "credentials",
                if report.is_unavailable() {
                    DoctorOutcome::Fail
                } else if report.grants().is_empty() {
                    DoctorOutcome::Pass
                } else {
                    DoctorOutcome::Warn
                },
                report.diagnostic_summary(),
            )
        })),
        Err(error) => checks.push(DoctorCheck::new(
            "credentials",
            DoctorOutcome::Fail,
            error.to_string(),
        )),
    }
    DoctorReport { checks }
}

fn root_check(path: &Path) -> DoctorCheck {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            DoctorCheck::new("root", DoctorOutcome::Pass, "managed root is a directory")
        }
        Ok(_) => DoctorCheck::new(
            "root",
            DoctorOutcome::Fail,
            "managed root is not a real directory",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DoctorCheck::new("root", DoctorOutcome::Fail, "managed root does not exist")
        }
        Err(_) => DoctorCheck::new("root", DoctorOutcome::Fail, "managed root is unreadable"),
    }
}

fn installation_check(root: &CdenvRoot) -> DoctorCheck {
    match Installation::load_record_read_only(root) {
        Ok(record) => DoctorCheck::new(
            "installation",
            DoctorOutcome::Pass,
            format!(
                "installation schema {} with SSH consent {:?}",
                record.schema_version(),
                record.ssh_include_consent()
            ),
        ),
        Err(error) => DoctorCheck::new("installation", DoctorOutcome::Fail, error.to_string()),
    }
}

fn directory_check(category: &'static str, path: &Path) -> DoctorCheck {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
            DoctorCheck::new(
                category,
                DoctorOutcome::Pass,
                "managed directory is present",
            )
        }
        Ok(_) => DoctorCheck::new(category, DoctorOutcome::Fail, "managed path is unsafe"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            DoctorCheck::new(category, DoctorOutcome::Warn, "managed directory is absent")
        }
        Err(_) => DoctorCheck::new(
            category,
            DoctorOutcome::Fail,
            "managed directory is unreadable",
        ),
    }
}

fn executable_check(category: &'static str, program: &str, arguments: &[&str]) -> DoctorCheck {
    match Command::new(program).args(arguments).output() {
        Ok(output) if output.status.success() => DoctorCheck::new(
            category,
            DoctorOutcome::Pass,
            "required command is available",
        ),
        Ok(_) => DoctorCheck::new(category, DoctorOutcome::Fail, "required command failed"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck::new(
            category,
            DoctorOutcome::NotCheckable,
            "required command is unavailable",
        ),
        Err(_) => DoctorCheck::new(
            category,
            DoctorOutcome::NotCheckable,
            "cannot start command",
        ),
    }
}

fn ssh_config_check(path: &Path) -> DoctorCheck {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            DoctorCheck::new(
                "ssh-config",
                DoctorOutcome::Pass,
                "managed SSH config is present",
            )
        }
        Ok(_) => DoctorCheck::new(
            "ssh-config",
            DoctorOutcome::Fail,
            "managed SSH config is unsafe",
        ),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => DoctorCheck::new(
            "ssh-config",
            DoctorOutcome::Warn,
            "managed SSH config is absent",
        ),
        Err(_) => DoctorCheck::new(
            "ssh-config",
            DoctorOutcome::Fail,
            "managed SSH config is unreadable",
        ),
    }
}

/// Renders a doctor report and applies its required-invariant exit policy.
#[must_use]
pub fn render_doctor_report(
    format: OutputFormat,
    report: &DoctorReport,
    stdout: &mut (impl Write + ?Sized),
    stderr: &mut (impl Write + ?Sized),
) -> ExitCode {
    match format {
        OutputFormat::Json => {
            match render_json_success(stdout, &crate::SuccessEnvelope::new(report)) {
                Ok(()) => exit_code(report),
                Err(error) => {
                    let _ = writeln!(stderr, "cdenv: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        OutputFormat::Human => {
            for check in &report.checks {
                if writeln!(
                    stdout,
                    "{}\t{}\t{}",
                    check.outcome.as_str(),
                    check.category,
                    check.message
                )
                .is_err()
                {
                    let _ = writeln!(stderr, "cdenv: failed to write doctor output");
                    return ExitCode::FAILURE;
                }
            }
            exit_code(report)
        }
    }
}

fn exit_code(report: &DoctorReport) -> ExitCode {
    if report.has_required_failure() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}
