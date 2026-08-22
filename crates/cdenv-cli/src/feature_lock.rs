//! Frozen Feature lock validation and the explicit checkout mutation boundary.

use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use cdenv_devcontainer::{
    FeatureLock, FeatureOptionValue, FeaturePackage, FeatureReference, FeatureRequest,
    FeatureSource, FeatureValue, ParseLimits, RawProfile, ResolvedFeatures, resolve_features,
    validate_profile,
};
use thiserror::Error;

use crate::{
    CdenvRoot, FeatureSourceError, FeatureSourceResolver, LockArgs, LockBehavior, LockError,
    LockGuard, LockMode, WorkspaceStateError, discover_and_read_config, load_workspace_state,
};

/// Standard adjacent Feature lockfile name.
pub const FEATURE_LOCK_FILE: &str = "devcontainer-lock.json";

/// Result of a non-network lock policy check for an existing container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExistingContainerLockStatus {
    /// Configured roots still match the lock.
    Current,
    /// The lock is absent; reproducibility cannot be guaranteed.
    Missing,
    /// Configured roots changed; `up` may continue with its existing container and warn.
    Drift,
}

/// Checks lock drift for an existing-container `up` without source or registry access.
///
/// # Errors
/// Returns an invalid-lock error when present bytes are corrupt rather than stale.
pub fn inspect_existing_container_lock(
    bytes: Option<&[u8]>,
    requests: &[FeatureRequest],
) -> Result<ExistingContainerLockStatus, cdenv_devcontainer::FeatureError> {
    let Some(bytes) = bytes else {
        return Ok(ExistingContainerLockStatus::Missing);
    };
    let lock = FeatureLock::parse(bytes)?;
    Ok(if lock.validate_requested(requests).is_ok() {
        ExistingContainerLockStatus::Current
    } else {
        ExistingContainerLockStatus::Drift
    })
}

/// Resolves every record from verified digest cache and validates the frozen graph offline.
///
/// # Errors
/// Returns stale/integrity/cache errors with `cdenv lock` guidance at the caller boundary.
pub fn resolve_frozen_features_offline(
    lock: &FeatureLock,
    requests: &[FeatureRequest],
    override_order: &[String],
    resolver: &FeatureSourceResolver,
    configuration_directory: &Path,
) -> Result<ResolvedFeatures, FeatureLockError> {
    lock.validate_requested(requests)
        .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    let mut packages = BTreeMap::new();
    for (value, record) in &lock.features {
        let reference = FeatureReference::parse(value)
            .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
        let package = resolver
            .resolve_locked(&reference, record, configuration_directory)?
            .package;
        packages.insert(reference, package);
    }
    let plan = resolve_features(requests, &packages, override_order)
        .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    lock.validate_frozen(requests, &plan, &packages)
        .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    Ok(plan)
}

