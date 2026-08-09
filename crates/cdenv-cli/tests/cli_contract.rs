//! Black-box tests for the complete V1 command-line contract.

use std::ffi::OsString;
use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;

use cdenv_cli::{CliCommand, CommandLine, OutputFormat, SshConfigConsent, WorkspaceSelector};
use clap::{CommandFactory, Parser, error::ErrorKind};

#[test]
fn global_root_and_modify_consent_parse_after_the_command() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "list",
        "--root",
        "/tmp/cdenv-root",
        "--modify-ssh-config",
    ])
    .expect("documented global options should parse after a command");

    assert_eq!(
        (command_line.root(), command_line.ssh_config_consent()),
        (
            Some(Path::new("/tmp/cdenv-root")),
            Some(SshConfigConsent::Accept)
        )
    );
}

#[test]
fn global_no_modify_consent_parses_before_the_command() {
    let command_line = CommandLine::try_parse_from(["cdenv", "--no-modify-ssh-config", "doctor"])
        .expect("documented global consent should parse before a command");

    assert_eq!(
        command_line.ssh_config_consent(),
        Some(SshConfigConsent::Decline)
    );
}

#[test]
fn create_parses_source_name_and_repo_relative_config() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "create",
        "git@github.com:example/project.git",
        "--name",
        "project-api",
        "--config",
        ".devcontainer/alternate/devcontainer.json",
    ])
    .expect("the documented create invocation should parse");
    let CliCommand::Create(arguments) = command_line.command() else {
        panic!("create should select the create command");
    };

    assert_eq!(
        (
            arguments.git_source.as_str(),
            arguments.name.as_ref().map(ToString::to_string),
            arguments.config.as_ref().map(ToString::to_string)
        ),
        (
            "git@github.com:example/project.git",
            Some("project-api".to_owned()),
            Some(".devcontainer/alternate/devcontainer.json".to_owned())
        )
    );
}

#[test]
fn list_json_selects_machine_output() {
    let command_line = CommandLine::try_parse_from(["cdenv", "list", "--json"])
        .expect("the documented list invocation should parse");

    assert_eq!(command_line.output_format(), OutputFormat::Json);
}

#[test]
fn up_parses_workspace_and_desired_config() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "up",
        "project",
        "--config",
        ".devcontainer/alternate/devcontainer.json",
    ])
    .expect("the documented up invocation should parse");
    let CliCommand::Up(arguments) = command_line.command() else {
        panic!("up should select the up command");
    };

    assert_eq!(
        (
            arguments.name.as_str(),
            arguments.config.as_ref().map(ToString::to_string)
        ),
        (
            "project",
            Some(".devcontainer/alternate/devcontainer.json".to_owned())
        )
    );
}

#[test]
fn down_parses_workspace_name() {
    let command_line = CommandLine::try_parse_from(["cdenv", "down", "project"])
        .expect("the documented down invocation should parse");
    let CliCommand::Down(arguments) = command_line.command() else {
        panic!("down should select the down command");
    };

    assert_eq!(arguments.name.as_str(), "project");
}

#[test]
fn rebuild_parses_config_and_no_cache() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "rebuild",
        "project",
        "--config",
        ".devcontainer/alternate/devcontainer.json",
        "--no-cache",
    ])
    .expect("the documented rebuild invocation should parse");
    let CliCommand::Rebuild(arguments) = command_line.command() else {
        panic!("rebuild should select the rebuild command");
    };

    assert_eq!(
        (
            arguments.name.as_str(),
            arguments.config.as_ref().map(ToString::to_string),
            arguments.no_cache
        ),
        (
            "project",
            Some(".devcontainer/alternate/devcontainer.json".to_owned()),
            true
        )
    );
}

#[test]
fn status_parses_workspace_and_json() {
    let command_line = CommandLine::try_parse_from(["cdenv", "status", "project", "--json"])
        .expect("the documented status invocation should parse");
    let CliCommand::Status(arguments) = command_line.command() else {
        panic!("status should select the status command");
    };

    assert_eq!((arguments.name.as_str(), arguments.json), ("project", true));
}

#[test]
fn ssh_without_remote_command_parses_an_empty_argv() {
    let command_line = CommandLine::try_parse_from(["cdenv", "ssh", "project"])
        .expect("the documented interactive SSH invocation should parse");
    let CliCommand::Ssh(arguments) = command_line.command() else {
        panic!("ssh should select the ssh command");
    };

    assert!(arguments.remote_argv.is_empty());
}

