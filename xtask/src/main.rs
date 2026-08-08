//! Repository automation entry point.

use std::env;
use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitCode};

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

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(command) = arguments.next() else {
        print_help();
        return ExitCode::FAILURE;
    };

    if arguments.next().is_some() {
        eprintln!("xtask: unexpected additional arguments");
        print_help();
        return ExitCode::FAILURE;
    }

    match command.to_str() {
        Some("check") => run_quality_gate(),
        Some("test-integration") => run_cargo(&[
            "test",
            "--package",
            "cdenv-integration-tests",
            "--features",
            "integration",
            "--locked",
        ]),
        Some("help" | "--help" | "-h") => {
            print_help();
            ExitCode::SUCCESS
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

fn run_cargo(arguments: &[&str]) -> ExitCode {
    let cargo = env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
    print_command(&cargo, arguments);

    match Command::new(cargo).args(arguments).status() {
        Ok(status) if status.success() => ExitCode::SUCCESS,
        Ok(status) => status
            .code()
            .and_then(|code| u8::try_from(code).ok())
            .map_or(ExitCode::FAILURE, ExitCode::from),
        Err(error) => {
            eprintln!("xtask: failed to start Cargo: {error}");
            ExitCode::FAILURE
        }
    }
}

fn print_command(cargo: &OsStr, arguments: &[&str]) {
    eprint!("$ {}", cargo.to_string_lossy());
    for argument in arguments {
        eprint!(" {argument}");
    }
    eprintln!();
}

fn print_help() {
    eprintln!("Usage: cargo xtask <check|test-integration>");
}
