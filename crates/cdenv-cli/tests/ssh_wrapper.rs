//! System OpenSSH wrapper process contract tests.

#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;

    fn private_tempdir() -> tempfile::TempDir {
        // Preserve space-containing roots without depending on the runner TMPDIR length.
        let parent = fs::canonicalize("/tmp").expect("canonical temporary root");
        tempfile::tempdir_in(parent).expect("private temporary directory")
    }

    #[test]
    fn cdenv_ssh_propagates_system_ssh_status() {
        let temporary = private_tempdir();
        let fake_ssh = temporary.path().join("ssh");
        fs::write(&fake_ssh, "#!/bin/sh\nexit 37\n").expect("fake ssh");
        fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("mode");
        let root = temporary.path().join("root");

        let status = Command::new(env!("CARGO_BIN_EXE_cdenv"))
            .args([
                "--root",
                root.to_str().expect("UTF-8 root"),
                "ssh",
                "project",
            ])
            .env("PATH", temporary.path())
            .status()
            .expect("cdenv should run");

        assert_eq!(status.code(), Some(37));
    }

    #[test]
    fn cdenv_ssh_passes_explicit_config_host_and_remote_arguments_unchanged() {
        let temporary = private_tempdir();
        let fake_ssh = temporary.path().join("ssh");
        let observed = temporary.path().join("arguments");
        fs::write(
            &fake_ssh,
            "#!/bin/sh\n: > \"$OBSERVED\"\nfor arg do printf '%s\\n' \"$arg\" >> \"$OBSERVED\"; done\n",
        )
        .expect("fake ssh");
        fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).expect("mode");
        let root = temporary.path().join("root with space");

        let status = Command::new(env!("CARGO_BIN_EXE_cdenv"))
            .arg("--root")
            .arg(&root)
            .args(["ssh", "project", "--", "printf '%s'", "a b"])
            .env("PATH", temporary.path())
            .env("OBSERVED", &observed)
            .status()
            .expect("cdenv should run");
        let arguments = fs::read_to_string(observed).expect("recorded arguments");

        assert!(status.success());
        assert_eq!(
            arguments,
            format!(
                "-F\n{}\nproject.cdenv\nprintf '%s'\na b\n",
                root.join("ssh/config").display()
            )
        );
    }
}
