//! Deterministic release archives and package-only validation.

use std::env;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

pub fn build_id() -> io::Result<String> {
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .output()?;
    if !status.status.success() || !status.stdout.is_empty() {
        return Err(io::Error::other("release builds require a clean checkout"));
    }
    let commit = Command::new("git").args(["rev-parse", "HEAD"]).output()?;
    let id = String::from_utf8_lossy(&commit.stdout).trim().to_owned();
    if !commit.status.success() || id.len() != 40 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(io::Error::other("cannot determine release commit"));
    }
    Ok(id)
}

pub fn valid_agent_version(bytes: &[u8], build_id: &str) -> bool {
    serde_json::from_slice::<Value>(bytes).is_ok_and(|v| {
        v["name"] == "cdenv-agent"
            && v["buildId"] == build_id
            && v["protocolVersion"] == 1
            && v["version"] == env!("CARGO_PKG_VERSION")
    })
}

pub fn agent_report(stage: &Path, build_id: &str) -> io::Result<Value> {
    let mut agents = serde_json::Map::new();
    for (arch, machine) in [("x86_64", 62), ("aarch64", 183)] {
        let bytes = fs::read(stage.join(format!("cdenv-agent-{arch}")))?;
        super::validate_static_elf(&bytes, machine)
            .map_err(|error| io::Error::other(format!("invalid static {arch} agent: {error}")))?;
        agents.insert(
            arch.to_owned(),
            json!(format!("{:x}", Sha256::digest(bytes))),
        );
    }
    Ok(
        json!({"schemaVersion": 1, "version": env!("CARGO_PKG_VERSION"),
        "buildId": build_id, "protocolVersion": 1, "agents": agents}),
    )
}

fn platform() -> String {
    format!("{}-{}", env::consts::OS, env::consts::ARCH)
}

fn validate_host(bytes: &[u8], platform: &str) -> io::Result<()> {
    let valid = match platform {
        "linux-x86_64" | "linux-aarch64" => {
            let machine = if platform.ends_with("x86_64") {
                62
            } else {
                183
            };
            bytes.len() >= 64
                && &bytes[..6] == b"\x7fELF\x02\x01"
                && matches!(u16::from_le_bytes([bytes[16], bytes[17]]), 2 | 3)
                && u16::from_le_bytes([bytes[18], bytes[19]]) == machine
                && bytes[24..32] != [0; 8]
        }
        "macos-x86_64" | "macos-aarch64" => {
            let cpu: u32 = if platform.ends_with("x86_64") {
                0x0100_0007
            } else {
                0x0100_000c
            };
            bytes.len() >= 32
                && bytes[..4] == [0xcf, 0xfa, 0xed, 0xfe]
                && bytes[4..8] == cpu.to_le_bytes()
                && bytes[12..16] == 2_u32.to_le_bytes()
        }
        _ => false,
    };
    if !valid {
        return Err(io::Error::other(
            "wrong host executable format/architecture",
        ));
    }
    Ok(())
}

fn archive_bytes(host: &[u8]) -> io::Result<Vec<u8>> {
    let mut tar = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_ustar();
    header.set_path("cdenv")?;
    header.set_size(host.len() as u64);
    header.set_mode(0o755);
    header.set_uid(0);
    header.set_gid(0);
    header.set_mtime(0);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    tar.append(&header, host)?;
    tar.into_inner()
}

fn checksum(path: &Path, bytes: &[u8]) -> String {
    format!(
        "{:x}  {}\n",
        Sha256::digest(bytes),
        path.file_name().unwrap_or_default().to_string_lossy()
    )
}

