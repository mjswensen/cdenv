//! Local Feature source containment and deterministic tree identities.
use super::metadata::read_feature_metadata;
use super::{FeatureSourceError, FeatureSourceLimits, VerifiedFeature};
use cdenv_devcontainer::{FeatureInstallIdentity, FeaturePackage, FeatureReference};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Read;
use std::path::Path;

pub(super) fn resolve_local(
    reference: &FeatureReference,
    value: &str,
    base: &Path,
    limits: FeatureSourceLimits,
) -> Result<VerifiedFeature, FeatureSourceError> {
    let canonical_base = fs::canonicalize(base).map_err(|source| FeatureSourceError::Cache {
        path: base.to_path_buf(),
        source,
    })?;
    let candidate = base.join(value.strip_prefix("./").unwrap_or(value));
    let source =
        fs::canonicalize(&candidate).map_err(|_| FeatureSourceError::LocalContainment {
            path: candidate.clone(),
        })?;
    let metadata =
        fs::symlink_metadata(&source).map_err(|_| FeatureSourceError::LocalContainment {
            path: source.clone(),
        })?;
    if !source.starts_with(&canonical_base)
        || !metadata.is_dir()
        || metadata.file_type().is_symlink()
    {
        return Err(FeatureSourceError::LocalContainment { path: source });
    }
    validate_local_tree(&source, &canonical_base, limits)?;
    let feature_metadata = read_feature_metadata(&source, limits.metadata_bytes)?;
    let integrity = hash_local_tree(&source)?;
    Ok(VerifiedFeature {
        package: FeaturePackage {
            reference: reference.clone(),
            identity: FeatureInstallIdentity::local(reference),
            metadata: feature_metadata,
            digest: None,
            integrity: Some(integrity),
        },
        artifact: source,
    })
}

fn validate_local_tree(
    root: &Path,
    base: &Path,
    limits: FeatureSourceLimits,
) -> Result<(), FeatureSourceError> {
    let mut pending = vec![root.to_path_buf()];
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).map_err(|source| FeatureSourceError::Cache {
            path: directory.clone(),
            source,
        })? {
            let entry = entry.map_err(|source| FeatureSourceError::Cache {
                path: directory.clone(),
                source,
            })?;
            count += 1;
            if count > limits.files {
                return Err(FeatureSourceError::Limit {
                    kind: "local files",
                    limit: limits.files as u64,
                });
            }
            let metadata =
                fs::symlink_metadata(entry.path()).map_err(|source| FeatureSourceError::Cache {
                    path: entry.path(),
                    source,
                })?;
            if metadata.file_type().is_symlink() {
                return Err(FeatureSourceError::LocalContainment { path: entry.path() });
            }
            if metadata.is_dir() {
                let canonical = fs::canonicalize(entry.path())
                    .map_err(|_| FeatureSourceError::LocalContainment { path: entry.path() })?;
                if !canonical.starts_with(base) {
                    return Err(FeatureSourceError::LocalContainment { path: canonical });
                }
                pending.push(canonical);
            } else if metadata.is_file() {
                bytes = bytes
                    .checked_add(metadata.len())
                    .ok_or(FeatureSourceError::Limit {
                        kind: "local bytes",
                        limit: limits.extracted_bytes,
                    })?;
                if bytes > limits.extracted_bytes {
                    return Err(FeatureSourceError::Limit {
                        kind: "local bytes",
                        limit: limits.extracted_bytes,
                    });
                }
            } else {
                return Err(FeatureSourceError::LocalContainment { path: entry.path() });
            }
        }
    }
    Ok(())
}
fn hash_local_tree(root: &Path) -> Result<String, FeatureSourceError> {
    fn visit(root: &Path, directory: &Path, hash: &mut Sha256) -> Result<(), FeatureSourceError> {
        let mut entries = fs::read_dir(directory)
            .map_err(|source| FeatureSourceError::Cache {
                path: directory.to_path_buf(),
                source,
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|source| FeatureSourceError::Cache {
                path: directory.to_path_buf(),
                source,
            })?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let relative = path
                .strip_prefix(root)
                .map_err(|_| FeatureSourceError::LocalContainment { path: path.clone() })?;
            let metadata = entry
                .metadata()
                .map_err(|source| FeatureSourceError::Cache {
                    path: path.clone(),
                    source,
                })?;
            hash.update((relative.as_os_str().len() as u64).to_be_bytes());
            hash.update(relative.as_os_str().as_encoded_bytes());
            hash.update([u8::from(metadata.is_dir())]);
            if metadata.is_dir() {
                visit(root, &path, hash)?;
            } else {
                let mut file = File::open(&path).map_err(|source| FeatureSourceError::Cache {
                    path: path.clone(),
                    source,
                })?;
                let mut buffer = [0_u8; 8192];
                loop {
                    let count =
                        file.read(&mut buffer)
                            .map_err(|source| FeatureSourceError::Cache {
                                path: path.clone(),
                                source,
                            })?;
                    if count == 0 {
                        break;
                    }
                    hash.update(&buffer[..count]);
                }
            }
        }
        Ok(())
    }
    let mut hash = Sha256::new();
    visit(root, root, &mut hash)?;
    Ok(format!("sha256:{}", hex::encode(hash.finalize())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_source_must_be_contained_and_link_free() {
        let temporary = tempfile::tempdir().expect("temp");
        let feature = temporary.path().join("features/tool");
        fs::create_dir_all(&feature).expect("dir");
        fs::write(
            feature.join("devcontainer-feature.json"),
            br#"{"id":"tool","version":"1"}"#,
        )
        .expect("metadata");
        let reference = FeatureReference::parse("./features/tool").expect("reference");
        let result = resolve_local(
            &reference,
            reference.as_str(),
            temporary.path(),
            FeatureSourceLimits::default(),
        )
        .expect("local");
        assert_eq!(result.package.metadata.id, "tool");
    }
}
