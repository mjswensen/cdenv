//! Safe Feature archive extraction.
use super::cache::random_suffix;
use super::{FeatureSourceError, FeatureSourceLimits};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path};

const COMPLETE_MARKER: &str = ".cdenv-feature-complete";

/// Safely extracts a gzip-compressed or plain tar archive into a newly-created directory.
///
/// # Errors
/// Rejects traversal, links, special files, duplicates, and configured bounds.
pub fn extract_archive(
    archive: &Path,
    destination: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    if destination.exists() {
        return validate_extraction(destination);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| FeatureSourceError::Archive {
            path: destination.to_path_buf(),
            message: "destination has no parent",
        })?;
    fs::create_dir_all(parent).map_err(|source| FeatureSourceError::ArchiveIo {
        path: parent.to_path_buf(),
        source,
    })?;
    let temporary = parent.join(format!(".extract-{}", random_suffix()?));
    fs::create_dir(&temporary).map_err(|source| FeatureSourceError::ArchiveIo {
        path: temporary.clone(),
        source,
    })?;
    let result = extract_into(archive, &temporary, limits).and_then(|()| {
        File::create_new(temporary.join(COMPLETE_MARKER))
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: temporary.join(COMPLETE_MARKER),
                source,
            })?
            .sync_all()
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: temporary.join(COMPLETE_MARKER),
                source,
            })
    });
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary, destination) {
        let _ = fs::remove_dir_all(&temporary);
        if destination.exists() {
            return validate_extraction(destination);
        }
        return Err(FeatureSourceError::ArchiveIo {
            path: destination.to_path_buf(),
            source,
        });
    }
    Ok(())
}
fn validate_extraction(destination: &Path) -> Result<(), FeatureSourceError> {
    let metadata =
        fs::symlink_metadata(destination).map_err(|source| FeatureSourceError::ArchiveIo {
            path: destination.to_path_buf(),
            source,
        })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(FeatureSourceError::Archive {
            path: destination.to_path_buf(),
            message: "existing extraction is not a regular directory",
        });
    }
    let marker = destination.join(COMPLETE_MARKER);
    let marker_metadata =
        fs::symlink_metadata(&marker).map_err(|source| FeatureSourceError::ArchiveIo {
            path: marker.clone(),
            source,
        })?;
    if !marker_metadata.is_file() || marker_metadata.file_type().is_symlink() {
        return Err(FeatureSourceError::Archive {
            path: marker,
            message: "existing extraction is incomplete or unsafe",
        });
    }
    let mut pending = vec![destination.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| FeatureSourceError::ArchiveIo {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| FeatureSourceError::ArchiveIo {
                path: directory.clone(),
                source,
            })?;
            let path = entry.path();
            let metadata =
                fs::symlink_metadata(&path).map_err(|source| FeatureSourceError::ArchiveIo {
                    path: path.clone(),
                    source,
                })?;
            if metadata.file_type().is_symlink() || !(metadata.is_dir() || metadata.is_file()) {
                return Err(FeatureSourceError::Archive {
                    path,
                    message: "existing extraction contains an unsafe entry",
                });
            }
            if metadata.is_dir() {
                pending.push(path);
            }
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_lines,
    reason = "the streaming archive state and all bound checks remain visible together"
)]
fn extract_into(
    archive: &Path,
    destination: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    let compressed = fs::metadata(archive)
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?
        .len()
        .max(1);
    let file = File::open(archive).map_err(|source| FeatureSourceError::ArchiveIo {
        path: archive.to_path_buf(),
        source,
    })?;
    let mut magic = [0_u8; 2];
    let mut buffered = BufReader::new(file);
    let count = buffered
        .read(&mut magic)
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
    buffered =
        BufReader::new(
            File::open(archive).map_err(|source| FeatureSourceError::ArchiveIo {
                path: archive.to_path_buf(),
                source,
            })?,
        );
    let reader: Box<dyn Read> = if count == 2 && magic == [0x1f, 0x8b] {
        Box::new(flate2::read::GzDecoder::new(buffered))
    } else {
        Box::new(buffered)
    };
    let mut tar = tar::Archive::new(reader);
    let entries = tar
        .entries()
        .map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
    let mut seen = BTreeSet::new();
    let mut files = 0_usize;
    let mut extracted = 0_u64;
    for entry in entries {
        let mut entry = entry.map_err(|source| FeatureSourceError::ArchiveIo {
            path: archive.to_path_buf(),
            source,
        })?;
        files += 1;
        if files > limits.files {
            return Err(FeatureSourceError::Limit {
                kind: "archive entries",
                limit: limits.files as u64,
            });
        }
        let path = entry
            .path()
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: archive.to_path_buf(),
                source,
            })?
            .into_owned();
        validate_archive_path(&path, limits.path_bytes)?;
        if !seen.insert(path.clone()) {
            return Err(FeatureSourceError::Archive {
                path,
                message: "duplicate archive path",
            });
        }
        let kind = entry.header().entry_type();
        if !(kind.is_file() || kind.is_dir()) {
            return Err(FeatureSourceError::Archive {
                path,
                message: "links and special entry types are unsupported",
            });
        }
        let target = destination.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|source| FeatureSourceError::ArchiveIo {
                path: target,
                source,
            })?;
            continue;
        }
        let size = entry.size();
        extracted = extracted
            .checked_add(size)
            .ok_or(FeatureSourceError::Limit {
                kind: "extracted bytes",
                limit: limits.extracted_bytes,
            })?;
        let expansion_limit =
            compressed
                .checked_mul(limits.expansion_ratio)
                .ok_or(FeatureSourceError::Limit {
                    kind: "archive expansion ratio",
                    limit: limits.expansion_ratio,
                })?;
        if extracted > limits.extracted_bytes || extracted > expansion_limit {
            return Err(FeatureSourceError::Limit {
                kind: "extracted bytes/ratio",
                limit: limits.extracted_bytes.min(expansion_limit),
            });
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| FeatureSourceError::ArchiveIo {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&target)
            .map_err(|source| FeatureSourceError::ArchiveIo {
                path: target.clone(),
                source,
            })?;
        io::copy(&mut entry, &mut output).map_err(|source| FeatureSourceError::ArchiveIo {
            path: target,
            source,
        })?;
    }
    Ok(())
}
fn validate_archive_path(path: &Path, max: usize) -> Result<(), FeatureSourceError> {
    let text = path.to_str().ok_or_else(|| FeatureSourceError::Archive {
        path: path.to_path_buf(),
        message: "path is not UTF-8",
    })?;
    if text.len() > max {
        return Err(FeatureSourceError::Limit {
            kind: "archive path bytes",
            limit: max as u64,
        });
    }
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(FeatureSourceError::Archive {
            path: path.to_path_buf(),
            message: "path is absolute or traversing",
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn archive_path_policy_rejects_absolute_and_parent_paths_before_io() {
        for path in ["/escape", "../escape", "safe/../../escape"] {
            assert!(
                validate_archive_path(Path::new(path), 4096).is_err(),
                "accepted {path}"
            );
        }
    }

    #[test]
    fn archive_path_policy_accepts_exact_byte_limit_but_rejects_one_over() {
        assert!(validate_archive_path(Path::new("safe/file"), 9).is_ok());
        assert!(validate_archive_path(Path::new("safe/file"), 8).is_err());
    }

    fn archive(path: &Path, name: &str) {
        let file = File::create(path).expect("archive");
        let mut tar = tar::Builder::new(file);
        let bytes = br#"{"id":"fixture","version":"1"}"#;
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, name, Cursor::new(bytes))
            .expect("entry");
        tar.finish().expect("finish");
    }

    fn archive_with_entries(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("archive");
        let mut tar = tar::Builder::new(file);
        for (name, bytes) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, name, Cursor::new(*bytes))
                .expect("entry");
        }
        tar.finish().expect("finish");
    }

    #[test]
    fn extraction_creates_a_complete_regular_directory() {
        let temporary = tempfile::tempdir().expect("temp");
        let archive_path = temporary.path().join("feature.tar");
        archive(&archive_path, "devcontainer-feature.json");
        let destination = temporary.path().join("out");
        extract_archive(&archive_path, &destination, FeatureSourceLimits::default())
            .expect("safe archive");
        assert!(destination.join("devcontainer-feature.json").is_file());
        extract_archive(&archive_path, &destination, FeatureSourceLimits::default())
            .expect("validated cache reuse");
    }

    #[test]
    fn extraction_rejects_parent_traversal() {
        let temporary = tempfile::tempdir().expect("temp");
        let bad = temporary.path().join("bad.tar");
        let file = File::create(&bad).expect("archive");
        let mut builder = tar::Builder::new(file);
        let mut header = tar::Header::new_gnu();
        header.set_size(1);
        header.set_mode(0o644);
        header.as_mut_bytes()[..10].copy_from_slice(b"../escape\0");
        header.set_cksum();
        builder.append(&header, Cursor::new(b"x")).expect("entry");
        builder.finish().expect("finish");
        assert!(
            extract_archive(
                &bad,
                &temporary.path().join("bad-out"),
                FeatureSourceLimits::default()
            )
            .is_err()
        );
        assert!(!temporary.path().join("escape").exists());
    }

    #[test]
    fn extraction_rejects_existing_directory_without_completion_marker() {
        let temporary = tempfile::tempdir().expect("temp");
        let archive_path = temporary.path().join("feature.tar");
        archive(&archive_path, "devcontainer-feature.json");
        let destination = temporary.path().join("out");
        fs::create_dir(&destination).expect("incomplete extraction");

        assert!(
            extract_archive(&archive_path, &destination, FeatureSourceLimits::default()).is_err()
        );
    }

    #[test]
    fn extraction_limits_accept_exact_bounds_and_reject_one_over() {
        let temporary = tempfile::tempdir().expect("temp");
        let archive_path = temporary.path().join("entries.tar");
        archive_with_entries(&archive_path, &[("one", b"a"), ("two", b"b")]);
        let limits = FeatureSourceLimits {
            files: 2,
            extracted_bytes: 2,
            expansion_ratio: 10,
            ..FeatureSourceLimits::default()
        };
        extract_archive(&archive_path, &temporary.path().join("accepted"), limits)
            .expect("exact entry and byte limits");

        let entries_over = FeatureSourceLimits { files: 1, ..limits };
        assert!(
            extract_archive(
                &archive_path,
                &temporary.path().join("entries-over"),
                entries_over
            )
            .is_err()
        );
        let bytes_over = FeatureSourceLimits {
            extracted_bytes: 1,
            ..limits
        };
        assert!(
            extract_archive(
                &archive_path,
                &temporary.path().join("bytes-over"),
                bytes_over
            )
            .is_err()
        );
    }
}
