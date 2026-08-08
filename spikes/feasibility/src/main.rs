mod docker_api;
mod error;
mod oci;
mod profile;
mod supervisor;

use std::collections::BTreeMap;
use std::error::Error;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::error::{Result, SpikeError, read, write};

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("cdenv feasibility spike: {error}");
        let mut source = error.source();
        while let Some(cause) = source {
            eprintln!("  caused by: {cause}");
            source = cause.source();
        }
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let command = arguments
        .next()
        .ok_or_else(|| SpikeError::Invocation("missing subcommand".to_string()))?;
    let rest = arguments.collect::<Vec<_>>();
    match command.as_str() {
        "baseline" if rest.is_empty() => baseline(),
        "profile" => profile_command(&rest),
        "resolve-oci" => resolve_oci_command(&rest).await,
        "download-https" => download_https_command(&rest).await,
        "docker-versions" if rest.is_empty() => docker_versions().await,
        "verify-labels" => verify_labels_command(&rest).await,
        "compose-override" => compose_override_command(&rest),
        "proxy" => proxy_command(&rest).await,
        "forward-supervisor" => supervisor_command(&rest).await,
        "forward-stop" => supervisor_stop_command(&rest).await,
        other => Err(SpikeError::Invocation(format!(
            "unknown subcommand or arguments for `{other}`"
        ))),
    }
}

fn harness_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn repository_root() -> Result<PathBuf> {
    harness_root()
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .ok_or_else(|| SpikeError::Invocation("harness has no repository parent".to_string()))
}

fn schema_path() -> PathBuf {
    harness_root().join("vendor/devcontainers-spec/schemas/devContainer.base.schema.json")
}

fn baseline() -> Result<()> {
    verify_vendored_checksums()?;
    let repository = repository_root()?;
    let mut configurations = vec![repository.join(".devcontainer/devcontainer.json")];
    for family in ["image", "dockerfile-feature", "compose", "metadata-feature"] {
        let root = harness_root().join("fixtures").join(family);
        let behavior = root.join("BEHAVIOR.md");
        if !behavior.is_file() {
            return Err(SpikeError::Profile {
                path: behavior.display().to_string(),
                detail: "fixture family has no behavior statement".to_string(),
            });
        }
        configurations.push(root.join(".devcontainer/devcontainer.json"));
    }
    for configuration in &configurations {
        profile::parse_and_validate(configuration, &schema_path())?;
    }
    let feature_fixtures = [
        "fixtures/dockerfile-feature/.devcontainer/features/base-marker",
        "fixtures/dockerfile-feature/.devcontainer/features/top-marker",
        "fixtures/metadata-feature/.devcontainer/features/metadata-marker",
    ];
    let feature_schema =
        harness_root().join("vendor/devcontainers-spec/schemas/devContainerFeature.schema.json");
    for feature in feature_fixtures {
        let root = harness_root().join(feature);
        validate_json_schema(&root.join("devcontainer-feature.json"), &feature_schema)?;
        let path = root.join("install.sh");
        let mode = std::fs::metadata(&path)
            .map_err(|source| SpikeError::Read {
                path: path.clone(),
                source,
            })?
            .permissions()
            .mode();
        if mode & 0o111 == 0 {
            return Err(SpikeError::Profile {
                path: path.display().to_string(),
                detail: "Feature install entry point is not executable".to_string(),
            });
        }
    }
    let manifest =
        std::fs::read_to_string(harness_root().join("Cargo.toml")).map_err(|source| {
            SpikeError::Read {
                path: harness_root().join("Cargo.toml"),
                source,
            }
        })?;
    if !manifest.lines().any(|line| line.trim() == "[workspace]") {
        return Err(SpikeError::Profile {
            path: "spikes/feasibility/Cargo.toml".to_string(),
            detail: "spike must root its own empty Cargo workspace".to_string(),
        });
    }
    println!(
        "baseline ok: {} checksummed vendor inputs, {} schema-valid configurations, {} schema-valid local Features, 4 fixture families, profile cdenv-devcontainer-v1",
        checksum_entries()?.len(),
        configurations.len(),
        feature_fixtures.len()
    );
    Ok(())
}

fn validate_json_schema(path: &Path, schema_path: &Path) -> Result<()> {
    let value: Value =
        serde_json::from_slice(&read(path)?).map_err(|error| SpikeError::Profile {
            path: path.display().to_string(),
            detail: format!("invalid JSON: {error}"),
        })?;
    validate_value_schema(path, &value, schema_path)
}

