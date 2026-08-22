//! Digest-addressed Feature blob cache mutation and verification.
use super::{FeatureSourceError, FeatureSourceResolver};
use futures_util::StreamExt;
use reqwest::Response;
use reqwest::header::CONTENT_LENGTH;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Read, Write};
use std::path::{Path, PathBuf};

impl FeatureSourceResolver {
    pub(super) async fn cache_response(
        &self,
        response: Response,
        expected_digest: Option<&str>,
        expected_size: Option<u64>,
    ) -> Result<(PathBuf, String, u64), FeatureSourceError> {
        if let Some(length) = response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok())
            && length > self.limits.blob_bytes
        {
            return Err(FeatureSourceError::Limit {
                kind: "download bytes",
                limit: self.limits.blob_bytes,
            });
        }
        fs::create_dir_all(&self.blobs).map_err(|source| FeatureSourceError::Cache {
            path: self.blobs.clone(),
            source,
        })?;
        let temporary = self.blobs.join(format!(".download-{}", random_suffix()?));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| FeatureSourceError::Cache {
                path: temporary.clone(),
                source,
            })?;
        let mut hash = Sha256::new();
        let mut size = 0_u64;
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|source| FeatureSourceError::Transport {
                url: "<response-body>".to_owned(),
                source,
            })?;
            size = match checked_download_size(size, chunk.len() as u64, self.limits.blob_bytes) {
                Ok(size) => size,
                Err(error) => {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
            };
            hash.update(&chunk);
            output
                .write_all(&chunk)
                .map_err(|source| FeatureSourceError::Cache {
                    path: temporary.clone(),
                    source,
                })?;
        }
        output
            .sync_all()
            .map_err(|source| FeatureSourceError::Cache {
                path: temporary.clone(),
                source,
            })?;
        let digest = format!("sha256:{}", hex::encode(hash.finalize()));
        if let Some(expected) = expected_digest
            && digest != expected
        {
            let _ = fs::remove_file(&temporary);
            return Err(FeatureSourceError::Digest {
                expected: expected.to_owned(),
                actual: digest,
            });
        }
        if let Some(expected) = expected_size
            && size != expected
        {
            let _ = fs::remove_file(&temporary);
            return Err(FeatureSourceError::Oci(format!(
                "blob size mismatch: expected {expected}, observed {size}"
            )));
        }
        let destination = self.cache_path(&digest);
        if destination.exists() {
            let _ = fs::remove_file(&temporary);
            verify_cached(&destination, &digest, Some(size))?;
        } else if let Err(source) = fs::rename(&temporary, &destination) {
            if destination.exists() {
                let _ = fs::remove_file(&temporary);
                verify_cached(&destination, &digest, Some(size))?;
            } else {
                return Err(FeatureSourceError::Cache {
                    path: destination,
                    source,
                });
            }
        }
        Ok((destination, digest, size))
    }

    pub(super) fn cache_path(&self, digest: &str) -> PathBuf {
        self.blobs.join(digest.trim_start_matches("sha256:"))
    }
}

fn checked_download_size(
    current: u64,
    additional: u64,
    limit: u64,
) -> Result<u64, FeatureSourceError> {
    let size = current
        .checked_add(additional)
        .ok_or(FeatureSourceError::Limit {
            kind: "download bytes",
            limit,
        })?;
    if size > limit {
        return Err(FeatureSourceError::Limit {
            kind: "download bytes",
            limit,
        });
    }
    Ok(size)
}

pub(super) fn validate_digest(value: &str) -> Result<(), FeatureSourceError> {
    let hex = value
        .strip_prefix("sha256:")
        .ok_or_else(|| FeatureSourceError::Oci("only SHA-256 digests are supported".to_owned()))?;
    if hex.len() == 64
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(())
    } else {
        Err(FeatureSourceError::Oci(
            "malformed SHA-256 digest".to_owned(),
        ))
    }
}
pub(super) fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}
pub(super) fn verify_digest(expected: &str, bytes: &[u8]) -> Result<(), FeatureSourceError> {
    let actual = digest_bytes(bytes);
    if actual == expected {
        Ok(())
    } else {
        Err(FeatureSourceError::Digest {
            expected: expected.to_owned(),
            actual,
        })
    }
}
pub(super) fn verify_cached(
    path: &Path,
    digest: &str,
    size: Option<u64>,
) -> Result<bool, FeatureSourceError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(FeatureSourceError::Cache {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(FeatureSourceError::Cache {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidData, "cache entry is not regular"),
        });
    }
    let file = File::open(path).map_err(|source| FeatureSourceError::Cache {
        path: path.to_path_buf(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| FeatureSourceError::Cache {
            path: path.to_path_buf(),
            source,
        })?
        .is_file()
    {
        return Err(FeatureSourceError::Cache {
            path: path.to_path_buf(),
            source: io::Error::new(io::ErrorKind::InvalidData, "cache entry is not regular"),
        });
    }
    let mut reader = BufReader::new(file);
    let mut hash = Sha256::new();
    let mut observed = 0_u64;
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|source| FeatureSourceError::Cache {
                path: path.to_path_buf(),
                source,
            })?;
        if count == 0 {
            break;
        }
        observed += count as u64;
        hash.update(&buffer[..count]);
    }
    let actual = format!("sha256:{}", hex::encode(hash.finalize()));
    if actual != digest {
        return Err(FeatureSourceError::Digest {
            expected: digest.to_owned(),
            actual,
        });
    }
    if size.is_some_and(|expected| expected != observed) {
        return Err(FeatureSourceError::Oci(
            "cached blob size mismatch".to_owned(),
        ));
    }
    Ok(true)
}
pub(super) fn random_suffix() -> Result<String, FeatureSourceError> {
    let mut bytes = [0_u8; 12];
    getrandom::fill(&mut bytes).map_err(|error| FeatureSourceError::Cache {
        path: PathBuf::from("<random>"),
        source: io::Error::other(error.to_string()),
    })?;
    Ok(hex::encode(bytes))
}
pub(super) fn extraction_directory(blobs: &Path, digest: &str) -> PathBuf {
    blobs
        .parent()
        .unwrap_or(blobs)
        .join("extracted")
        .join(digest.trim_start_matches("sha256:"))
}

pub(super) fn cache_path(blobs: &Path, digest: &str) -> PathBuf {
    blobs.join(digest.trim_start_matches("sha256:"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloaded_size_accepts_the_limit_and_detects_one_byte_overflow() {
        assert_eq!(checked_download_size(63, 1, 64).expect("exact limit"), 64);
        assert!(checked_download_size(64, 1, 64).is_err());
        assert!(checked_download_size(u64::MAX, 1, 64).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn cache_verification_rejects_a_symlink_before_hashing() {
        let temporary = tempfile::tempdir().expect("temp");
        let target = temporary.path().join("target");
        fs::write(&target, b"cache").expect("target");
        let entry = temporary.path().join("entry");
        std::os::unix::fs::symlink(&target, &entry).expect("symlink");

        assert!(verify_cached(&entry, &digest_bytes(b"cache"), Some(5)).is_err());
    }
}
