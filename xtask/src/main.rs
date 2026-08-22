//! Repository automation entry point.

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitCode, Output};

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
        Some("test-integration") => run_integration(arguments),
        Some("help" | "--help" | "-h") if arguments.next().is_none() => {
            print_help();
            ExitCode::SUCCESS
        }
        Some("check" | "help" | "--help" | "-h") => {
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

fn run_quality_gate() -> ExitCode {
    for arguments in QUALITY_COMMANDS {
        let exit_code = run_cargo(arguments);
        if exit_code != ExitCode::SUCCESS {
            return exit_code;
        }
    }
    ExitCode::SUCCESS
}

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
    if env::var_os("CDENV_INTEGRATION").as_deref() == Some(OsStr::new("1"))
        && !integration_dependencies_available()
    {
        return ExitCode::FAILURE;
    }

    let arguments = [
        "test",
        "--package",
        "cdenv-integration-tests",
        "--features",
        "integration",
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
    let executed = run_cargo(&[
        "test",
        "--package",
        "cdenv-integration-tests",
        "--features",
        "integration",
        "--locked",
    ]);
    if executed == ExitCode::SUCCESS {
        eprintln!(
            "xtask: integration suite `{}` executed {discovered} tests",
            suite.name()
        );
    }
    executed
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
        "Usage:\n  cargo xtask check\n  cargo xtask test-integration --suite <devcontainer-v1|openssh>\n\nIntegration suites require at least one discovered and executed test. Until a suite is implemented, the command fails as unavailable. Set CDENV_INTEGRATION=1 for declared CI runs; Docker Engine/CLI, Compose V2, and OpenSSH are then required."
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
}
