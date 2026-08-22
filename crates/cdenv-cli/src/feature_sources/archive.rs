//! Safe Feature archive extraction.
use super::cache::random_suffix;
use super::{FeatureSourceError, FeatureSourceLimits};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read};
use std::path::{Component, Path};

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
        return Ok(());
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
    let result = extract_into(archive, &temporary, limits);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&temporary);
        return Err(error);
    }
    if let Err(source) = fs::rename(&temporary, destination) {
        if destination.is_dir() {
            let _ = fs::remove_dir_all(&temporary);
        } else {
            let _ = fs::remove_dir_all(&temporary);
            return Err(FeatureSourceError::ArchiveIo {
                path: destination.to_path_buf(),
                source,
            });
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
    fn extraction_accepts_metadata_and_rejects_traversal() {
        let temporary = tempfile::tempdir().expect("temp");
        let good = temporary.path().join("good.tar");
        archive(&good, "devcontainer-feature.json");
        extract_archive(
            &good,
            &temporary.path().join("out"),
            FeatureSourceLimits::default(),
        )
        .expect("safe archive");
        assert!(
            temporary
                .path()
                .join("out/devcontainer-feature.json")
                .is_file()
        );
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