#[test]
fn ssh_preserves_each_remote_argument_after_the_delimiter() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "ssh",
        "project",
        "--",
        "printf",
        "%s\\n",
        "two words",
        "--remote-flag=value",
        "",
    ])
    .expect("remote argv after -- should parse");
    let CliCommand::Ssh(arguments) = command_line.command() else {
        panic!("ssh should select the ssh command");
    };

    assert_eq!(
        arguments.remote_argv,
        [
            OsString::from("printf"),
            OsString::from("%s\\n"),
            OsString::from("two words"),
            OsString::from("--remote-flag=value"),
            OsString::new(),
        ]
    );
}

#[test]
fn forward_parses_one_mapping_with_the_loopback_default() {
    let command_line = CommandLine::try_parse_from(["cdenv", "forward", "project", "8080:3000"])
        .expect("the documented one-port forward invocation should parse");
    let CliCommand::Forward(arguments) = command_line.command() else {
        panic!("forward should select the forward command");
    };

    assert_eq!(
        (
            arguments.name.as_str(),
            arguments.mappings.first().map(ToString::to_string),
            arguments.bind
        ),
        (
            "project",
            Some("8080:3000".to_owned()),
            IpAddr::V4(Ipv4Addr::LOCALHOST)
        )
    );
}

#[test]
fn forward_parses_multiple_mappings_and_an_explicit_bind() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "forward",
        "project",
        "8080:3000",
        "5432:5432",
        "--bind",
        "0.0.0.0",
    ])
    .expect("the documented multi-port forward invocation should parse");
    let CliCommand::Forward(arguments) = command_line.command() else {
        panic!("forward should select the forward command");
    };

    assert_eq!(
        (
            arguments
                .mappings
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            arguments.bind
        ),
        (
            vec!["8080:3000".to_owned(), "5432:5432".to_owned()],
            IpAddr::V4(Ipv4Addr::UNSPECIFIED)
        )
    );
}

#[test]
fn lock_parses_workspace_and_target_config() {
    let command_line = CommandLine::try_parse_from([
        "cdenv",
        "lock",
        "project",
        "--config",
        ".devcontainer/devcontainer.json",
    ])
    .expect("the documented lock invocation should parse");
    let CliCommand::Lock(arguments) = command_line.command() else {
        panic!("lock should select the lock command");
    };

    assert_eq!(
        (
            arguments.name.as_str(),
            arguments.config.as_ref().map(ToString::to_string)
        ),
        (
            "project",
            Some(".devcontainer/devcontainer.json".to_owned())
        )
    );
}

#[test]
fn proxy_parses_a_workspace_name() {
    let command_line =
        CommandLine::try_parse_from(["cdenv", "--root", "/tmp/cdenv-root", "proxy", "project"])
            .expect("the documented proxy invocation should parse");
    let CliCommand::Proxy(arguments) = command_line.command() else {
        panic!("proxy should select the proxy command");
    };

    assert!(matches!(arguments.workspace, WorkspaceSelector::Name(_)));
}

#[test]
fn proxy_parses_an_exact_workspace_host() {
    let command_line = CommandLine::try_parse_from(["cdenv", "proxy", "project.cdenv"])
        .expect("the documented proxy host form should parse");
    let CliCommand::Proxy(arguments) = command_line.command() else {
        panic!("proxy should select the proxy command");
    };

    assert_eq!(arguments.workspace.workspace_name().as_str(), "project");
}

#[test]
fn doctor_json_selects_machine_output() {
    let command_line = CommandLine::try_parse_from(["cdenv", "doctor", "--json"])
        .expect("the documented doctor invocation should parse");

    assert_eq!(command_line.output_format(), OutputFormat::Json);
}

