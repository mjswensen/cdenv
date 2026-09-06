use super::super::super::{FeatureInstallIdentity, FeatureMetadata, ResolvedFeature};
use super::*;
use serde_json::json;

fn verified_resolution() -> (
    Vec<FeatureRequest>,
    ResolvedFeatures,
    BTreeMap<FeatureReference, FeaturePackage>,
) {
    let reference = FeatureReference::parse("ghcr.io/example/tool:1").expect("reference");
    let digest = format!("sha256:{}", "a".repeat(64));
    let package = FeaturePackage {
        reference: reference.clone(),
        identity: FeatureInstallIdentity::oci_digest(digest.clone()).expect("identity"),
        metadata: FeatureMetadata::from_value(&json!({
            "id":"tool", "version":"1",
            "options":{"enabled":{"type":"boolean", "default":true}}
        }))
        .expect("metadata"),
        digest: Some(digest),
        integrity: Some(format!("sha256:{}", "b".repeat(64))),
    };
    let resolved = ResolvedFeatures {
        installation_order: vec![ResolvedFeature {
            reference: reference.clone(),
            identity: package.identity.clone(),
            metadata: package.metadata.clone(),
            options: BTreeMap::from([("enabled".to_owned(), FeatureValue::Boolean(true))]),
        }],
    };
    (
        vec![FeatureRequest::new(reference.clone())],
        resolved,
        BTreeMap::from([(reference, package)]),
    )
}

#[test]
fn encoding_has_canonical_field_order_sorted_maps_and_one_trailing_newline() {
    let lock = FeatureLock {
        requested: BTreeMap::from([
            ("./z".to_owned(), BTreeMap::new()),
            (
                "./a".to_owned(),
                BTreeMap::from([
                    ("z".to_owned(), FeatureValue::Boolean(false)),
                    ("a".to_owned(), FeatureValue::String("value".to_owned())),
                ]),
            ),
        ]),
        ..FeatureLock::default()
    };
    assert_eq!(lock.to_json_bytes().expect("encode"), b"{\n  \"version\": 1,\n  \"requested\": {\n    \"./a\": {\n      \"a\": \"value\",\n      \"z\": false\n    },\n    \"./z\": {}\n  },\n  \"features\": {}\n}\n");
}

#[test]
fn strict_parsing_rejects_malformed_json_and_unknown_fields() {
    for bytes in [
        &b"not JSON"[..],
        br#"{"version":1,"requested":{},"features":{},"unknown":true}"#,
        br#"{"version":1,"requested":{},"features":{"./a":{"unknown":true}}}"#,
        br#"{"version":1,"requested":{},"features":{},}"#,
    ] {
        assert_eq!(
            FeatureLock::parse(bytes),
            Err(FeatureError::InvalidLock {
                message: "lockfile is not valid strict JSON",
            })
        );
    }
}

#[test]
fn parsing_and_encoding_enforce_the_same_structural_validation() {
    let record = LockedFeature::new(
        "1".to_owned(),
        "./a".to_owned(),
        format!("sha256:{}", "a".repeat(64)),
        BTreeMap::new(),
        BTreeMap::new(),
    )
    .expect("record");
    let valid = FeatureLock {
        features: BTreeMap::from([("./a".to_owned(), record)]),
        ..FeatureLock::default()
    };
    for (case, message) in [
        (0, "unsupported lockfile version"),
        (1, "lock record has an empty version or resolution"),
        (2, "lock record has an empty version or resolution"),
        (3, "digest must use SHA-256"),
        (4, "digest must contain 64 lowercase hexadecimal characters"),
    ] {
        let mut lock = valid.clone();
        let record = lock.features.get_mut("./a").expect("record");
        match case {
            0 => lock.version += 1,
            1 => record.version.clear(),
            2 => record.resolved.clear(),
            3 => record.integrity = "sha512:abc".to_owned(),
            _ => record.integrity = format!("sha256:{}", "A".repeat(64)),
        }
        let bytes = serde_json::to_vec(&lock).expect("unchecked JSON");
        assert_eq!(
            FeatureLock::parse(&bytes),
            Err(FeatureError::InvalidLock { message })
        );
        assert_eq!(
            lock.to_json_bytes(),
            Err(FeatureError::InvalidLock { message })
        );
    }
}

#[test]
fn lock_construction_requires_catalog_entries_and_verified_integrity() {
    let (requests, resolved, mut packages) = verified_resolution();
    assert_eq!(
        FeatureLock::from_resolution(&requests, &resolved, &BTreeMap::new()),
        Err(FeatureError::InvalidLock {
            message: "resolved Feature is absent from its package catalog",
        })
    );
    packages.values_mut().next().expect("package").integrity = None;
    assert_eq!(
        FeatureLock::from_resolution(&requests, &resolved, &packages),
        Err(FeatureError::InvalidLock {
            message: "resolved Feature has no content integrity",
        })
    );
}

#[test]
fn frozen_validation_detects_changes_in_each_serialized_resolution_field() {
    let (requests, resolved, packages) = verified_resolution();
    let lock = FeatureLock::from_resolution(&requests, &resolved, &packages).expect("lock");
    assert_eq!(
        lock.validate_frozen(&requests, &resolved, &packages),
        Ok(())
    );
    for case in 0..7 {
        let mut requests = requests.clone();
        let mut resolved = resolved.clone();
        let mut packages = packages.clone();
        let package = packages.values_mut().next().expect("package");
        match case {
            0 => requests.clear(),
            1 => {
                requests[0]
                    .options
                    .insert("enabled".to_owned(), FeatureValue::Boolean(false));
            }
            2 => resolved.installation_order[0].metadata.version = "2".to_owned(),
            3 => {
                resolved.installation_order[0]
                    .options
                    .insert("enabled".to_owned(), FeatureValue::Boolean(false));
            }
            4 => package.digest = Some(format!("sha256:{}", "c".repeat(64))),
            5 => package.integrity = Some(format!("sha256:{}", "c".repeat(64))),
            _ => {
                package.metadata.depends_on.insert(
                    FeatureReference::parse("./base").expect("dependency"),
                    BTreeMap::new(),
                );
            }
        }
        assert_eq!(
            lock.validate_frozen(&requests, &resolved, &packages),
            Err(FeatureError::StaleLock),
            "case {case}"
        );
    }
}

#[test]
fn requested_validation_needs_only_roots_not_a_resolution_or_catalog() {
    let (mut requests, resolved, packages) = verified_resolution();
    let lock = FeatureLock::from_resolution(&requests, &resolved, &packages).expect("lock");
    assert_eq!(lock.validate_requested(&requests), Ok(()));
    requests[0]
        .options
        .insert("enabled".to_owned(), FeatureValue::Boolean(true));
    assert_eq!(
        lock.validate_requested(&requests),
        Err(FeatureError::StaleLock)
    );
    assert_eq!(lock.validate_requested(&[]), Err(FeatureError::StaleLock));
}
