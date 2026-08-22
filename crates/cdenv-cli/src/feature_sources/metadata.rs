//! Bounded Feature metadata parsing.
use super::FeatureSourceError;
use cdenv_devcontainer::FeatureMetadata;
use std::fs::File;
use std::io::Read;
use std::path::Path;

pub(super) fn read_feature_metadata(
    root: &Path,
    limit: u64,
) -> Result<FeatureMetadata, FeatureSourceError> {
    let path = root.join("devcontainer-feature.json");
    let file = File::open(&path).map_err(|source| FeatureSourceError::Metadata {
        path: path.clone(),
        source,
    })?;
    let mut bytes = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|source| FeatureSourceError::Metadata {
            path: path.clone(),
            source,
        })?;
    if bytes.len() as u64 > limit {
        return Err(FeatureSourceError::Limit {
            kind: "Feature metadata bytes",
            limit,
        });
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|source| FeatureSourceError::MetadataJson { path, source })?;
    Ok(FeatureMetadata::from_value(&value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_limit_accepts_the_exact_size_and_rejects_one_byte_over() {
        let temporary = tempfile::tempdir().expect("temp");
        let bytes = br#"{"id":"fixture","version":"1"}"#;
        std::fs::write(temporary.path().join("devcontainer-feature.json"), bytes)
            .expect("metadata");

        read_feature_metadata(temporary.path(), bytes.len() as u64).expect("exact limit");
        assert!(read_feature_metadata(temporary.path(), bytes.len() as u64 - 1).is_err());
    }
}
