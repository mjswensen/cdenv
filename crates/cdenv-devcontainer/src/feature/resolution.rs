//! Dependency expansion, identity merging, and deterministic installation rounds.
//! The catalog is already fetched; this module performs no I/O or lock encoding.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use super::metadata::validate_options;
use super::{
    FeatureError, FeatureInstallIdentity, FeaturePackage, FeatureReference, FeatureRequest,
    FeatureValue, ResolvedFeature, ResolvedFeatures, render_path,
};

#[derive(Clone)]
struct Node {
    package: FeaturePackage,
    options: BTreeMap<String, FeatureValue>,
    explicit_options: BTreeMap<String, FeatureValue>,
    dependencies: BTreeSet<FeatureInstallIdentity>,
    first_path: Vec<FeatureReference>,
    priority: usize,
}

struct Pending {
    reference: FeatureReference,
    options: BTreeMap<String, FeatureValue>,
    parent: Option<FeatureInstallIdentity>,
    path: Vec<FeatureReference>,
}

/// Recursively resolves requests and applies the specification's deterministic round algorithm.
///
/// `packages` is an injected, already-fetched catalog keyed by normalized reference. This function
/// performs no source or filesystem I/O.
///
/// # Errors
///
/// Returns typed errors for missing packages, invalid options, conflicting duplicate requests,
/// invalid override hints, and hard/soft dependency cycles.
pub fn resolve_features(
    requests: &[FeatureRequest],
    packages: &BTreeMap<FeatureReference, FeaturePackage>,
    override_feature_install_order: &[String],
) -> Result<ResolvedFeatures, FeatureError> {
    let mut pending = requests
        .iter()
        .map(|request| Pending {
            reference: request.reference.clone(),
            options: request.options.clone(),
            parent: None,
            path: vec![request.reference.clone()],
        })
        .collect::<VecDeque<_>>();
    let mut nodes: BTreeMap<FeatureInstallIdentity, Node> = BTreeMap::new();

    while let Some(item) = pending.pop_front() {
        let package =
            packages
                .get(&item.reference)
                .ok_or_else(|| FeatureError::MissingPackage {
                    reference: item.reference.clone(),
                    path: render_path(&item.path),
                })?;
        if package.reference != item.reference {
            return Err(FeatureError::CatalogMismatch {
                key: item.reference,
                package: package.reference.clone(),
            });
        }
        let effective = validate_options(&package.metadata, &item.options, &item.path)?;
        if let Some(existing) = nodes.get_mut(&package.identity) {
            if existing.options != effective {
                let option = differing_option(&existing.options, &effective);
                return Err(FeatureError::ConflictingOptions {
                    identity: package.identity.clone(),
                    option,
                    first_path: render_path(&existing.first_path),
                    second_path: render_path(&item.path),
                });
            }
            if let Some(parent) = item.parent
                && let Some(parent_node) = nodes.get_mut(&parent)
            {
                parent_node.dependencies.insert(package.identity.clone());
            }
            continue;
        }
        nodes.insert(
            package.identity.clone(),
            Node {
                package: package.clone(),
                options: effective,
                explicit_options: item.options,
                dependencies: BTreeSet::new(),
                first_path: item.path.clone(),
                priority: 0,
            },
        );
        if let Some(parent) = item.parent
            && let Some(parent_node) = nodes.get_mut(&parent)
        {
            parent_node.dependencies.insert(package.identity.clone());
        }
        for (reference, options) in &package.metadata.depends_on {
            let mut path = item.path.clone();
            path.push(reference.clone());
            pending.push_back(Pending {
                reference: reference.clone(),
                options: options.clone(),
                parent: Some(package.identity.clone()),
                path,
            });
        }
    }

    apply_soft_edges(&mut nodes);
    apply_priorities(&mut nodes, override_feature_install_order)?;
    round_sort(nodes)
}

fn apply_soft_edges(nodes: &mut BTreeMap<FeatureInstallIdentity, Node>) {
    let matches = nodes
        .iter()
        .map(|(identity, node)| {
            (
                node.package.reference.resource_name().to_owned(),
                identity.clone(),
            )
        })
        .collect::<Vec<_>>();
    for node in nodes.values_mut() {
        for hint in &node.package.metadata.installs_after {
            let hint_name = FeatureReference::parse(hint)
                .map_or_else(
                    |_| hint.to_owned(),
                    |reference| reference.resource_name().to_owned(),
                )
                .to_ascii_lowercase();
            node.dependencies.extend(
                matches
                    .iter()
                    .filter(|(name, identity)| {
                        name.eq_ignore_ascii_case(&hint_name) && *identity != node.package.identity
                    })
                    .map(|(_, identity)| identity.clone()),
            );
        }
    }
}

