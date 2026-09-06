//! Repository automation entry point.

use cdenv_core::executable::validate_static_elf;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};

mod package;
use package::package_distribution;

const QUALITY_COMMANDS: &[&[&str]] = &[
    &["fmt", "--check"],
    &[
        "clippy",
        "--workspace",
        "--all-targets",
        "--all-features",
        "--locked",
        "--",
        "-D",
        "warnings",
    ],
    &["test", "--workspace", "--locked"],
    &["doc", "--workspace", "--no-deps", "--locked"],
    &["deny", "check"],
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntegrationSuite {
    DevcontainerV1,
    Openssh,
}

impl IntegrationSuite {
    const fn name(self) -> &'static str {
        match self {
            Self::DevcontainerV1 => "devcontainer-v1",
            Self::Openssh => "openssh",
        }
    }

    const fn feature(self) -> &'static str {
        self.name()
    }

    const fn target(self) -> &'static str {
        match self {
            Self::DevcontainerV1 => "devcontainer_v1",
            Self::Openssh => "openssh",
        }
    }

    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "devcontainer-v1" => Ok(Self::DevcontainerV1),
            "openssh" => Ok(Self::Openssh),
            _ => Err(format!("unknown integration suite `{value}`")),
        }
    }
}

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        print_help();
        return ExitCode::FAILURE;
    };

    match command.to_str() {
        Some("check") if arguments.next().is_none() => run_quality_gate(),
        Some("build" | "dist") if arguments.next().is_none() => build_distribution(false),
        Some("stage-agents") if arguments.next().is_none() => build_distribution(true),
        Some("test-package") => {
            let paths = arguments.collect::<Vec<_>>();
            let [archive] = paths.as_slice() else {
                print_help();
                return ExitCode::FAILURE;
            };
            match package::smoke(Path::new(archive)) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("xtask: {error}");
                    ExitCode::FAILURE
                }
            }
        }
        Some("test-installed") => run_installed_smoke(arguments),
        Some("test-integration") => run_integration(arguments),
        Some("help" | "--help" | "-h") if arguments.next().is_none() => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("build" | "dist" | "check" | "help" | "--help" | "-h") => {
            eprintln!("xtask: unexpected additional arguments");
            print_help();
            ExitCode::FAILURE
        }
        Some(other) => {
            eprintln!("xtask: unknown command `{other}`");
            print_help();
            ExitCode::FAILURE
        }
        None => {
            eprintln!("xtask: commands must be valid UTF-8");
            ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the release pipeline keeps its ordered Docker staging steps auditable in one place"
)]
fn build_distribution(agents_only: bool) -> ExitCode {
    let build_id = match package::build_id() {
        Ok(id) => id,
        Err(error) => {
            eprintln!("xtask: cannot create build ID: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let supplied_stage = env::var_os("CDENV_AGENT_ARTIFACT_DIR").map(PathBuf::from);
    let stage = supplied_stage
        .clone()
        .unwrap_or_else(|| root.join("target/cdenv-agent").join(&build_id));
    if let Err(error) = fs::create_dir_all(&stage) {
        eprintln!("xtask: cannot create staging directory: {error}");
        return ExitCode::FAILURE;
    }
    for (platform, name, machine) in [
        ("linux/amd64", "cdenv-agent-x86_64", 62_u16),
        ("linux/arm64", "cdenv-agent-aarch64", 183_u16),
    ] {
        if supplied_stage.is_some() {
            continue;
        }
        let tag = format!("cdenv-agent-{build_id}-{name}");
        let built = Command::new("docker")
            .current_dir(&root)
            .args([
                "buildx",
                "build",
                "--load",
                "--platform",
                platform,
                "--build-arg",
                &format!("CDENV_AGENT_BUILD_ID={build_id}"),
                "--file",
                "xtask/agent.Dockerfile",
                "--tag",
                &tag,
                ".",
            ])
            .status();
        if !matches!(built, Ok(status) if status.success()) {
            eprintln!("xtask: Docker Buildx failed for {platform}");
            return ExitCode::FAILURE;
        }
        let version = Command::new("docker")
            .args(["run", "--rm", "--platform", platform, &tag, "version"])
            .output();
        if !version.is_ok_and(|output| {
            output.status.success() && package::valid_agent_version(&output.stdout, &build_id)
        }) {
            eprintln!(
                "xtask: {platform} agent did not report the expected build and protocol identity"
            );
            return ExitCode::FAILURE;
        }
        let container = format!("{tag}-extract");
        if !matches!(Command::new("docker").args(["create", "--name", &container, &tag]).status(), Ok(status) if status.success())
        {
            eprintln!("xtask: could not create {platform} artifact container");
            return ExitCode::FAILURE;
        }
        let destination = stage.join(name);
        let copied = Command::new("docker")
            .args([
                "cp",
                &format!("{container}:/cdenv-agent"),
                &destination.display().to_string(),
            ])
            .status();
        let _ = Command::new("docker")
            .args(["rm", "-f", &container])
            .status();
        if !matches!(copied, Ok(status) if status.success()) {
            eprintln!("xtask: could not stage {platform} artifact");
            return ExitCode::FAILURE;
        }
        if fs::read(&destination)
            .ok()
            .and_then(|bytes| validate_static_elf(&bytes, machine).ok())
            .is_none()
        {
            eprintln!(
                "xtask: staged {platform} artifact is not static for its claimed architecture"
            );
            return ExitCode::FAILURE;
        }
    }
    let report = match package::agent_report(&stage, &build_id) {
        Ok(report) => report,
        Err(error) => {
            eprintln!("xtask: {error}");
            return ExitCode::FAILURE;
        }
    };
    let manifest = stage.join("agents.json");
    let verified = if supplied_stage.is_some() {
        fs::read(&manifest).is_ok_and(|bytes| bytes == report.to_string().as_bytes())
    } else {
        fs::write(&manifest, report.to_string()).is_ok()
    };
    if !verified {
        eprintln!("xtask: staged agent identity/checksum mismatch");
        return ExitCode::FAILURE;
    }
    if agents_only {
        return ExitCode::SUCCESS;
    }
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    match Command::new(cargo)
        .current_dir(&root)
        .args(["build", "--release", "--package", "cdenv-cli", "--locked"])
        .env("CDENV_AGENT_ARTIFACT_DIR", &stage)
        .env("CDENV_BUILD_ID", &build_id)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .env(
            "RUSTFLAGS",
            format!("--remap-path-prefix={}=/cdenv", root.display()),
        )
        .status()
    {
        Ok(status) if status.success() => match package_distribution(&root, &build_id, &report) {
            Ok(archive) => {
                eprintln!(
                    "xtask: staged host and agents with build ID {build_id}; packaged {}",
                    archive.display()
                );
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("xtask: cannot package distribution: {error}");
                ExitCode::FAILURE
            }
        },
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            eprintln!("xtask: failed to build host: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run_quality_gate() -> ExitCode {
    for arguments in QUALITY_COMMANDS {
        let exit_code = run_cargo(arguments);
        if exit_code != ExitCode::SUCCESS {
            return exit_code;
        }
    }
    ExitCode::SUCCESS
}

fn run_installed_smoke(arguments: impl Iterator<Item = OsString>) -> ExitCode {
    let paths = arguments.collect::<Vec<_>>();
    let [archive] = paths.as_slice() else {
        eprintln!("xtask: test-installed requires exactly one release archive");
        return ExitCode::FAILURE;
    };
    let archive = match fs::canonicalize(archive) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("xtask: cannot resolve release archive: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = package::smoke(&archive) {
        eprintln!("xtask: package validation failed before installed smoke: {error}");
        return ExitCode::FAILURE;
    }
    let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
        .join("tests/release/installed-smoke.sh");
    match Command::new("bash").arg(script).arg(archive).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => ExitCode::from(u8::try_from(status.code().unwrap_or(1)).unwrap_or(1)),
        Err(error) => {
            eprintln!("xtask: failed to start installed smoke: {error}");
            ExitCode::FAILURE
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "the release gate keeps dependency, discovery, execution, and count checks ordered"
)]
fn run_integration(mut arguments: impl Iterator<Item = OsString>) -> ExitCode {
    let Some(flag) = arguments.next() else {
        eprintln!("xtask: test-integration requires --suite <devcontainer-v1|openssh>");
        return ExitCode::FAILURE;
    };
    if flag != "--suite" {
        eprintln!("xtask: expected --suite, got {}", flag.display());
        return ExitCode::FAILURE;
    }
    let Some(value) = arguments.next() else {
        eprintln!("xtask: --suite requires a suite name");
        return ExitCode::FAILURE;
    };
    if arguments.next().is_some() {
        eprintln!("xtask: unexpected additional integration arguments");
        return ExitCode::FAILURE;
    }
    let Some(value) = value.to_str() else {
        eprintln!("xtask: suite names must be valid UTF-8");
        return ExitCode::FAILURE;
    };
    let suite = match IntegrationSuite::parse(value) {
        Ok(suite) => suite,
        Err(error) => {
            eprintln!("xtask: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !integration_dependencies_available() || !integration_platform_supported() {
        return ExitCode::FAILURE;
    }
    if suite == IntegrationSuite::DevcontainerV1 {
        let regressions = run_cargo(&[
            "test",
            "--release",
            "--package",
            "cdenv-devcontainer",
            "--package",
            "cdenv-cli",
            "--lib",
            "--locked",
        ]);
        if regressions != ExitCode::SUCCESS {
            eprintln!("xtask: focused profile regression tests failed");
            return regressions;
        }
    }

    let arguments = [
        "test",
        "--release",
        "--package",
        "cdenv-integration-tests",
        "--features",
        suite.feature(),
        "--test",
        suite.target(),
        "--locked",
        "--",
        "--list",
    ];
    let listing = match cargo_output(&arguments) {
        Ok(output) if output.status.success() => output,
        Ok(output) => return report_cargo_failure(&output),
        Err(error) => {
            eprintln!("xtask: failed to start Cargo: {error}");
            return ExitCode::FAILURE;
        }
    };
    let discovered = discovered_tests(&listing);
    if discovered == 0 {
        eprintln!(
            "xtask: integration suite `{}` is unavailable: discovered 0 tests",
            suite.name()
        );
        return ExitCode::FAILURE;
    }
    eprintln!(
        "xtask: integration suite `{}` discovered {discovered} tests",
        suite.name()
    );
    let execution_arguments = [
        "test",
        "--release",
        "--package",
        "cdenv-integration-tests",
        "--features",
        suite.feature(),
        "--test",
        suite.target(),
        "--locked",
    ];
    let execution = match cargo_output(&execution_arguments) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("xtask: failed to start Cargo: {error}");
            return ExitCode::FAILURE;
        }
    };
    print_output(&execution);
    if !execution.status.success() {
        return status_exit_code(&execution);
    }
    let passed = passed_tests(&execution);
    if passed == 0 || passed != discovered {
        eprintln!(
            "xtask: integration suite `{}` discovered {discovered} tests but passed {passed}; skipped or unexecuted tests fail the release gate",
            suite.name()
        );
        return ExitCode::FAILURE;
    }
    eprintln!(
        "xtask: integration suite `{}` executed {passed} tests",
        suite.name()
    );
    ExitCode::SUCCESS
}

fn integration_platform_supported() -> bool {
    let supported = cfg!(target_os = "linux") && matches!(env::consts::ARCH, "x86_64" | "aarch64");
    if !supported {
        eprintln!(
            "xtask: complete integration suites require Linux x86_64 or arm64, found {} {}",
            env::consts::OS,
            env::consts::ARCH
        );
        return false;
    }
    if let Ok(declared) = env::var("CDENV_INTEGRATION_ARCH") {
        let declared = match declared.as_str() {
            "x86_64" | "amd64" => "x86_64",
            "arm64" | "aarch64" => "aarch64",
            _ => {
                eprintln!("xtask: unsupported declared integration architecture `{declared}`");
                return false;
            }
        };
        if declared != env::consts::ARCH {
            eprintln!(
                "xtask: declared integration architecture {declared} does not match host {}",
                env::consts::ARCH
            );
            return false;
        }
    }
    true
}

fn integration_dependencies_available() -> bool {
    let docker = dependency_output(
        "Docker Engine/CLI",
        "docker",
        &[
            "version",
            "--format",
            "{{.Client.Version}} {{.Server.Version}} {{.Server.APIVersion}}",
        ],
    );
    let compose = dependency_output(
        "Docker Compose V2",
        "docker",
        &["compose", "version", "--short"],
    );
    let openssh = dependency_output("OpenSSH client", "ssh", &["-V"]);
    let Some((docker, compose, openssh)) = docker
        .zip(compose)
        .zip(openssh)
        .map(|((a, b), c)| (a, b, c))
    else {
        return false;
    };

    let docker = String::from_utf8_lossy(&docker.stdout);
    let fields = docker.split_whitespace().collect::<Vec<_>>();
    let docker_supported = fields.len() == 3
        && version_at_least(fields[0], (29, 7, 1))
        && version_at_least(fields[1], (29, 6, 2))
        && version_at_least(fields[2], (1, 55, 0));
    let compose_supported =
        version_at_least(String::from_utf8_lossy(&compose.stdout).trim(), (5, 3, 1));
    let openssh_text = format!(
        "{}{}",
        String::from_utf8_lossy(&openssh.stdout),
        String::from_utf8_lossy(&openssh.stderr)
    );
    let openssh_version = openssh_text
        .split_whitespace()
        .find_map(|field| field.strip_prefix("OpenSSH_"));
    let openssh_supported =
        openssh_version.is_some_and(|value| version_at_least(value, (10, 0, 2)));
    if !docker_supported {
        eprintln!(
            "xtask: Docker requires CLI 29.7.1+, Engine 29.6.2+, and API 1.55+; found `{docker}`"
        );
    }
    if !compose_supported {
        eprintln!("xtask: Docker Compose V2 5.3.1+ is required");
    }
    if !openssh_supported {
        eprintln!(
            "xtask: OpenSSH 10.0p2+ is required; found `{}`",
            openssh_text.trim()
        );
    }
    docker_supported && compose_supported && openssh_supported
}

fn dependency_output(name: &str, executable: &str, arguments: &[&str]) -> Option<Output> {
    match Command::new(executable).args(arguments).output() {
        Ok(output) if output.status.success() => Some(output),
        Ok(_) | Err(_) => {
            eprintln!("xtask: declared integration environment is missing {name}");
            None
        }
    }
}

fn version_at_least(value: &str, minimum: (u32, u32, u32)) -> bool {
    let mut components = value
        .trim_start_matches(['v', 'V'])
        .split(['.', 'p', '_'])
        .map(|part| {
            part.chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<u32>()
        });
    let Some(Ok(major)) = components.next() else {
        return false;
    };
    let Some(Ok(minor)) = components.next() else {
        return false;
    };
    let patch = components.next().and_then(Result::ok).unwrap_or(0);
    (major, minor, patch) >= minimum
}

fn discovered_tests(output: &Output) -> usize {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.trim_end().ends_with(": test"))
        .count()
}

fn passed_tests(output: &Output) -> usize {
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| line.starts_with("test result: ok."))
        .filter_map(|line| line.split_whitespace().nth(3))
        .filter_map(|value| value.parse::<usize>().ok())
        .sum()
}

fn run_cargo(arguments: &[&str]) -> ExitCode {
    match cargo_output(arguments) {
        Ok(output) if output.status.success() => {
            print_output(&output);
            ExitCode::SUCCESS
        }
        Ok(output) => report_cargo_failure(&output),
        Err(error) => {
            eprintln!("xtask: failed to start Cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cargo_output(arguments: &[&str]) -> std::io::Result<Output> {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    print_command(&cargo, arguments);
    Command::new(cargo).args(arguments).output()
}

fn report_cargo_failure(output: &Output) -> ExitCode {
    print_output(output);
    status_exit_code(output)
}

fn status_exit_code(output: &Output) -> ExitCode {
    output
        .status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map_or(ExitCode::FAILURE, ExitCode::from)
}

fn print_output(output: &Output) {
    eprint!("{}", String::from_utf8_lossy(&output.stdout));
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
}

fn print_command(cargo: &OsStr, arguments: &[&str]) {
    eprint!("$ {}", cargo.to_string_lossy());
    for argument in arguments {
        eprint!(" {argument}");
    }
    eprintln!();
}

fn print_help() {
    eprintln!(
        "Usage:\n  cargo xtask check\n  cargo xtask dist\n  cargo xtask stage-agents\n  cargo xtask test-package <archive.tar>\n  cargo xtask test-installed <archive.tar>\n  cargo xtask test-integration --suite <devcontainer-v1|openssh>\n\nIntegration suites run in release mode and require every discovered test to execute and pass. Docker Engine/CLI, Compose V2, and OpenSSH are mandatory; missing or below-baseline dependencies never skip the suite."
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integration_suite_parser_accepts_only_declared_suites() {
        assert_eq!(
            IntegrationSuite::parse("devcontainer-v1"),
            Ok(IntegrationSuite::DevcontainerV1)
        );
        assert_eq!(
            IntegrationSuite::parse("openssh"),
            Ok(IntegrationSuite::Openssh)
        );
        assert!(IntegrationSuite::parse("unknown").is_err());
    }

    #[test]
    fn dependency_version_parser_handles_docker_compose_and_openssh_versions() {
        assert!(version_at_least("v29.7.1", (29, 7, 1)));
        assert!(version_at_least("10.0p2", (10, 0, 2)));
        assert!(!version_at_least("OpenSSH_9.9p9", (10, 0, 2)));
        assert!(!version_at_least("29.6.9", (29, 7, 1)));
    }
}
