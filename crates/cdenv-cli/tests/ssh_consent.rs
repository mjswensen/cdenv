//! Remembered user OpenSSH Include consent contract tests.

use std::fs;
use std::io;
use std::path::Path;

use cdenv_cli::{
    CdenvRoot, Installation, ProcessEnvironment, SshConfigConsent, SshConsentInteraction,
    SshIncludeConsent, SshIncludeOutcome, apply_ssh_include_consent,
};

fn root_at(path: &Path) -> CdenvRoot {
    CdenvRoot::resolve(Some(path), &ProcessEnvironment).expect("absolute test root")
}

#[derive(Default)]
struct Interaction {
    tty: bool,
    response: String,
    messages: String,
    reads: usize,
}

impl SshConsentInteraction for Interaction {
    fn stdin_is_terminal(&self) -> bool {
        self.tty
    }

    fn write_message(&mut self, message: &str) -> io::Result<()> {
        self.messages.push_str(message);
        Ok(())
    }

    fn read_response(&mut self) -> io::Result<String> {
        self.reads += 1;
        Ok(self.response.clone())
    }
}

#[test]
fn unknown_noninteractive_consent_does_not_prompt_or_modify() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut interaction = Interaction::default();

    let outcome =
        apply_ssh_include_consent(&root, &mut installation, None, &home, &mut interaction)
            .expect("consent handling");

    assert_eq!(outcome, SshIncludeOutcome::NeedsExplicitConsent);
    assert_eq!(interaction.reads, 0);
    assert!(!home.join(".ssh/config").exists());
}

#[test]
fn tty_acceptance_inserts_before_first_host_and_is_remembered() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root with space"));
    let home = temporary.path().join("home");
    fs::create_dir_all(home.join(".ssh")).expect("SSH directory");
    fs::write(
        home.join(".ssh/config"),
        "# retained\n\nHost example.test\n    User example\n",
    )
    .expect("user config");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut interaction = Interaction {
        tty: true,
        response: "yes\n".to_owned(),
        ..Interaction::default()
    };

    let outcome =
        apply_ssh_include_consent(&root, &mut installation, None, &home, &mut interaction)
            .expect("consent handling");
    let content = fs::read_to_string(home.join(".ssh/config")).expect("user config");

    assert_eq!(outcome, SshIncludeOutcome::Inserted);
    assert!(content.starts_with("# retained\n\nInclude \""));
    assert!(content.find("Include").expect("Include") < content.find("Host").expect("Host"));
    assert_eq!(
        installation.record().ssh_include_consent(),
        SshIncludeConsent::Accepted
    );
}

#[test]
fn accepted_include_insertion_is_idempotent_without_another_prompt() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root with \"quote"));
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut first = Interaction::default();
    apply_ssh_include_consent(
        &root,
        &mut installation,
        Some(SshConfigConsent::Accept),
        &home,
        &mut first,
    )
    .expect("first insertion");
    let before = fs::read(home.join(".ssh/config")).expect("config");
    let mut second = Interaction::default();

    let outcome = apply_ssh_include_consent(&root, &mut installation, None, &home, &mut second)
        .expect("second setup");

    assert_eq!(outcome, SshIncludeOutcome::AlreadyPresent);
    assert_eq!(fs::read(home.join(".ssh/config")).expect("config"), before);
    assert_eq!(second.reads, 0);
}

#[test]
fn tty_decline_is_remembered_and_never_prompts_again() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let home = temporary.path().join("home");
    fs::create_dir(&home).expect("home");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut first = Interaction {
        tty: true,
        response: "no\n".to_owned(),
        ..Interaction::default()
    };
    let first_outcome =
        apply_ssh_include_consent(&root, &mut installation, None, &home, &mut first)
            .expect("decline");
    let mut second = Interaction {
        tty: true,
        response: "yes\n".to_owned(),
        ..Interaction::default()
    };

    let second_outcome =
        apply_ssh_include_consent(&root, &mut installation, None, &home, &mut second)
            .expect("remembered decline");

    assert_eq!(
        (first_outcome, second_outcome),
        (SshIncludeOutcome::Declined, SshIncludeOutcome::Declined)
    );
    assert_eq!(second.reads, 0);
}

#[test]
fn explicit_decline_does_not_remove_an_existing_include() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let home = temporary.path().join("home");
    fs::create_dir_all(home.join(".ssh")).expect("SSH directory");
    let original = format!("Include \"{}\"\n", root.ssh().config().display());
    fs::write(home.join(".ssh/config"), &original).expect("config");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut interaction = Interaction::default();

    let outcome = apply_ssh_include_consent(
        &root,
        &mut installation,
        Some(SshConfigConsent::Decline),
        &home,
        &mut interaction,
    )
    .expect("explicit decline");

    assert_eq!(outcome, SshIncludeOutcome::Declined);
    assert_eq!(
        fs::read_to_string(home.join(".ssh/config")).expect("config"),
        original
    );
}

#[cfg(unix)]
#[test]
fn symlinked_user_config_is_refused_with_manual_instructions() {
    use std::os::unix::fs::symlink;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let home = temporary.path().join("home");
    fs::create_dir_all(home.join(".ssh")).expect("SSH directory");
    let target = temporary.path().join("real-config");
    fs::write(&target, "Host retained\n").expect("target");
    symlink(&target, home.join(".ssh/config")).expect("symlink");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut interaction = Interaction::default();

    let outcome = apply_ssh_include_consent(
        &root,
        &mut installation,
        Some(SshConfigConsent::Accept),
        &home,
        &mut interaction,
    )
    .expect("unsafe config should produce instructions");

    assert_eq!(outcome, SshIncludeOutcome::ManualInstructions);
    assert!(interaction.messages.contains("Add this line manually"));
    assert_eq!(
        fs::read_to_string(target).expect("target"),
        "Host retained\n"
    );
}

#[cfg(unix)]
#[test]
fn insertion_preserves_existing_user_config_mode() {
    use std::os::unix::fs::PermissionsExt;

    let temporary = tempfile::tempdir().expect("temporary directory");
    let root = root_at(&temporary.path().join("root"));
    let home = temporary.path().join("home");
    fs::create_dir_all(home.join(".ssh")).expect("SSH directory");
    let config = home.join(".ssh/config");
    fs::write(&config, "Match all\n").expect("config");
    fs::set_permissions(&config, fs::Permissions::from_mode(0o640)).expect("mode");
    let mut installation = Installation::open_or_create(&root).expect("installation");
    let mut interaction = Interaction::default();

    apply_ssh_include_consent(
        &root,
        &mut installation,
        Some(SshConfigConsent::Accept),
        &home,
        &mut interaction,
    )
    .expect("insert Include");
    let mode = fs::metadata(config).expect("metadata").permissions().mode() & 0o777;

    assert_eq!(mode, 0o640);
}