fn validate_value_schema(path: &Path, value: &Value, schema_path: &Path) -> Result<()> {
    let schema: Value = serde_json::from_slice(&read(schema_path)?).map_err(|error| {
        SpikeError::SchemaCompile(format!("{}: {error}", schema_path.display()))
    })?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|error| SpikeError::SchemaCompile(error.to_string()))?;
    let diagnostics = validator
        .iter_errors(value)
        .map(|error| format!("{}: {error}", error.instance_path()))
        .collect::<Vec<_>>();
    if diagnostics.is_empty() {
        Ok(())
    } else {
        Err(SpikeError::Schema {
            path: path.to_path_buf(),
            diagnostics: diagnostics.join("\n"),
        })
    }
}

fn checksum_entries() -> Result<Vec<(String, String)>> {
    let checksum_path = harness_root().join("SHA256SUMS");
    let contents =
        String::from_utf8(read(&checksum_path)?).map_err(|error| SpikeError::Profile {
            path: checksum_path.display().to_string(),
            detail: format!("checksum manifest is not UTF-8: {error}"),
        })?;
    contents
        .lines()
        .map(|line| {
            let (digest, path) = line.split_once("  ").ok_or_else(|| SpikeError::Profile {
                path: checksum_path.display().to_string(),
                detail: format!("invalid checksum line `{line}`"),
            })?;
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(SpikeError::Profile {
                    path: checksum_path.display().to_string(),
                    detail: format!("invalid sha256 `{digest}`"),
                });
            }
            Ok((digest.to_string(), path.to_string()))
        })
        .collect()
}

fn verify_vendored_checksums() -> Result<()> {
    for (expected, relative) in checksum_entries()? {
        let path = harness_root().join(&relative);
        let actual = hex::encode(Sha256::digest(read(&path)?));
        if expected != actual {
            return Err(SpikeError::Profile {
                path: relative,
                detail: format!("vendored checksum mismatch: expected {expected}, got {actual}"),
            });
        }
    }
    Ok(())
}

fn profile_command(arguments: &[String]) -> Result<()> {
    let accept = match arguments {
        [] => false,
        [flag] if flag == "--accept" => true,
        _ => {
            return Err(SpikeError::Invocation(
                "profile accepts only optional `--accept`".to_string(),
            ));
        }
    };
    baseline()?;
    let fixtures = harness_root().join("fixtures");
    let container_env = BTreeMap::from([(
        "PATH".to_string(),
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_string(),
    )]);
    for family in ["image", "dockerfile-feature"] {
        let path = fixtures
            .join(family)
            .join(".devcontainer/devcontainer.json");
        let raw = profile::parse_and_validate(&path, &schema_path())?;
        let plan = profile::effective_plan(
            &raw,
            Path::new(&format!("/fixture/{family}")),
            &container_env,
        )?;
        compare_snapshot(
            &format!("{family}-effective-plan.json"),
            &profile::canonical_json(&plan)?,
            accept,
        )?;
    }

    let metadata_root = fixtures.join("metadata-feature");
    let repository = profile::parse_and_validate(
        &metadata_root.join(".devcontainer/devcontainer.json"),
        &schema_path(),
    )?;
    let image_metadata: Value = serde_json::from_slice(&read(
        &metadata_root.join("image-metadata.json"),
    )?)
    .map_err(|error| SpikeError::Profile {
        path: "fixtures/metadata-feature/image-metadata.json".to_string(),
        detail: error.to_string(),
    })?;
    let feature_metadata: Value = serde_json::from_slice(&read(
        &metadata_root.join(".devcontainer/features/metadata-marker/devcontainer-feature.json"),
    )?)
    .map_err(|error| SpikeError::Feature(error.to_string()))?;
    let mut contributions = image_metadata.as_array().cloned().unwrap_or_default();
    contributions.push(feature_metadata);
    let merged = profile::merge_metadata(&contributions, &repository)?;
    compare_snapshot(
        "metadata-merge.json",
        &profile::canonical_json(&merged)?,
        accept,
    )?;

    let dockerfile_config = fixtures.join("dockerfile-feature/.devcontainer/devcontainer.json");
    let dockerfile_raw = profile::parse_and_validate(&dockerfile_config, &schema_path())?;
    let features = dockerfile_raw
        .get("features")
        .and_then(Value::as_object)
        .ok_or_else(|| SpikeError::Feature("Dockerfile fixture has no Features".to_string()))?;
    let override_order = dockerfile_raw
        .get("overrideFeatureInstallOrder")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    let order =
        profile::resolve_local_feature_order(&dockerfile_config, features, &override_order)?;
    let order_record = json!({
        "features": order,
        "digest": profile::feature_order_digest(&order)?
    });
    compare_snapshot(
        "local-feature-order.json",
        &profile::canonical_json(&order_record)?,
        accept,
    )?;
    println!(
        "profile snapshots ok: deterministic image/Dockerfile plans, metadata merge, and local Feature order"
    );
    Ok(())
}

