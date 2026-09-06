use super::*;
use serde_json::json;

fn option_metadata() -> FeatureMetadata {
    FeatureMetadata::from_value(&json!({
        "id": "tool", "version": "1",
        "options": {
            "enabled": {"type": "boolean", "default": true},
            "channel": {"type": "string", "default": "stable", "enum": ["stable", "beta"]},
            "custom": {"type": "string", "default": "", "proposals": ["suggested"]}
        }
    }))
    .expect("valid metadata")
}

#[test]
fn effective_options_apply_defaults_without_a_package_catalog() {
    assert_eq!(
        validate_options(&option_metadata(), &BTreeMap::new(), &[]).expect("defaults"),
        BTreeMap::from([
            ("enabled".to_owned(), FeatureValue::Boolean(true)),
            (
                "channel".to_owned(),
                FeatureValue::String("stable".to_owned())
            ),
            ("custom".to_owned(), FeatureValue::String(String::new())),
        ])
    );
}

#[test]
fn proposals_do_not_restrict_custom_option_values() {
    let provided = BTreeMap::from([(
        "custom".to_owned(),
        FeatureValue::String("other".to_owned()),
    )]);
    let effective = validate_options(&option_metadata(), &provided, &[]).expect("custom allowed");
    assert_eq!(effective["custom"], provided["custom"]);
}

#[test]
fn invalid_option_types_and_enums_preserve_the_dependency_path() {
    let path = [
        FeatureReference::parse("./root").expect("root"),
        FeatureReference::parse("./tool").expect("tool"),
    ];
    for (name, value) in [
        ("enabled", FeatureValue::String("true".to_owned())),
        ("channel", FeatureValue::Boolean(true)),
        ("channel", FeatureValue::String("nightly".to_owned())),
    ] {
        let provided = BTreeMap::from([(name.to_owned(), value.clone())]);
        assert_eq!(
            validate_options(&option_metadata(), &provided, &path),
            Err(FeatureError::InvalidOption {
                feature: "tool".to_owned(),
                option: name.to_owned(),
                value,
                path: "./root -> ./tool".to_owned(),
            })
        );
    }
}

#[test]
fn unknown_options_return_the_typed_error_before_applying_defaults() {
    assert_eq!(
        validate_options(
            &option_metadata(),
            &BTreeMap::from([("unknown".to_owned(), FeatureValue::Boolean(false))]),
            &[],
        ),
        Err(FeatureError::UnknownOption {
            feature: "tool".to_owned(),
            option: "unknown".to_owned(),
            path: String::new(),
        })
    );
}

#[test]
fn metadata_errors_retain_exact_property_locations() {
    for (value, path, message) in [
        (
            json!({"id":"tool", "version":"1", "typo":true}),
            "$.typo",
            "unknown Feature metadata property",
        ),
        (
            json!({"id":"tool", "version":"1", "options":{"channel":{"type":"string", "default":"nightly", "enum":["stable"]}}}),
            "$.options[\"channel\"].default",
            "default is not in the option enum",
        ),
        (
            json!({"id":"tool", "version":"1", "dependsOn":{"./base":{"count":2}}}),
            "$.dependsOn[\"./base\"][\"count\"]",
            "option must be boolean or string",
        ),
    ] {
        assert_eq!(
            FeatureMetadata::from_value(&value),
            Err(FeatureError::InvalidMetadata {
                path: path.to_owned(),
                message: message.to_owned(),
            })
        );
    }
}

#[test]
fn dependency_parsing_normalizes_references_without_resolving_them() {
    let metadata = FeatureMetadata::from_value(&json!({
        "id":"tool", "version":"1", "dependsOn":{"./features//base/./":{"enabled":false}}
    }))
    .expect("metadata does not require dependency packages");
    assert_eq!(
        metadata.depends_on,
        BTreeMap::from([(
            FeatureReference::Local("./features/base".to_owned()),
            BTreeMap::from([("enabled".to_owned(), FeatureValue::Boolean(false))]),
        )])
    );
}

#[test]
fn dependency_parsing_preserves_security_reference_errors() {
    let reference = "https://user:secret@example.com/feature.tgz";
    let value = json!({"id":"tool", "version":"1", "dependsOn":{reference:{}}});
    assert_eq!(
        FeatureMetadata::from_value(&value).expect_err("credentials rejected"),
        FeatureReference::parse(reference).expect_err("credentials rejected")
    );
}