pub fn package_distribution(root: &Path, build_id: &str, report: &Value) -> io::Result<PathBuf> {
    let host = root.join("target/release/cdenv");
    if !fs::symlink_metadata(&host)?.is_file() {
        return Err(io::Error::other("release host is not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(&host)?.permissions().mode() & 0o111 == 0 {
            return Err(io::Error::other("release host is not executable"));
        }
    }
    let bytes = fs::read(&host)?;
    validate_host(&bytes, &platform())?;
    let destination = root.join("target/dist");
    fs::create_dir_all(&destination)?;
    let archive = destination.join(format!("cdenv-{}-{build_id}.tar", platform()));
    let bytes = archive_bytes(&bytes)?;
    fs::write(&archive, &bytes)?;
    fs::write(
        archive.with_extension("tar.sha256"),
        checksum(&archive, &bytes),
    )?;
    let metadata = json!({"platform": platform(), "artifacts": report});
    fs::write(archive.with_extension("tar.json"), metadata.to_string())?;
    smoke(&archive)?;
    Ok(archive)
}

// Validate before extraction, including canonical tar bytes, so duplicate members,
// links, extension headers, nonzero padding and trailing payloads cannot hide files.
fn unpack_checked(bytes: &[u8], platform: &str) -> io::Result<Vec<u8>> {
    let mut archive = tar::Archive::new(bytes);
    let mut entries = archive.entries()?;
    let mut entry = entries
        .next()
        .ok_or_else(|| io::Error::other("missing host"))??;
    if entry.path()?.as_ref() != Path::new("cdenv")
        || entry.header().entry_type() != tar::EntryType::Regular
        || entry.header().mode()? != 0o755
    {
        return Err(io::Error::other("unexpected archive member or mode"));
    }
    let mut host = Vec::new();
    entry.read_to_end(&mut host)?;
    if entries.next().is_some() || archive_bytes(&host)? != bytes {
        return Err(io::Error::other(
            "unexpected or noncanonical archive contents",
        ));
    }
    validate_host(&host, platform)?;
    Ok(host)
}

pub fn smoke(archive: &Path) -> io::Result<()> {
    let bytes = fs::read(archive)?;
    if fs::read_to_string(archive.with_extension("tar.sha256"))? != checksum(archive, &bytes) {
        return Err(io::Error::other("distribution checksum mismatch"));
    }
    let metadata: Value = serde_json::from_slice(&fs::read(archive.with_extension("tar.json"))?)?;
    if metadata["platform"] != platform() {
        return Err(io::Error::other(
            "package smoke must run on the declared host",
        ));
    }
    let report = &metadata["artifacts"];
    let id = report["buildId"]
        .as_str()
        .ok_or_else(|| io::Error::other("missing build ID"))?;
    if archive.file_name().unwrap_or_default() != format!("cdenv-{}-{id}.tar", platform()).as_str()
    {
        return Err(io::Error::other("archive name/identity mismatch"));
    }
    let host = unpack_checked(&bytes, &platform())?;
    let temporary = tempfile::tempdir()?;
    let binary = temporary.path().join("cdenv");
    fs::write(&binary, host)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755))?;
    }
    let version = Command::new(&binary)
        .arg("--version")
        .env_clear()
        .output()?;
    if !version.status.success()
        || !version.stderr.is_empty()
        || version.stdout != format!("cdenv {}\n", env!("CARGO_PKG_VERSION")).as_bytes()
    {
        return Err(io::Error::other("packaged host version mismatch"));
    }
    let validation = Command::new(&binary)
        .arg("__validate-artifacts")
        .env_clear()
        .output()?;
    if !validation.status.success()
        || !validation.stderr.is_empty()
        || serde_json::from_slice::<Value>(&validation.stdout)? != *report
    {
        return Err(io::Error::other(
            "packaged embedded artifact identity/checksum mismatch",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elf() -> Vec<u8> {
        let mut bytes = vec![0; 64];
        bytes[..6].copy_from_slice(b"\x7fELF\x02\x01");
        bytes[16] = 3;
        bytes[18] = 62;
        bytes[24] = 1;
        bytes
    }

    #[test]
    fn archive_is_deterministic_and_contains_only_canonical_executable() {
        let bytes = archive_bytes(&elf()).expect("archive");
        assert_eq!(unpack_checked(&bytes, "linux-x86_64").expect("host"), elf());
        assert_eq!(bytes, archive_bytes(&elf()).expect("repeat"));
    }

    #[test]
    fn rejects_empty_wrong_format_and_wrong_architecture_hosts() {
        for bytes in [vec![], b"#!/bin/sh".to_vec(), elf()] {
            assert!(
                unpack_checked(&archive_bytes(&bytes).expect("archive"), "linux-aarch64").is_err()
            );
        }
    }

    #[test]
    fn rejects_missing_host() {
        assert!(unpack_checked(&[0; 1024], "linux-x86_64").is_err());
    }

    #[test]
    fn rejects_unexpected_trailing_files() {
        let mut bytes = archive_bytes(&elf()).expect("archive");
        bytes.extend(archive_bytes(b"unexpected").expect("extra"));
        assert!(unpack_checked(&bytes, "linux-x86_64").is_err());
    }

    #[test]
    fn rejects_duplicate_unexpected_link_and_nonexecutable_members() {
        for (name, kind, mode, duplicate) in [
            ("extra", tar::EntryType::Regular, 0o755, false),
            ("cdenv", tar::EntryType::Symlink, 0o755, false),
            ("cdenv", tar::EntryType::Regular, 0o644, false),
            ("cdenv", tar::EntryType::Regular, 0o755, true),
        ] {
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(kind);
            header.set_mode(mode);
            header.set_size(64);
            builder
                .append_data(&mut header, name, elf().as_slice())
                .expect("member");
            if duplicate {
                builder
                    .append_data(&mut header, name, elf().as_slice())
                    .expect("duplicate");
            }
            assert!(
                unpack_checked(&builder.into_inner().expect("archive"), "linux-x86_64").is_err()
            );
        }
    }

    #[test]
    fn rejects_missing_release_host() {
        let root = tempfile::tempdir().expect("root");
        assert!(package_distribution(root.path(), "build", &Value::Null).is_err());
    }

    #[test]
    fn rejects_missing_agent_artifacts() {
        let stage = tempfile::tempdir().expect("stage");
        assert!(agent_report(stage.path(), "build").is_err());
    }

    #[test]
    fn rejects_checksum_mismatch_before_executing_host() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("release.tar");
        fs::write(&path, archive_bytes(&elf()).expect("archive")).expect("write");
        fs::write(path.with_extension("tar.sha256"), "bad").expect("checksum");
        assert!(
            smoke(&path)
                .expect_err("mismatch")
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    fn rejects_checksum_with_correct_digest_but_wrong_filename() {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("release.tar");
        let bytes = archive_bytes(&elf()).expect("archive");
        fs::write(&path, &bytes).expect("write");
        fs::write(
            path.with_extension("tar.sha256"),
            checksum(Path::new("other.tar"), &bytes),
        )
        .expect("checksum");
        assert!(
            smoke(&path)
                .expect_err("mismatch")
                .to_string()
                .contains("checksum")
        );
    }

    #[test]
    fn rejects_wrong_agent_identity_and_protocol() {
        for (id, protocol) in [("wrong", 1), ("build", 10)] {
            let report = json!({"name":"cdenv-agent", "buildId":id, "protocolVersion":protocol, "version":env!("CARGO_PKG_VERSION")});
            assert!(!valid_agent_version(report.to_string().as_bytes(), "build"));
        }
    }

    #[test]
    fn validates_both_macho_architectures() {
        for (platform, cpu) in [
            ("macos-x86_64", 0x0100_0007_u32),
            ("macos-aarch64", 0x0100_000c),
        ] {
            let mut bytes = vec![0; 32];
            bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
            bytes[4..8].copy_from_slice(&cpu.to_le_bytes());
            bytes[12] = 2;
            assert!(validate_host(&bytes, platform).is_ok());
            bytes[4] ^= 1;
            assert!(validate_host(&bytes, platform).is_err());
        }
    }
}