fn compare_snapshot(name: &str, actual: &[u8], accept: bool) -> Result<()> {
    let path = harness_root().join("expected").join(name);
    if accept {
        write(&path, actual)?;
        return Ok(());
    }
    let expected = read(&path)?;
    if expected != actual {
        return Err(SpikeError::Profile {
            path: path.display().to_string(),
            detail:
                "snapshot differs; inspect the plan and run `profile --accept` only after review"
                    .to_string(),
        });
    }
    Ok(())
}

async fn resolve_oci_command(arguments: &[String]) -> Result<()> {
    let [reference, mode] = arguments else {
        return Err(SpikeError::Invocation(
            "resolve-oci <reference> <check|accept>".to_string(),
        ));
    };
    let cache = harness_root().join("target/spike-cache/oci");
    let feature = oci::resolve_public_feature(reference, &cache).await?;
    let lock = oci::lockfile(&feature)?;
    compare_snapshot("public-feature-lock.json", &lock, mode == "accept")?;
    let evidence = OciEvidence {
        source: &feature.source,
        manifest_digest: &feature.manifest_digest,
        blob_digest: &feature.blob_digest,
        size: feature.size,
        version: &feature.version,
        metadata_id: feature
            .metadata
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
    };
    compare_snapshot(
        "public-feature-evidence.json",
        &profile::canonical_json(&evidence)?,
        mode == "accept",
    )?;
    let extraction = harness_root().join("target/spike-runtime/public-feature");
    if extraction.exists() {
        std::fs::remove_dir_all(&extraction).map_err(|source| SpikeError::Write {
            path: extraction.clone(),
            source,
        })?;
    }
    oci::extract_archive(&feature.archive, &extraction)?;
    let metadata = oci::archive_metadata(&feature.archive)?;
    validate_value_schema(
        Path::new("resolved OCI Feature metadata"),
        &metadata,
        &harness_root().join("vendor/devcontainers-spec/schemas/devContainerFeature.schema.json"),
    )?;
    if feature.metadata != Value::Null && metadata.get("id") != feature.metadata.get("id") {
        return Err(SpikeError::Feature(
            "manifest annotation and extracted Feature metadata IDs differ".to_string(),
        ));
    }
    println!(
        "OCI ok: anonymous bearer, manifest {}, blob {}, {} bytes, cache verified",
        feature.manifest_digest, feature.blob_digest, feature.size
    );
    Ok(())
}

#[derive(Serialize)]
struct OciEvidence<'a> {
    source: &'a str,
    #[serde(rename = "manifestDigest")]
    manifest_digest: &'a str,
    #[serde(rename = "blobDigest")]
    blob_digest: &'a str,
    size: usize,
    version: &'a str,
    #[serde(rename = "metadataId")]
    metadata_id: &'a str,
}

async fn download_https_command(arguments: &[String]) -> Result<()> {
    let [url, ca_path, expected_digest] = arguments else {
        return Err(SpikeError::Invocation(
            "download-https <url> <trusted-ca.pem> <sha256:digest>".to_string(),
        ));
    };
    let bytes = oci::download_https_feature(url, Some(Path::new(ca_path))).await?;
    oci::verify_digest(expected_digest, &bytes, "HTTPS Feature")?;
    let metadata = oci::archive_metadata(&bytes)?;
    validate_value_schema(
        Path::new("downloaded HTTPS Feature metadata"),
        &metadata,
        &harness_root().join("vendor/devcontainers-spec/schemas/devContainerFeature.schema.json"),
    )?;
    println!(
        "HTTPS Feature ok: verified TLS, {}, id {}",
        oci::digest(&bytes),
        metadata
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
    );
    Ok(())
}