/// Failure while selecting, resolving, or replacing a Feature lockfile.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum FeatureLockError {
    /// Workspace coordination failed.
    #[error(transparent)]
    WorkspaceLock(#[from] LockError),
    /// Workspace state could not be read.
    #[error(transparent)]
    State(#[from] WorkspaceStateError),
    /// Configuration discovery or reading failed.
    #[error("cannot load selected Dev Container configuration: {0}")]
    Config(String),
    /// Profile parsing or validation failed.
    #[error("cannot validate selected Dev Container configuration: {0}")]
    Profile(String),
    /// Feature retrieval failed.
    #[error(transparent)]
    Source(#[from] FeatureSourceError),
    /// Pure Feature resolution or lock generation failed.
    #[error("cannot resolve Feature lock: {0}")]
    Resolution(String),
    /// The lock target is outside the checkout or is not a regular file.
    #[error("unsafe Feature lock target: {path:?}: {message}")]
    Target {
        /// Rejected target.
        path: PathBuf,
        /// Rejection reason.
        message: &'static str,
    },
    /// Lockfile replacement failed.
    #[error("cannot atomically replace Feature lock {path:?}: {source}")]
    Write {
        /// Target path.
        path: PathBuf,
        /// Filesystem failure.
        #[source]
        source: io::Error,
    },
    /// Runtime setup failed.
    #[error("cannot start Feature resolver runtime: {0}")]
    Runtime(String),
}

/// Runs `cdenv lock` under the workspace's exclusive lifecycle lock.
///
/// `--config` affects only this invocation; workspace desired state is never persisted.
///
/// # Errors
/// Returns a typed selection, source, resolution, containment, or atomic-write failure.
pub fn lock_workspace(root: &CdenvRoot, arguments: &LockArgs) -> Result<PathBuf, FeatureLockError> {
    let workspace = root.workspace(&arguments.name);
    let _guard = LockGuard::acquire(
        &workspace.lock_file(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )?;
    let checkout = workspace.checkout();
    let selected = if let Some(config) = arguments.config.as_ref() {
        config.as_path().to_path_buf()
    } else {
        PathBuf::from(
            load_workspace_state(&workspace.state_file())?
                .state()
                .desired_devcontainer_config()
                .as_str(),
        )
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| FeatureLockError::Runtime(error.to_string()))?;
    runtime.block_on(generate_feature_lock(
        &checkout,
        Some(&selected),
        &root.cache().blobs_dir(),
    ))
}

/// Resolves a selected profile and atomically writes only its adjacent lockfile.
///
/// # Errors
/// Returns a typed configuration, source, resolution, target, or write failure.
pub async fn generate_feature_lock(
    checkout: &Path,
    selection: Option<&Path>,
    blobs: &Path,
) -> Result<PathBuf, FeatureLockError> {
    let source = discover_and_read_config(checkout, selection)
        .map_err(|error| FeatureLockError::Config(error.to_string()))?
        .ok_or_else(|| {
            FeatureLockError::Config("no Dev Container configuration found".to_owned())
        })?;
    let document =
        cdenv_devcontainer::parse_jsonc(source.path(), source.bytes(), ParseLimits::default())
            .map_err(|error| FeatureLockError::Profile(error.to_string()))?;
    let profile = validate_profile(&document)
        .map_err(|error| FeatureLockError::Profile(error.to_string()))?;
    let config_absolute = checkout.join(source.path().as_str());
    let configuration_directory =
        config_absolute
            .parent()
            .ok_or_else(|| FeatureLockError::Target {
                path: config_absolute.clone(),
                message: "configuration has no parent directory",
            })?;
    let requests = root_requests(&profile)?;
    let resolver = FeatureSourceResolver::new(blobs)?;
    let packages = retrieve_packages(&resolver, &requests, configuration_directory).await?;
    let plan = resolve_features(
        &requests,
        &packages,
        &profile.common.override_feature_install_order,
    )
    .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    let lock = FeatureLock::from_resolution(&requests, &plan, &packages)
        .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    let bytes = lock
        .to_json_bytes()
        .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
    let target = checked_lock_target(checkout, configuration_directory)?;
    atomic_replace_checkout_file(&target, &bytes)?;
    Ok(target)
}

fn root_requests(profile: &RawProfile) -> Result<Vec<FeatureRequest>, FeatureLockError> {
    profile
        .common
        .features
        .values()
        .map(|feature| {
            let value = match &feature.source {
                FeatureSource::Oci(value)
                | FeatureSource::Https(value)
                | FeatureSource::Local(value) => value,
            };
            let reference = FeatureReference::parse(value)
                .map_err(|error| FeatureLockError::Resolution(error.to_string()))?;
            let options = feature
                .options
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        FeatureOptionValue::Boolean(value) => FeatureValue::Boolean(*value),
                        FeatureOptionValue::String(value) => FeatureValue::String(value.clone()),
                    };
                    (name.clone(), value)
                })
                .collect();
            Ok(FeatureRequest { reference, options })
        })
        .collect()
}

async fn retrieve_packages(
    resolver: &FeatureSourceResolver,
    requests: &[FeatureRequest],
    configuration_directory: &Path,
) -> Result<BTreeMap<FeatureReference, FeaturePackage>, FeatureLockError> {
    let mut pending = requests
        .iter()
        .map(|request| request.reference.clone())
        .collect::<VecDeque<_>>();
    let mut packages = BTreeMap::new();
    while let Some(reference) = pending.pop_front() {
        if packages.contains_key(&reference) {
            continue;
        }
        let verified = resolver
            .resolve(&reference, configuration_directory)
            .await?;
        pending.extend(
            verified
                .package
                .metadata
                .depends_on
                .keys()
                .filter(|dependency| !packages.contains_key(*dependency))
                .cloned(),
        );
        packages.insert(reference, verified.package);
    }
    Ok(packages)
}

