//! Repository automation entry point.

use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

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
    &["doc", "--workspace", "--no-deps"],
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
        Some("build" | "dist") if arguments.next().is_none() => build_distribution(),
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
fn build_distribution() -> ExitCode {
    let build_id = match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => format!("{:x}-{}", duration.as_nanos(), std::process::id()),
        Err(error) => {
            eprintln!("xtask: cannot create build ID: {error}");
            return ExitCode::FAILURE;
        }
    };
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let stage = root.join("target/cdenv-agent").join(&build_id);
    if let Err(error) = fs::create_dir_all(&stage) {
        eprintln!("xtask: cannot create staging directory: {error}");
        return ExitCode::FAILURE;
    }
    for (platform, name, machine) in [
        ("linux/amd64", "cdenv-agent-x86_64", 62_u16),
        ("linux/arm64", "cdenv-agent-aarch64", 183_u16),
    ] {
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
        let expected_version = format!("\"buildId\":\"{build_id}\"");
        if !matches!(version, Ok(ref output) if output.status.success() && String::from_utf8_lossy(&output.stdout).contains("\"name\":\"cdenv-agent\"") && String::from_utf8_lossy(&output.stdout).contains("\"protocolVersion\":1") && String::from_utf8_lossy(&output.stdout).contains(&expected_version))
        {
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
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    match Command::new(cargo)
        .current_dir(&root)
        .args(["build", "--release", "--package", "cdenv-cli", "--locked"])
        .env("CDENV_AGENT_ARTIFACT_DIR", &stage)
        .env("CDENV_BUILD_ID", &build_id)
        .status()
    {
        Ok(status) if status.success() => match package_distribution(&root, &build_id) {
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

fn package_distribution(root: &Path, build_id: &str) -> std::io::Result<PathBuf> {
    let host = root.join("target/release/cdenv");
    let metadata = fs::metadata(&host)?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(std::io::Error::other(
            "release host binary is missing or empty",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::other(
                "release host binary is not executable",
            ));
        }
    }
    let platform = format!("{}-{}", env::consts::OS, env::consts::ARCH);
    let destination = root.join("target/dist");
    fs::create_dir_all(&destination)?;
    let archive = destination.join(format!("cdenv-{platform}-{build_id}.tar"));
    let file = fs::File::create(&archive)?;
    let mut tar = tar::Builder::new(file);
    tar.append_path_with_name(&host, "cdenv")?;
    tar.finish()?;
    let bytes = fs::read(&archive)?;
    let checksum = format!(
        "{:x}  {}\n",
        Sha256::digest(&bytes),
        archive.file_name().unwrap_or_default().to_string_lossy()
    );
    let checksum_path = archive.with_extension("tar.sha256");
    fs::File::create(&checksum_path)?.write_all(checksum.as_bytes())?;
    let recorded = fs::read_to_string(&checksum_path)?;
    if !recorded.starts_with(&format!("{:x}", Sha256::digest(fs::read(&archive)?))) {
        return Err(std::io::Error::other(
            "distribution checksum verification failed",
        ));
    }
    Ok(archive)
}

fn validate_static_elf(bytes: &[u8], machine: u16) -> Result<(), ()> {
    if bytes.len() < 64
        || &bytes[..4] != b"\x7fELF"
        || bytes[4] != 2
        || bytes[5] != 1
        || u16::from_le_bytes([bytes[18], bytes[19]]) != machine
    {
        return Err(());
    }
    let offset = usize::try_from(u64::from_le_bytes(
        bytes[32..40].try_into().map_err(|_| ())?,
    ))
    .map_err(|_| ())?;
    let size = usize::from(u16::from_le_bytes([bytes[54], bytes[55]]));
    let count = usize::from(u16::from_le_bytes([bytes[56], bytes[57]]));
    let end = offset
        .checked_add(size.checked_mul(count).ok_or(())?)
        .ok_or(())?;
    if size < 4 || end > bytes.len() {
        return Err(());
    }
    if (0..count).any(|index| {
        u32::from_le_bytes(
            bytes[offset + index * size..][..4]
                .try_into()
                .unwrap_or([0; 4]),
        ) == 3
    }) {
        return Err(());
    }
    Ok(())
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
    }
    supported
}

fn integration_dependencies_available() -> bool {
    let dependencies = [
        ("Docker Engine/CLI", "docker", vec!["version"]),
        ("Docker Compose V2", "docker", vec!["compose", "version"]),
        ("OpenSSH client", "ssh", vec!["-V"]),
    ];
    let mut available = true;
    for (name, executable, arguments) in dependencies {
        if !matches!(
            Command::new(executable).args(arguments).status(),
            Ok(status) if status.success()
        ) {
            eprintln!("xtask: declared integration environment is missing {name}");
            available = false;
        }
    }
    available
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
        "Usage:\n  cargo xtask check\n  cargo xtask dist\n  cargo xtask test-integration --suite <devcontainer-v1|openssh>\n\nIntegration suites run in release mode and require every discovered test to execute and pass. Docker Engine/CLI, Compose V2, and OpenSSH are mandatory; missing dependencies never skip the suite."
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
    #[cfg(unix)]
    fn distribution_archive_contains_only_the_executable_and_verified_checksum() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary root");
        let release = temporary.path().join("target/release");
        fs::create_dir_all(&release).expect("release directory");
        let host = release.join("cdenv");
        fs::write(&host, b"host-binary").expect("host binary");
        fs::set_permissions(&host, fs::Permissions::from_mode(0o755)).expect("executable mode");

        let archive = package_distribution(temporary.path(), "test-build").expect("package");
        let mut entries = tar::Archive::new(fs::File::open(&archive).expect("archive"));
        let names = entries
            .entries()
            .expect("entries")
            .map(|entry| entry.expect("entry").path().expect("path").into_owned())
            .collect::<Vec<_>>();
        assert_eq!(names, [PathBuf::from("cdenv")]);
        let checksum = fs::read_to_string(archive.with_extension("tar.sha256")).expect("checksum");
        assert!(checksum.starts_with(&format!(
            "{:x}",
            Sha256::digest(fs::read(archive).expect("archive bytes"))
        )));
    }
}