async fn docker_versions() -> Result<()> {
    let endpoint = docker_api::resolve_endpoint()?;
    let versions = docker_api::versions(&endpoint).await?;
    print!(
        "{}",
        String::from_utf8(profile::canonical_json(&versions)?).map_err(|error| {
            SpikeError::Profile {
                path: "Docker versions".to_string(),
                detail: error.to_string(),
            }
        })?
    );
    Ok(())
}

async fn verify_labels_command(arguments: &[String]) -> Result<()> {
    let [workspace, count] = arguments else {
        return Err(SpikeError::Invocation(
            "verify-labels <workspace> <expected-count>".to_string(),
        ));
    };
    let count = count
        .parse::<usize>()
        .map_err(|error| SpikeError::Invocation(format!("invalid count: {error}")))?;
    let endpoint = docker_api::resolve_endpoint()?;
    let evidence = docker_api::verify_workspace_labels(&endpoint, workspace, count).await?;
    print!(
        "{}",
        String::from_utf8(profile::canonical_json(&evidence)?).map_err(|error| {
            SpikeError::Profile {
                path: "Docker label evidence".to_string(),
                detail: error.to_string(),
            }
        })?
    );
    Ok(())
}

fn compose_override_command(arguments: &[String]) -> Result<()> {
    let [workspace, workspace_source, workspace_target, agent, output] = arguments else {
        return Err(SpikeError::Invocation(
            "compose-override <workspace> <workspace-source> <workspace-target> <agent-binary> <output>"
                .to_string(),
        ));
    };
    for (name, path) in [
        ("workspace source", workspace_source),
        ("workspace target", workspace_target),
        ("agent binary", agent),
    ] {
        if !path.starts_with('/') {
            return Err(SpikeError::Invocation(format!(
                "{name} path must be absolute"
            )));
        }
    }
    let labels = json!({
        "cdenv.installation": "spike-installation",
        "cdenv.workspace": workspace,
        "cdenv.generation": "1",
        "cdenv.profile": "cdenv-devcontainer-v1"
    });
    let override_value = json!({
        "services": {
            "dependency": {"labels": labels.clone()},
            "workspace": {
                "labels": labels,
                "working_dir": workspace_target,
                "volumes": [
                    {"type": "bind", "source": workspace_source, "target": workspace_target},
                    {"type": "bind", "source": agent, "target": "/opt/cdenv-spike/agent", "read_only": true}
                ]
            }
        }
    });
    write(
        Path::new(output),
        &profile::canonical_json(&override_value)?,
    )
}

async fn proxy_command(arguments: &[String]) -> Result<()> {
    let [container, agent_path, host_key, allowed_key, workspace] = arguments else {
        return Err(SpikeError::Invocation(
            "proxy <container> <agent> <host-key> <allowed-key> <workspace>".to_string(),
        ));
    };
    let endpoint = docker_api::resolve_endpoint()?;
    docker_api::proxy_exec(
        &endpoint,
        container,
        vec![
            agent_path.clone(),
            "ssh-server".to_string(),
            host_key.clone(),
            allowed_key.clone(),
            workspace.clone(),
        ],
    )
    .await
}

async fn supervisor_command(arguments: &[String]) -> Result<()> {
    let [
        workspace,
        generation,
        container,
        agent,
        target_host,
        target_port,
        bind,
        control,
        token,
        ready,
    ] = arguments
    else {
        return Err(SpikeError::Invocation(
            "forward-supervisor <workspace> <generation> <container> <agent> <target-host> <target-port> <bind> <control> <token-file> <ready-file>".to_string(),
        ));
    };
    let target_port = target_port
        .parse::<u16>()
        .map_err(|error| SpikeError::Invocation(format!("invalid target port: {error}")))?;
    supervisor::run(supervisor::SupervisorConfig {
        endpoint: docker_api::resolve_endpoint()?,
        workspace: workspace.clone(),
        generation: generation.clone(),
        container: container.clone(),
        agent_path: agent.clone(),
        target_host: target_host.clone(),
        target_port,
        bind: bind.clone(),
        control_socket: PathBuf::from(control),
        token_file: PathBuf::from(token),
        ready_file: PathBuf::from(ready),
    })
    .await
}

async fn supervisor_stop_command(arguments: &[String]) -> Result<()> {
    let [socket, token, workspace, generation] = arguments else {
        return Err(SpikeError::Invocation(
            "forward-stop <control> <token-file> <workspace> <generation>".to_string(),
        ));
    };
    supervisor::stop(Path::new(socket), Path::new(token), workspace, generation).await
}