fn checked_lock_target(
    checkout: &Path,
    configuration_directory: &Path,
) -> Result<PathBuf, FeatureLockError> {
    let checkout = fs::canonicalize(checkout).map_err(|source| FeatureLockError::Write {
        path: checkout.to_path_buf(),
        source,
    })?;
    let directory =
        fs::canonicalize(configuration_directory).map_err(|source| FeatureLockError::Write {
            path: configuration_directory.to_path_buf(),
            source,
        })?;
    if !directory.starts_with(&checkout) {
        return Err(FeatureLockError::Target {
            path: directory,
            message: "configuration directory escapes checkout",
        });
    }
    let target = directory.join(FEATURE_LOCK_FILE);
    match fs::symlink_metadata(&target) {
        Ok(metadata) if !metadata.file_type().is_file() => Err(FeatureLockError::Target {
            path: target,
            message: "existing target is a symlink or non-regular file",
        }),
        Ok(_) => Ok(target),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(target),
        Err(source) => Err(FeatureLockError::Write {
            path: target,
            source,
        }),
    }
}

fn atomic_replace_checkout_file(path: &Path, bytes: &[u8]) -> Result<(), FeatureLockError> {
    let mode = existing_mode(path)?;
    let parent = path.parent().ok_or_else(|| FeatureLockError::Target {
        path: path.to_path_buf(),
        message: "target has no parent",
    })?;
    let (mut file, temporary) = allocate_temporary(parent)?;
    let result = (|| {
        set_mode(&file, mode)?;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if let Err(source) = result {
        let _ = fs::remove_file(&temporary);
        return Err(FeatureLockError::Write {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

fn allocate_temporary(parent: &Path) -> Result<(File, PathBuf), FeatureLockError> {
    for _ in 0..32 {
        let mut random = [0_u8; 12];
        getrandom::fill(&mut random).map_err(|error| FeatureLockError::Write {
            path: parent.to_path_buf(),
            source: io::Error::other(error.to_string()),
        })?;
        let path = parent.join(format!(".devcontainer-lock.{}.tmp", hex::encode(random)));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(FeatureLockError::Write { path, source }),
        }
    }
    Err(FeatureLockError::Write {
        path: parent.to_path_buf(),
        source: io::Error::new(io::ErrorKind::AlreadyExists, "temporary name exhaustion"),
    })
}

#[cfg(unix)]
fn existing_mode(path: &Path) -> Result<u32, FeatureLockError> {
    use std::os::unix::fs::MetadataExt;
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.mode() & 0o777),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0o644),
        Err(source) => Err(FeatureLockError::Write {
            path: path.to_path_buf(),
            source,
        }),
    }
}
#[cfg(not(unix))]
fn existing_mode(_path: &Path) -> Result<u32, FeatureLockError> {
    Ok(0o644)
}

#[cfg(unix)]
fn set_mode(file: &File, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(mode))
}
#[cfg(not(unix))]
fn set_mode(_file: &File, _mode: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(reference: &str) -> FeatureRequest {
        FeatureRequest::new(FeatureReference::parse(reference).expect("reference"))
    }

    fn lock_for(request: &FeatureRequest) -> FeatureLock {
        FeatureLock {
            requested: [(request.reference.to_string(), request.options.clone())]
                .into_iter()
                .collect(),
            ..FeatureLock::default()
        }
    }

    #[test]
    fn existing_container_lock_inspection_is_network_free_and_reports_root_drift() {
        let current_request = request("./features/tool");
        let bytes = lock_for(&current_request).to_json_bytes().expect("lock");
        assert_eq!(
            inspect_existing_container_lock(Some(&bytes), std::slice::from_ref(&current_request))
                .expect("current lock"),
            ExistingContainerLockStatus::Current
        );
        assert_eq!(
            inspect_existing_container_lock(None, std::slice::from_ref(&current_request))
                .expect("missing lock"),
            ExistingContainerLockStatus::Missing
        );
        assert_eq!(
            inspect_existing_container_lock(Some(&bytes), &[request("./features/other")])
                .expect("root drift"),
            ExistingContainerLockStatus::Drift
        );
        let mut changed_options = current_request;
        changed_options
            .options
            .insert("enabled".to_owned(), FeatureValue::Boolean(true));
        assert_eq!(
            inspect_existing_container_lock(Some(&bytes), &[changed_options])
                .expect("option drift"),
            ExistingContainerLockStatus::Drift
        );
    }

    #[test]
    fn existing_container_lock_inspection_rejects_malformed_and_newer_locks() {
        let request = request("./features/tool");
        assert!(
            inspect_existing_container_lock(Some(b"not JSON"), std::slice::from_ref(&request))
                .is_err()
        );
        assert!(
            inspect_existing_container_lock(
                Some(br#"{"version":2,"requested":{},"features":{}}"#),
                &[request]
            )
            .is_err()
        );
    }

    #[test]
    fn offline_resolution_rejects_a_missing_frozen_record() {
        let request = request("./features/tool");
        let lock = lock_for(&request);
        let resolver = FeatureSourceResolver::new(tempfile::tempdir().expect("cache").path())
            .expect("resolver");
        assert!(
            resolve_frozen_features_offline(&lock, &[request], &[], &resolver, Path::new("."))
                .is_err()
        );
    }

    #[tokio::test]
    async fn local_lock_is_deterministic_and_changes_only_adjacent_target() {
        let checkout = tempfile::tempdir().expect("checkout");
        let config = checkout.path().join(".devcontainer");
        let feature = config.join("features/tool");
        fs::create_dir_all(&feature).expect("directories");
        fs::write(
            config.join("devcontainer.json"),
            br#"{"image":"debian:13","features":{"./features/tool":{"enabled":true}}}"#,
        )
        .expect("config");
        fs::write(feature.join("devcontainer-feature.json"), br#"{"id":"tool","version":"1","options":{"enabled":{"type":"boolean","default":false}}}"#).expect("feature");
        let other_config = checkout.path().join("other/.devcontainer");
        fs::create_dir_all(&other_config).expect("other config");
        let other_lock = other_config.join(FEATURE_LOCK_FILE);
        fs::write(&other_lock, b"unrelated lock").expect("other lock");
        let selected = Path::new(".devcontainer/devcontainer.json");
        let target = generate_feature_lock(
            checkout.path(),
            Some(selected),
            &checkout.path().join("cache"),
        )
        .await
        .expect("lock");
        let first = fs::read(&target).expect("lock bytes");
        assert_eq!(
            fs::read(&other_lock).expect("other lock"),
            b"unrelated lock"
        );
        let lock = FeatureLock::parse(&first).expect("lock parse");
        let mut root = request("./features/tool");
        root.options
            .insert("enabled".to_owned(), FeatureValue::Boolean(true));
        let requests = vec![root];
        let offline =
            FeatureSourceResolver::new(checkout.path().join("offline-cache")).expect("resolver");
        assert_eq!(
            resolve_frozen_features_offline(&lock, &requests, &[], &offline, &config)
                .expect("offline local lock")
                .installation_order
                .len(),
            1
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            fs::set_permissions(&target, fs::Permissions::from_mode(0o640)).expect("mode");
            assert_eq!(
                fs::metadata(&target).expect("metadata").mode() & 0o777,
                0o640
            );
        }
        generate_feature_lock(
            checkout.path(),
            Some(selected),
            &checkout.path().join("cache"),
        )
        .await
        .expect("repeat");
        assert_eq!(first, fs::read(&target).expect("lock bytes"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(
                fs::metadata(target).expect("metadata").mode() & 0o777,
                0o640
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lock_refuses_symlink_target() {
        use std::os::unix::fs::symlink;
        let checkout = tempfile::tempdir().expect("checkout");
        let config = checkout.path().join(".devcontainer");
        fs::create_dir(&config).expect("config dir");
        fs::write(
            config.join("devcontainer.json"),
            br#"{"image":"debian:13"}"#,
        )
        .expect("config");
        let outside = checkout.path().join("outside");
        fs::write(&outside, b"unchanged").expect("outside");
        symlink(&outside, config.join(FEATURE_LOCK_FILE)).expect("symlink");
        assert!(
            generate_feature_lock(
                checkout.path(),
                Some(Path::new(".devcontainer/devcontainer.json")),
                &checkout.path().join("cache")
            )
            .await
            .is_err()
        );
        assert_eq!(fs::read(outside).expect("outside"), b"unchanged");
    }
}