#[test]
fn parser_rejects_zero_local_forward_port() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "0:3000"])
        .expect_err("port zero must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_zero_container_forward_port() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "8080:0"])
        .expect_err("port zero must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_mapping_without_a_colon() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "8080"])
        .expect_err("a malformed mapping must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_mapping_with_multiple_colons() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "8080:3000:80"])
        .expect_err("a malformed mapping must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_nonnumeric_mapping_port() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "http:3000"])
        .expect_err("a nonnumeric mapping must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_mapping_port_above_u16_range() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project", "65536:3000"])
        .expect_err("an out-of-range mapping must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_requires_at_least_one_forward_mapping() {
    let error = CommandLine::try_parse_from(["cdenv", "forward", "project"])
        .expect_err("forward without a mapping must be rejected");

    assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
}

#[test]
fn parser_rejects_invalid_workspace_names() {
    let error = CommandLine::try_parse_from(["cdenv", "up", "Project_Name"])
        .expect_err("an invalid workspace name must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_invalid_explicit_create_name() {
    let error = CommandLine::try_parse_from([
        "cdenv",
        "create",
        "https://example.test/project.git",
        "--name",
        "Project",
    ])
    .expect_err("an invalid explicit name must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_an_invalid_proxy_host() {
    let error = CommandLine::try_parse_from(["cdenv", "proxy", "Project.cdenv"])
        .expect_err("an invalid proxy host must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_conflicting_global_consent_flags() {
    let error = CommandLine::try_parse_from([
        "cdenv",
        "--modify-ssh-config",
        "list",
        "--no-modify-ssh-config",
    ])
    .expect_err("conflicting SSH consent must be rejected");

    assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn parser_rejects_an_unknown_command() {
    let error = CommandLine::try_parse_from(["cdenv", "open", "project"])
        .expect_err("unknown commands must be rejected");

    assert_eq!(error.kind(), ErrorKind::InvalidSubcommand);
}

#[test]
fn parser_rejects_an_unknown_option() {
    let error = CommandLine::try_parse_from(["cdenv", "up", "project", "--force"])
        .expect_err("unknown command options must be rejected");

    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn parser_rejects_json_on_a_command_without_machine_output() {
    let error = CommandLine::try_parse_from(["cdenv", "down", "project", "--json"])
        .expect_err("undocumented JSON flags must be rejected");

    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn parser_requires_the_ssh_delimiter_before_remote_argv() {
    let error = CommandLine::try_parse_from(["cdenv", "ssh", "project", "uname", "-a"])
        .expect_err("remote argv without -- must be rejected");

    assert_eq!(error.kind(), ErrorKind::UnknownArgument);
}

#[test]
fn parser_rejects_an_absolute_config_path() {
    let error = CommandLine::try_parse_from([
        "cdenv",
        "up",
        "project",
        "--config",
        "/tmp/devcontainer.json",
    ])
    .expect_err("an absolute config path must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn parser_rejects_config_parent_traversal() {
    let error =
        CommandLine::try_parse_from(["cdenv", "up", "project", "--config", "../devcontainer.json"])
            .expect_err("config traversal must be rejected");

    assert_eq!(error.kind(), ErrorKind::ValueValidation);
}

#[test]
fn help_lists_exactly_the_v1_commands() {
    let command = <CommandLine as CommandFactory>::command();
    let command_names = command
        .get_subcommands()
        .map(clap::Command::get_name)
        .collect::<Vec<_>>();

    assert_eq!(
        command_names,
        [
            "create", "list", "up", "down", "rebuild", "status", "ssh", "forward", "lock", "proxy",
            "doctor"
        ]
    );
}

fn rendered_help(arguments: &[&str]) -> String {
    CommandLine::try_parse_from(arguments)
        .expect_err("--help should stop parsing with display output")
        .to_string()
}

#[test]
fn top_level_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-top-level.txt"));
}

#[test]
fn create_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "create", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-create.txt"));
}

#[test]
fn list_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "list", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-list.txt"));
}

#[test]
fn up_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "up", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-up.txt"));
}

#[test]
fn down_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "down", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-down.txt"));
}

#[test]
fn rebuild_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "rebuild", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-rebuild.txt"));
}

#[test]
fn status_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "status", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-status.txt"));
}

#[test]
fn ssh_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "ssh", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-ssh.txt"));
}

#[test]
fn forward_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "forward", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-forward.txt"));
}

#[test]
fn lock_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "lock", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-lock.txt"));
}

#[test]
fn proxy_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "proxy", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-proxy.txt"));
}

#[test]
fn doctor_help_matches_the_reviewed_snapshot() {
    let help = rendered_help(&["cdenv", "doctor", "--help"]);

    assert_eq!(help, include_str!("snapshots/help-doctor.txt"));
}