fn apply_priorities(
    nodes: &mut BTreeMap<FeatureInstallIdentity, Node>,
    overrides: &[String],
) -> Result<(), FeatureError> {
    let count = overrides.len();
    for (index, value) in overrides.iter().enumerate() {
        if value.contains('@')
            || value
                .rsplit('/')
                .next()
                .is_some_and(|last| last.contains(':'))
        {
            return Err(FeatureError::InvalidOverride {
                reference: value.clone(),
                message: "override must omit tags, digests, and options",
            });
        }
        let matches = nodes
            .values_mut()
            .filter(|node| {
                node.package
                    .reference
                    .resource_name()
                    .eq_ignore_ascii_case(value)
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Err(FeatureError::InvalidOverride {
                reference: value.clone(),
                message: "override does not match a resolved Feature",
            });
        }
        for node in matches {
            node.priority = count - index;
        }
    }
    Ok(())
}

fn round_sort(
    mut nodes: BTreeMap<FeatureInstallIdentity, Node>,
) -> Result<ResolvedFeatures, FeatureError> {
    let mut installed = BTreeSet::new();
    let mut output = Vec::with_capacity(nodes.len());
    while !nodes.is_empty() {
        let mut ready = nodes
            .iter()
            .filter(|(_, node)| node.dependencies.is_subset(&installed))
            .map(|(identity, _)| identity.clone())
            .collect::<Vec<_>>();
        if ready.is_empty() {
            let cycle = find_cycle(&nodes).unwrap_or_else(|| nodes.keys().cloned().collect());
            return Err(FeatureError::DependencyCycle {
                cycle: cycle
                    .iter()
                    .map(identity_label)
                    .collect::<Vec<_>>()
                    .join(" -> "),
            });
        }
        let max_priority = ready
            .iter()
            .map(|identity| nodes[identity].priority)
            .max()
            .unwrap_or(0);
        ready.retain(|identity| nodes[identity].priority == max_priority);
        ready.sort_by(|left, right| compare_nodes(&nodes[left], &nodes[right]));
        for identity in ready {
            if let Some(node) = nodes.remove(&identity) {
                installed.insert(identity.clone());
                output.push(ResolvedFeature {
                    reference: node.package.reference,
                    identity,
                    metadata: node.package.metadata,
                    options: node.options,
                });
            }
        }
    }
    Ok(ResolvedFeatures {
        installation_order: output,
    })
}

fn compare_nodes(left: &Node, right: &Node) -> Ordering {
    left.package
        .reference
        .resource_name()
        .cmp(right.package.reference.resource_name())
        .then_with(|| {
            left.package
                .reference
                .selector()
                .cmp(right.package.reference.selector())
        })
        .then_with(|| {
            right
                .explicit_options
                .len()
                .cmp(&left.explicit_options.len())
        })
        .then_with(|| left.explicit_options.cmp(&right.explicit_options))
        .then_with(|| left.package.identity.cmp(&right.package.identity))
}

fn find_cycle(
    nodes: &BTreeMap<FeatureInstallIdentity, Node>,
) -> Option<Vec<FeatureInstallIdentity>> {
    fn visit(
        identity: &FeatureInstallIdentity,
        nodes: &BTreeMap<FeatureInstallIdentity, Node>,
        visiting: &mut Vec<FeatureInstallIdentity>,
        done: &mut BTreeSet<FeatureInstallIdentity>,
    ) -> Option<Vec<FeatureInstallIdentity>> {
        if let Some(index) = visiting.iter().position(|item| item == identity) {
            let mut cycle = visiting[index..].to_vec();
            cycle.push(identity.clone());
            return Some(cycle);
        }
        if done.contains(identity) {
            return None;
        }
        visiting.push(identity.clone());
        for dependency in nodes[identity]
            .dependencies
            .iter()
            .filter(|dependency| nodes.contains_key(*dependency))
        {
            if let Some(cycle) = visit(dependency, nodes, visiting, done) {
                return Some(cycle);
            }
        }
        visiting.pop();
        done.insert(identity.clone());
        None
    }
    let mut done = BTreeSet::new();
    for identity in nodes.keys() {
        if let Some(cycle) = visit(identity, nodes, &mut Vec::new(), &mut done) {
            return Some(cycle);
        }
    }
    None
}

fn identity_label(identity: &FeatureInstallIdentity) -> String {
    match identity {
        FeatureInstallIdentity::OciDigest(value)
        | FeatureInstallIdentity::HttpsIntegrity(value)
        | FeatureInstallIdentity::Local(value) => value.clone(),
    }
}
fn differing_option(
    first: &BTreeMap<String, FeatureValue>,
    second: &BTreeMap<String, FeatureValue>,
) -> String {
    first
        .keys()
        .chain(second.keys())
        .find(|key| first.get(*key) != second.get(*key))
        .cloned()
        .unwrap_or_else(|| "<options>".to_owned())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::ids;
    use super::super::test_support::{catalog, package, reference};
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_dependency_reports_the_full_request_path() {
        let packages = catalog(vec![package(
            "root",
            json!({
                "id":"root", "version":"1", "dependsOn":{"./features/missing":{}}
            }),
        )]);
        assert_eq!(
            resolve_features(&[FeatureRequest::new(reference("root"))], &packages, &[]),
            Err(FeatureError::MissingPackage {
                reference: reference("missing"),
                path: "./features/root -> ./features/missing".to_owned(),
            })
        );
    }

    #[test]
    fn retained_soft_edges_report_the_same_closed_cycle_as_hard_edges() {
        let packages = catalog(vec![
            package(
                "a",
                json!({"id":"a", "version":"1", "installsAfter":["./features/b"]}),
            ),
            package(
                "b",
                json!({"id":"b", "version":"1", "installsAfter":["./features/a"]}),
            ),
        ]);
        assert_eq!(
            resolve_features(
                &[
                    FeatureRequest::new(reference("b")),
                    FeatureRequest::new(reference("a"))
                ],
                &packages,
                &[]
            ),
            Err(FeatureError::DependencyCycle {
                cycle: "./features/a -> ./features/b -> ./features/a".to_owned(),
            })
        );
    }

    #[test]
    fn metadata_options_apply_defaults_and_validate_enums() {
        let package = package(
            "tool",
            json!({
                "id": "tool", "version": "1.0.0",
                "options": {
                    "enabled": {"type": "boolean", "default": true},
                    "channel": {"type": "string", "default": "stable", "enum": ["stable", "beta"]}
                }
            }),
        );
        let packages = catalog(vec![package]);
        let result = resolve_features(&[FeatureRequest::new(reference("tool"))], &packages, &[])
            .expect("defaults resolve");
        assert_eq!(
            result.installation_order[0].options,
            BTreeMap::from([
                (
                    "channel".to_owned(),
                    FeatureValue::String("stable".to_owned())
                ),
                ("enabled".to_owned(), FeatureValue::Boolean(true)),
            ])
        );

        let request = FeatureRequest {
            reference: reference("tool"),
            options: BTreeMap::from([(
                "channel".to_owned(),
                FeatureValue::String("nightly".to_owned()),
            )]),
        };
        assert!(matches!(
            resolve_features(&[request], &packages, &[]),
            Err(FeatureError::InvalidOption { option, .. }) if option == "channel"
        ));
    }

    #[test]
    fn recursive_equal_requests_merge_and_conflicts_include_both_paths() {
        let base = package(
            "base",
            json!({
                "id": "base", "version": "1.0.0",
                "options": {"mode": {"type": "string", "default": "same"}}
            }),
        );
        let left = package(
            "left",
            json!({"id": "left", "version": "1", "dependsOn": {"./features/base": {"mode": "same"}}}),
        );
        let right = package(
            "right",
            json!({"id": "right", "version": "1", "dependsOn": {"./features/base": {"mode": "same"}}}),
        );
        let packages = catalog(vec![base, left, right]);
        let requests = [
            FeatureRequest::new(reference("right")),
            FeatureRequest::new(reference("left")),
        ];
        let result = resolve_features(&requests, &packages, &[]).expect("equal requests merge");
        assert_eq!(ids(&result), vec!["base", "left", "right"]);

        let mut conflicting = packages;
        conflicting
            .get_mut(&reference("right"))
            .expect("fixture package")
            .metadata
            .depends_on
            .get_mut(&reference("base"))
            .expect("fixture dependency")
            .insert(
                "mode".to_owned(),
                FeatureValue::String("different".to_owned()),
            );
        let error = resolve_features(&requests, &conflicting, &[]).expect_err("conflict fails");
        assert!(matches!(
            error,
            FeatureError::ConflictingOptions { option, first_path, second_path, .. }
                if option == "mode" && first_path.contains("base") && second_path.contains("base")
        ));
    }

    #[test]
    fn soft_hints_overrides_and_disconnected_nodes_use_deterministic_rounds() {
        let base = package("base", json!({"id": "base", "version": "1"}));
        let after = package(
            "after",
            json!({"id": "after", "version": "1", "installsAfter": ["./features/base"]}),
        );
        let top = package(
            "top",
            json!({"id": "top", "version": "1", "dependsOn": {"./features/base": {}}}),
        );
        let free = package("free", json!({"id": "free", "version": "1"}));
        let packages = catalog(vec![top, free, after, base]);
        let requests = [
            FeatureRequest::new(reference("top")),
            FeatureRequest::new(reference("free")),
            FeatureRequest::new(reference("after")),
        ];
        let result = resolve_features(
            &requests,
            &packages,
            &["./features/free".to_owned(), "./features/top".to_owned()],
        )
        .expect("graph resolves");
        assert_eq!(ids(&result), vec!["free", "base", "top", "after"]);
    }

    #[test]
    fn order_is_independent_of_request_and_catalog_insertion_order() {
        let packages = vec![
            package(
                "c",
                json!({"id": "c", "version": "1", "dependsOn": {"./features/a": {}}}),
            ),
            package("a", json!({"id": "a", "version": "1"})),
            package("b", json!({"id": "b", "version": "1"})),
        ];
        let first_catalog = catalog(packages.clone());
        let second_catalog = catalog(packages.into_iter().rev().collect());
        let first = resolve_features(
            &[
                FeatureRequest::new(reference("c")),
                FeatureRequest::new(reference("b")),
            ],
            &first_catalog,
            &[],
        )
        .expect("first order resolves");
        let second = resolve_features(
            &[
                FeatureRequest::new(reference("b")),
                FeatureRequest::new(reference("c")),
            ],
            &second_catalog,
            &[],
        )
        .expect("second order resolves");
        assert_eq!(ids(&first), ids(&second));
    }

    #[test]
    fn cycle_error_is_actionable_and_each_contribution_occurs_once() {
        let a = package(
            "a",
            json!({"id": "a", "version": "1", "dependsOn": {"./features/b": {}}, "containerEnv": {"A": "a"}}),
        );
        let b = package(
            "b",
            json!({"id": "b", "version": "1", "dependsOn": {"./features/a": {}}, "containerEnv": {"B": "b"}}),
        );
        let packages = catalog(vec![a, b]);
        let error = resolve_features(&[FeatureRequest::new(reference("a"))], &packages, &[])
            .expect_err("cycle fails");
        assert_eq!(
            error,
            FeatureError::DependencyCycle {
                cycle: "./features/a -> ./features/b -> ./features/a".to_owned(),
            }
        );
        assert_eq!(
            error.to_string(),
            "Feature dependency cycle: ./features/a -> ./features/b -> ./features/a; remove a dependsOn/installsAfter edge or change the requested Features"
        );

        let leaf = package(
            "leaf",
            json!({"id": "leaf", "version": "1", "containerEnv": {"ONLY": "once"}}),
        );
        let one = package(
            "one",
            json!({"id": "one", "version": "1", "dependsOn": {"./features/leaf": {}}}),
        );
        let two = package(
            "two",
            json!({"id": "two", "version": "1", "dependsOn": {"./features/leaf": {}}}),
        );
        let packages = catalog(vec![leaf, one, two]);
        let result = resolve_features(
            &[
                FeatureRequest::new(reference("one")),
                FeatureRequest::new(reference("two")),
            ],
            &packages,
            &[],
        )
        .expect("diamond resolves");
        assert_eq!(
            result
                .installation_order
                .iter()
                .filter(|feature| feature
                    .metadata
                    .contributions
                    .container_env
                    .contains_key("ONLY"))
                .count(),
            1
        );
    }
}
