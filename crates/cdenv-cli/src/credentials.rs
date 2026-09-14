//! Private host-owned permission storage, explicit consent, and read-only facts.
//!
//! This is the permission foundation of issue 68, not a live credential broker.
//! Until production lifecycle/supervisor wiring exists, no command here starts a
//! transport or runs a helper. A grant for an active generation fails readiness
//! explicitly instead of claiming to have installed a working integration.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use cdenv_core::credentials::{
    CredentialCapability, CredentialGrants, CredentialPolicyError, HttpsOrigin, SshAgentSelector,
};
use cdenv_core::{InstallationId, WorkspaceName};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    ApplicationError, CdenvRoot, CommandLine, CredentialEnable, CredentialsCommand, Installation,
    LockBehavior, LockGuard, LockMode, ManagedMode, OutputFormat, StateTimestamp, WorkspaceState,
    atomic_write, ensure_lock_file, ensure_private_directory, load_workspace_state,
    render_application_result, render_json_success,
};

/// Schema for permission records; workspace/agent schemas are not reinterpreted.
pub const CREDENTIAL_PERMISSION_SCHEMA: u32 = 1;
const MAX_PERMISSION_BYTES: u64 = 256 * 1024;

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PermissionRecord {
    schema_version: u32,
    installation: InstallationId,
    installation_root_device: u64,
    installation_root_inode: u64,
    workspace: WorkspaceName,
    revision: u64,
    binding: PermissionBinding,
    grants: CredentialGrants,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum PermissionBinding {
    Staged,
    Bound { receipt: BindingReceipt },
}

// A random receipt belongs to an exact private workspace directory and immutable
// record. Deleting/replacing the workspace does not make its old grant reusable.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindingReceipt {
    schema_version: u32,
    installation: InstallationId,
    workspace: WorkspaceName,
    created_at: StateTimestamp,
    source_digest: String,
    directory_device: u64,
    directory_inode: u64,
    nonce: String,
}

impl BindingReceipt {
    fn matches(
        &self,
        directory: &Path,
        state: &WorkspaceState,
    ) -> Result<bool, CredentialCommandError> {
        let metadata =
            private_metadata(directory, true)?.ok_or(CredentialCommandError::Workspace)?;
        Ok(self.schema_version == CREDENTIAL_PERMISSION_SCHEMA
            && self.installation == *state.installation_id()
            && self.workspace == *state.name()
            && self.created_at == *state.created_at()
            && self.source_digest == source_digest(state)
            && self.directory_device == metadata.dev()
            && self.directory_inode == metadata.ino()
            && self.nonce.len() == 64
            && self
                .nonce
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    }
}

/// Credential command failure with no raw policy/request/result diagnostics.
#[derive(Debug, Error)]
pub enum CredentialCommandError {
    /// Explicit origin/capability policy is invalid.
    #[error(transparent)]
    Policy(#[from] CredentialPolicyError),
    /// Installation storage setup failed.
    #[error("cannot prepare credential permission installation")]
    Installation,
    /// A private path, owner, mode, symlink, or hard-link invariant failed.
    #[error("credential permission storage is unsafe; refusing to read or repair it")]
    UnsafeStorage,
    /// Private storage was unreadable or not durably writable.
    #[error("cannot read or durably write private credential permission state")]
    Storage,
    /// A record is oversized, malformed, or from an unsupported schema.
    #[error("invalid or unsupported credential permission schema; no grants were adopted")]
    InvalidRecord,
    /// Another root/workspace identity cannot lend its permission.
    #[error("credential permission belongs to another installation or workspace")]
    Scope,
    /// Current workspace metadata is unsafe, missing, or mismatched.
    #[error("workspace metadata is unavailable or unsafe; credentials were not bound")]
    Workspace,
    /// A bound grant must never transfer to a replacement workspace.
    #[error(
        "credential permission binding does not match this workspace identity; disable the old grants, then explicitly enable new grants"
    )]
    Binding,
    /// An automatic name is never allowed to consume staged permission.
    #[error(
        "staged permission requires explicit create --name; disable it before using an automatically selected name"
    )]
    ExplicitNameRequired,
    /// An active or interrupted mutation owns the workspace/policy lock.
    #[error(
        "credential permission state is locked or its lock is unsafe; retry after the current mutation"
    )]
    Locked,
    /// Revision exhaustion fails rather than wrapping and reusing old authority.
    #[error("credential permission revision is exhausted")]
    Revision,
    /// The verified generation has no authenticated credential transport to reconcile.
    #[error(
        "permission saved, but live credential reconciliation is unavailable; run `cdenv up` to enroll or repair this environment, then retry"
    )]
    RuntimeUnavailable,
    /// Disk revocation is not a claim of confirmed live revocation.
    #[error(
        "permission revoked on disk, but live revocation is unconfirmed; the supervisor could not be safely proven stopped; do not assume old clients have lost access"
    )]
    RevocationUnconfirmed,
}

/// Distinguishes an explicit staged grant from an actual workspace binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPermissionState {
    /// No permission has been recorded for this name.
    Absent,
    /// Permission is held for an explicit future create, not an automatic name.
    Staged,
    /// A durable receipt permits retrying an interrupted staged-to-bound write.
    BindingPending,
    /// Permission matches an exact existing workspace receipt.
    Bound,
    /// The recorded workspace identity is gone or has been replaced.
    Stale,
    /// Permission could not be safely inspected. This grants no authority.
    Unavailable,
}

/// Value-free health facts for one independently configured capability.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialCapabilityStatus {
    configured: bool,
    bound: bool,
    active: bool,
    transport: &'static str,
    backend: &'static str,
}

/// Safe facts shared by credentials status, list/status, and doctor.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CredentialStatusReport {
    schema_version: u32,
    workspace: WorkspaceName,
    permission: CredentialPermissionState,
    revision: Option<u64>,
    grants: CredentialGrants,
    capabilities: BTreeMap<CredentialCapability, CredentialCapabilityStatus>,
    transport: &'static str,
    backend: &'static str,
    message: Option<String>,
}

impl CredentialStatusReport {
    /// Borrows the configured independent grants, never credential values.
    #[must_use]
    pub const fn grants(&self) -> &CredentialGrants {
        &self.grants
    }

    /// Returns the current durable monotonic revision, when permission exists.
    #[must_use]
    pub const fn revision(&self) -> Option<u64> {
        self.revision
    }

    /// Returns staged/bound/stale permission state without starting services.
    #[must_use]
    pub const fn permission(&self) -> CredentialPermissionState {
        self.permission
    }

    /// Returns a concise shared human summary of actual implemented facts.
    #[must_use]
    pub fn summary(&self) -> &'static str {
        match self.permission {
            CredentialPermissionState::Unavailable => "permission unavailable",
            CredentialPermissionState::Stale => "stale permission; inactive",
            _ if self.grants.is_empty() => "disabled; inactive",
            CredentialPermissionState::Staged => "staged; inactive",
            CredentialPermissionState::BindingPending => "binding pending; inactive",
            _ if self.transport == "healthy" => "bound; transport healthy; backend untested",
            _ => "bound permission; integration unavailable",
        }
    }

    pub(crate) fn diagnostic_summary(&self) -> String {
        format!(
            "{}: {}; backend uninspected",
            self.workspace,
            self.summary()
        )
    }

    /// Reports an inspection or identity failure, not mere backend uncertainty.
    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        matches!(
            self.permission,
            CredentialPermissionState::Unavailable | CredentialPermissionState::Stale
        ) || self.transport == "unavailable"
    }
}

/// Inspects private staged/bound grants without locks, repair, helpers, or Docker.
///
/// Invalid state is represented as unavailable, never as a default grant.
#[must_use]
pub fn credential_status(root: &CdenvRoot, name: &WorkspaceName) -> CredentialStatusReport {
    match inspect_status(root, name) {
        Ok(report) => report,
        Err(error) => CredentialStatusReport {
            schema_version: CREDENTIAL_PERMISSION_SCHEMA,
            workspace: name.clone(),
            permission: CredentialPermissionState::Unavailable,
            revision: None,
            grants: CredentialGrants::default(),
            capabilities: capability_statuses(
                &CredentialGrants::default(),
                CredentialPermissionState::Unavailable,
                "unavailable",
                "uninspected",
            ),
            transport: "unavailable",
            backend: "uninspected",
            message: Some(error.to_string()),
        },
    }
}

pub(crate) fn credential_reports(
    root: &CdenvRoot,
) -> Result<Vec<CredentialStatusReport>, CredentialCommandError> {
    if private_metadata(root.as_path(), true)?.is_none()
        || private_metadata(&root.credential_permissions_dir(), true)?.is_none()
    {
        return Ok(Vec::new());
    }
    let mut names = Vec::new();
    for entry in fs::read_dir(root.credential_permissions_dir())
        .map_err(|_| CredentialCommandError::Storage)?
    {
        let entry = entry.map_err(|_| CredentialCommandError::Storage)?;
        let file_name = entry.file_name();
        let file_name = file_name
            .to_str()
            .ok_or(CredentialCommandError::InvalidRecord)?;
        if let Some(name) = file_name.strip_suffix(".json") {
            names.push(
                WorkspaceName::parse(name).map_err(|_| CredentialCommandError::InvalidRecord)?,
            );
        }
    }
    names.sort();
    Ok(names
        .iter()
        .map(|name| credential_status(root, name))
        .collect())
}

fn inspect_status(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<CredentialStatusReport, CredentialCommandError> {
    let Some(record) = load_record(root, name)? else {
        return Ok(CredentialStatusReport {
            schema_version: CREDENTIAL_PERMISSION_SCHEMA,
            workspace: name.clone(),
            permission: CredentialPermissionState::Absent,
            revision: None,
            grants: CredentialGrants::default(),
            capabilities: capability_statuses(
                &CredentialGrants::default(),
                CredentialPermissionState::Absent,
                "inactive",
                "disabled",
            ),
            transport: "inactive",
            backend: "disabled",
            message: None,
        });
    };
    let installation = Installation::load_record_read_only(root)
        .map_err(|_| CredentialCommandError::Installation)?;
    validate_scope(&record, installation.installation_id(), name)?;
    let permission = match &record.binding {
        PermissionBinding::Staged => inspect_staged_binding(root, &record)?,
        PermissionBinding::Bound { .. } => {
            match load_existing_workspace(root, name, installation.installation_id())? {
                Some(state) if bound_matches(root, &record, &state)? => {
                    CredentialPermissionState::Bound
                }
                _ => CredentialPermissionState::Stale,
            }
        }
    };
    let (transport, backend, live_message) = inspect_live_facts(root, &record, permission);
    let message = live_message.or_else(|| (!record.grants.is_empty()).then(|| match permission {
        CredentialPermissionState::BindingPending => "A durable receipt matches this workspace, but permission binding was interrupted. Retry an explicit credentials enable for an already granted capability.".to_owned(),
        CredentialPermissionState::Stale => "Permission has no matching workspace binding. Disable the old grants, then explicitly enable new grants; authority will not transfer automatically.".to_owned(),
        CredentialPermissionState::Bound if transport == "healthy" => "Transport was authenticated without retrieving credentials, signing, or testing backend availability. Existing processes may require a new cdenv SSH session when integration was previously absent.".to_owned(),
        _ => "Configured permission is inactive. Run `cdenv up` to enroll or repair environment integration; no helper, login, signing, or identity lookup was performed by status.".to_owned(),
    }));
    let capabilities = capability_statuses(&record.grants, permission, transport, backend);
    Ok(CredentialStatusReport {
        schema_version: CREDENTIAL_PERMISSION_SCHEMA,
        workspace: name.clone(),
        permission,
        revision: Some(record.revision),
        capabilities,
        transport,
        backend,
        grants: record.grants,
        message,
    })
}

fn capability_statuses(
    grants: &CredentialGrants,
    permission: CredentialPermissionState,
    transport: &'static str,
    backend: &'static str,
) -> BTreeMap<CredentialCapability, CredentialCapabilityStatus> {
    [
        CredentialCapability::GitHttps,
        CredentialCapability::SshAgent,
        CredentialCapability::GitIdentity,
    ]
    .into_iter()
    .map(|capability| {
        let configured = grants.enabled(capability);
        (
            capability,
            CredentialCapabilityStatus {
                configured,
                bound: configured && permission == CredentialPermissionState::Bound,
                active: configured && transport == "healthy",
                transport: if configured { transport } else { "inactive" },
                backend: if configured { backend } else { "disabled" },
            },
        )
    })
    .collect()
}

fn inspect_live_facts(
    root: &CdenvRoot,
    record: &PermissionRecord,
    permission: CredentialPermissionState,
) -> (&'static str, &'static str, Option<String>) {
    if record.grants.is_empty() {
        return ("inactive", "disabled", None);
    }
    if permission != CredentialPermissionState::Bound {
        return ("inactive", "uninspected", None);
    }
    let inspected = (|| -> Result<bool, CredentialCommandError> {
        let installation = Installation::load_record_read_only(root)
            .map_err(|_| CredentialCommandError::Installation)?;
        let state =
            load_existing_workspace(root, &record.workspace, installation.installation_id())?
                .ok_or(CredentialCommandError::Workspace)?;
        let Some(active) = state.active() else {
            return Ok(false);
        };
        let paths = root.workspace(&record.workspace);
        if private_metadata(&paths.supervisor_state_file(), false)?.is_none()
            || private_socket_metadata(&paths.supervisor_socket())?.is_none()
        {
            return Err(CredentialCommandError::RuntimeUnavailable);
        }
        let supervisor = crate::load_supervisor_state(&paths.supervisor_state_file())
            .map_err(|_| CredentialCommandError::RuntimeUnavailable)?;
        let host_build = crate::agent_artifacts::AgentArtifactProvider::embedded_identity()
            .map_err(|_| CredentialCommandError::RuntimeUnavailable)?;
        let lease = supervisor
            .credential_lease
            .as_ref()
            .ok_or(CredentialCommandError::RuntimeUnavailable)?;
        let PermissionBinding::Bound { receipt } = &record.binding else {
            return Err(CredentialCommandError::Binding);
        };
        if supervisor.installation != *state.installation_id()
            || supervisor.workspace != *state.name()
            || supervisor.generation != active.generation()
            || supervisor.host_build_id != host_build
            || supervisor.agent_build_id != *active.provisioned().agent_build_id()
            || supervisor.agent_protocol != active.provisioned().protocol_version()
            || lease.container != *active.container_id()
            || lease.workspace_receipt != receipt.nonce
        {
            return Err(CredentialCommandError::RuntimeUnavailable);
        }
        let token = crate::supervisor_control_token(&supervisor).to_owned();
        let claim = crate::SupervisorClaim {
            installation: &supervisor.installation,
            workspace: &supervisor.workspace,
            generation: supervisor.generation,
            host_build_id: &supervisor.host_build_id,
            agent_build_id: &supervisor.agent_build_id,
            agent_protocol: supervisor.agent_protocol,
        };
        let socket = paths.supervisor_socket();
        let status = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| CredentialCommandError::RuntimeUnavailable)?;
                    runtime
                        .block_on(crate::supervisor_credential_status(&socket, &token, &claim))
                        .map_err(|_| CredentialCommandError::RuntimeUnavailable)
                })
                .join()
                .map_err(|_| CredentialCommandError::RuntimeUnavailable)?
        })?;
        Ok(status.1 == Some(record.revision))
    })();
    match inspected {
        Ok(true) => ("healthy", "untested", None),
        Ok(false) => ("inactive", "uninspected", None),
        Err(_) => (
            "unavailable",
            "uninspected",
            Some("Credential transport could not be authenticated at the durable revision. Run `cdenv up` to repair it; no backend operation was attempted.".to_owned()),
        ),
    }
}

fn inspect_staged_binding(
    root: &CdenvRoot,
    record: &PermissionRecord,
) -> Result<CredentialPermissionState, CredentialCommandError> {
    if record.grants.is_empty() {
        return Ok(CredentialPermissionState::Staged);
    }
    let Some(state) = load_existing_workspace(root, &record.workspace, &record.installation)?
    else {
        return Ok(CredentialPermissionState::Staged);
    };
    let paths = root.workspace(state.name());
    match read_private::<BindingReceipt>(&paths.credential_binding_file())? {
        Some(receipt) if receipt.matches(&paths.root(), &state)? => {
            Ok(CredentialPermissionState::BindingPending)
        }
        _ => Ok(CredentialPermissionState::Stale),
    }
}

/// Changes exactly the explicitly consented policy under private host locks.
///
/// Staging starts no checkout, container, helper, or login. Existing active
/// generations return explicit integration-unavailable guidance after saving
/// grants. Revocation never claims success without proving no live supervisor.
///
/// # Errors
///
/// Returns typed scope, storage, validation, locking, readiness, or unconfirmed
/// revocation errors. A readiness/revocation error may follow a durable policy write.
pub fn mutate_credentials(
    root: &CdenvRoot,
    command: &CredentialsCommand,
) -> Result<CredentialStatusReport, CredentialCommandError> {
    if matches!(command, CredentialsCommand::Status { .. }) {
        return inspect_status(root, command.name());
    }
    private_metadata(root.as_path(), true)?;
    let installation =
        Installation::open_or_create(root).map_err(|_| CredentialCommandError::Installation)?;
    prepare_store(root)?;
    let name = command.name();
    // A staged mutation retains the namespace lock until policy durability so a
    // concurrent create cannot appear between the missing check and the write.
    // Existing workspaces use fail-fast acquisition, then release the namespace
    // before waiting for policy. Create uses workspace -> policy in that order.
    let namespace = namespace_guard(root)?;
    let workspace = workspace_guard(root, name)?;
    let _namespace_guard = if workspace.is_some() {
        drop(namespace);
        None
    } else {
        Some(namespace)
    };
    let _workspace_guard = workspace;
    let _policy_guard = policy_guard(root)?;
    let previous = load_record(root, name)?;
    if let Some(record) = &previous {
        validate_scope(record, installation.record().installation_id(), name)?;
    }
    let root_metadata =
        private_metadata(root.as_path(), true)?.ok_or(CredentialCommandError::UnsafeStorage)?;
    let mut record = previous.clone().unwrap_or_else(|| PermissionRecord {
        schema_version: CREDENTIAL_PERMISSION_SCHEMA,
        installation: installation.record().installation_id().clone(),
        installation_root_device: root_metadata.dev(),
        installation_root_inode: root_metadata.ino(),
        workspace: name.clone(),
        revision: 0,
        binding: PermissionBinding::Staged,
        grants: CredentialGrants::default(),
    });
    let revoking = matches!(
        command,
        CredentialsCommand::Disable { .. } | CredentialsCommand::Deny(_)
    );
    if let CredentialsCommand::Disable { capabilities, .. } = command {
        if capabilities.is_empty() {
            record.grants = CredentialGrants::default();
        } else {
            for capability in capabilities {
                record.grants.disable(*capability);
            }
        }
        if record.grants.is_empty() {
            record.binding = PermissionBinding::Staged;
        }
        persist_change(root, &mut record, previous.as_ref())?;
        reconcile_running_generation(root, &record, true)?;
        return inspect_status(root, name);
    }

    let state = load_existing_workspace(root, name, installation.record().installation_id())?;
    validate_mutation_binding(root, &record, state.as_ref(), command)?;
    match command {
        CredentialsCommand::Enable { capability, .. } => {
            enable(&mut record.grants, capability, state.as_ref())?;
        }
        CredentialsCommand::Allow(arguments) => {
            record.grants.adjust_origins(&arguments.origins, true)?;
        }
        CredentialsCommand::Deny(arguments) => {
            record.grants.adjust_origins(&arguments.origins, false)?;
        }
        CredentialsCommand::Disable { .. } | CredentialsCommand::Status { .. } => {
            unreachable!("handled above")
        }
    }
    if let Some(state) = &state {
        if !bound_matches(root, &record, state)? {
            record.binding = PermissionBinding::Bound {
                receipt: ensure_receipt(root, state)?,
            };
        }
    } else if previous.as_ref().is_some_and(|old| old.grants.is_empty()) {
        record.binding = PermissionBinding::Staged;
    }
    persist_change(root, &mut record, previous.as_ref())?;
    if state.as_ref().is_some_and(|state| state.active().is_some()) {
        reconcile_running_generation(root, &record, revoking)?;
    } else if revoking {
        confirm_no_live_supervisor(root, name)?;
    }
    inspect_status(root, name)
}

fn enable(
    grants: &mut CredentialGrants,
    capability: &CredentialEnable,
    state: Option<&WorkspaceState>,
) -> Result<(), CredentialCommandError> {
    match capability {
        CredentialEnable::GitHttps { origins } => {
            if !origins.is_empty() || grants.enabled(CredentialCapability::GitHttps) {
                grants.enable_https(origins)?;
            } else {
                let origin = state
                    .and_then(|state| {
                        HttpsOrigin::from_repository_source(state.repository_source().as_str()).ok()
                    })
                    .ok_or(CredentialPolicyError::OriginsRequired)?;
                grants.enable_https(&[origin])?;
            }
        }
        CredentialEnable::SshAgent { socket } => {
            let selector = socket
                .as_ref()
                .or_else(|| grants.ssh_selector())
                .cloned()
                .unwrap_or_else(SshAgentSelector::automatic);
            grants.enable_ssh_agent(selector);
        }
        CredentialEnable::GitIdentity => grants.enable_identity(),
    }
    Ok(())
}

fn validate_mutation_binding(
    root: &CdenvRoot,
    record: &PermissionRecord,
    state: Option<&WorkspaceState>,
    command: &CredentialsCommand,
) -> Result<(), CredentialCommandError> {
    if record.grants.is_empty() && matches!(command, CredentialsCommand::Enable { .. }) {
        return Ok(());
    }
    match (&record.binding, state) {
        (PermissionBinding::Staged, None) => Ok(()),
        (PermissionBinding::Staged, Some(state)) => {
            let paths = root.workspace(state.name());
            if read_private::<BindingReceipt>(&paths.credential_binding_file())?
                .is_some_and(|receipt| receipt.matches(&paths.root(), state).unwrap_or(false))
            {
                Ok(())
            } else {
                Err(CredentialCommandError::ExplicitNameRequired)
            }
        }
        (PermissionBinding::Bound { .. }, Some(state)) if bound_matches(root, record, state)? => {
            Ok(())
        }
        _ => Err(CredentialCommandError::Binding),
    }
}

fn persist_change(
    root: &CdenvRoot,
    record: &mut PermissionRecord,
    previous: Option<&PermissionRecord>,
) -> Result<(), CredentialCommandError> {
    if previous == Some(record) {
        return Ok(());
    }
    record.revision = record
        .revision
        .checked_add(1)
        .ok_or(CredentialCommandError::Revision)?;
    persist_private(&root.credential_permission_file(&record.workspace), record)
}

/// Binds a staged grant only after a successful clone with an explicit name.
///
/// Called under the create transaction's workspace lock, before any future
/// container lifecycle work. Failed clones leave permission staged. The receipt
/// is written first, making a policy-write failure retryable against that exact
/// workspace; neither write widens authority by itself.
///
/// # Errors
///
/// Returns a safe binding/storage error and preserves the successful checkout.
pub(crate) fn bind_created_workspace(
    root: &CdenvRoot,
    state: &WorkspaceState,
    explicit_name: bool,
) -> Result<(), CredentialCommandError> {
    if load_record(root, state.name())?.is_none() {
        return Ok(());
    }
    let _guard = policy_guard(root)?;
    let Some(mut record) = load_record(root, state.name())? else {
        return Ok(());
    };
    validate_scope(&record, state.installation_id(), state.name())?;
    if record.grants.is_empty() {
        return Ok(());
    }
    if !explicit_name {
        return Err(CredentialCommandError::ExplicitNameRequired);
    }
    if matches!(record.binding, PermissionBinding::Bound { .. }) {
        return if bound_matches(root, &record, state)? {
            Ok(())
        } else {
            Err(CredentialCommandError::Binding)
        };
    }
    let previous = record.clone();
    record.binding = PermissionBinding::Bound {
        receipt: ensure_receipt(root, state)?,
    };
    persist_change(root, &mut record, Some(&previous))
}

/// Builds the exact generation lease used by production readiness.
///
/// This reads only already-bound policy. It never creates, broadens, or repairs a
/// grant and returns `None` when every capability is disabled.
#[doc(hidden)]
pub fn credential_supervisor_lease(
    root: &CdenvRoot,
    state: &WorkspaceState,
    user: cdenv_core::credential_broker::CredentialUserIdentity,
    home: &Path,
    generation: cdenv_core::GenerationId,
) -> Result<Option<crate::SupervisorCredentialLease>, CredentialCommandError> {
    let Some(record) = load_record(root, state.name())? else {
        return Ok(None);
    };
    validate_scope(&record, state.installation_id(), state.name())?;
    if record.grants.is_empty() {
        return Ok(None);
    }
    if !bound_matches(root, &record, state)? || !home.is_absolute() || home == Path::new("/") {
        return Err(CredentialCommandError::Binding);
    }
    let PermissionBinding::Bound { receipt } = &record.binding else {
        return Err(CredentialCommandError::Binding);
    };
    let runtime_directory: PathBuf = home.join(".cdenv/credentials").join(generation.to_string());
    Ok(Some(crate::SupervisorCredentialLease {
        workspace_receipt: receipt.nonce.clone(),
        user,
        grant_revision: record.revision,
        grants: record.grants,
        runtime_directory: runtime_directory.display().to_string(),
    }))
}

fn ensure_receipt(
    root: &CdenvRoot,
    state: &WorkspaceState,
) -> Result<BindingReceipt, CredentialCommandError> {
    let paths = root.workspace(state.name());
    if let Some(receipt) = read_private::<BindingReceipt>(&paths.credential_binding_file())? {
        return if receipt.matches(&paths.root(), state)? {
            Ok(receipt)
        } else {
            Err(CredentialCommandError::Binding)
        };
    }
    let metadata =
        private_metadata(&paths.root(), true)?.ok_or(CredentialCommandError::Workspace)?;
    let mut nonce = [0_u8; 32];
    getrandom::fill(&mut nonce).map_err(|_| CredentialCommandError::Storage)?;
    let receipt = BindingReceipt {
        schema_version: CREDENTIAL_PERMISSION_SCHEMA,
        installation: state.installation_id().clone(),
        workspace: state.name().clone(),
        created_at: state.created_at().clone(),
        source_digest: source_digest(state),
        directory_device: metadata.dev(),
        directory_inode: metadata.ino(),
        nonce: hex::encode(nonce),
    };
    persist_private(&paths.credential_binding_file(), &receipt)?;
    Ok(receipt)
}

fn source_digest(state: &WorkspaceState) -> String {
    hex::encode(Sha256::digest(
        state.repository_source().as_str().as_bytes(),
    ))
}

fn bound_matches(
    root: &CdenvRoot,
    record: &PermissionRecord,
    state: &WorkspaceState,
) -> Result<bool, CredentialCommandError> {
    let PermissionBinding::Bound { receipt } = &record.binding else {
        return Ok(false);
    };
    let paths = root.workspace(state.name());
    Ok(receipt.matches(&paths.root(), state)?
        && read_private::<BindingReceipt>(&paths.credential_binding_file())?.as_ref()
            == Some(receipt))
}

fn load_existing_workspace(
    root: &CdenvRoot,
    name: &WorkspaceName,
    installation: &InstallationId,
) -> Result<Option<WorkspaceState>, CredentialCommandError> {
    let paths = root.workspace(name);
    if private_metadata(&root.workspaces_dir(), true)?.is_none()
        || private_metadata(&paths.root(), true)?.is_none()
    {
        return Ok(None);
    }
    private_metadata(&paths.state_file(), false)?.ok_or(CredentialCommandError::Workspace)?;
    let state =
        load_workspace_state(&paths.state_file()).map_err(|_| CredentialCommandError::Workspace)?;
    if state.needs_migration_persistence()
        || state.state().name() != name
        || state.state().installation_id() != installation
    {
        return Err(CredentialCommandError::Workspace);
    }
    Ok(Some(state.into_state()))
}

fn namespace_guard(root: &CdenvRoot) -> Result<LockGuard, CredentialCommandError> {
    if private_metadata(&root.workspaces_dir(), true)?.is_none() {
        ensure_private_directory(&root.workspaces_dir())
            .map_err(|_| CredentialCommandError::Storage)?;
    }
    let lock = root.workspace_namespace_lock();
    if private_metadata(&lock, false)?.is_none() {
        ensure_lock_file(&lock).map_err(|_| CredentialCommandError::Storage)?;
    }
    LockGuard::acquire(&lock, LockMode::Exclusive, LockBehavior::Wait)
        .map_err(|_| CredentialCommandError::Locked)
}

fn workspace_guard(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<Option<LockGuard>, CredentialCommandError> {
    if private_metadata(&root.workspaces_dir(), true)?.is_none()
        || private_metadata(&root.workspace(name).root(), true)?.is_none()
    {
        return Ok(None);
    }
    let lock = root.workspace(name).lock_file();
    private_metadata(&lock, false)?.ok_or(CredentialCommandError::Locked)?;
    LockGuard::acquire(&lock, LockMode::Exclusive, LockBehavior::FailFast)
        .map(Some)
        .map_err(|_| CredentialCommandError::Locked)
}

fn policy_guard(root: &CdenvRoot) -> Result<LockGuard, CredentialCommandError> {
    private_metadata(&root.credential_permissions_lock(), false)?
        .ok_or(CredentialCommandError::Locked)?;
    LockGuard::acquire(
        &root.credential_permissions_lock(),
        LockMode::Exclusive,
        LockBehavior::Wait,
    )
    .map_err(|_| CredentialCommandError::Locked)
}

fn prepare_store(root: &CdenvRoot) -> Result<(), CredentialCommandError> {
    private_metadata(root.as_path(), true)?.ok_or(CredentialCommandError::UnsafeStorage)?;
    if private_metadata(&root.credential_permissions_dir(), true)?.is_none() {
        ensure_private_directory(&root.credential_permissions_dir())
            .map_err(|_| CredentialCommandError::Storage)?;
    }
    if private_metadata(&root.credential_permissions_lock(), false)?.is_none() {
        ensure_lock_file(&root.credential_permissions_lock())
            .map_err(|_| CredentialCommandError::Storage)?;
    }
    Ok(())
}

fn load_record(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<Option<PermissionRecord>, CredentialCommandError> {
    if private_metadata(root.as_path(), true)?.is_none()
        || private_metadata(&root.credential_permissions_dir(), true)?.is_none()
    {
        return Ok(None);
    }
    let Some(record) = read_private::<PermissionRecord>(&root.credential_permission_file(name))?
    else {
        return Ok(None);
    };
    if record.schema_version != CREDENTIAL_PERMISSION_SCHEMA || record.revision == 0 {
        return Err(CredentialCommandError::InvalidRecord);
    }
    record.grants.validate()?;
    let metadata =
        private_metadata(root.as_path(), true)?.ok_or(CredentialCommandError::UnsafeStorage)?;
    if record.installation_root_device != metadata.dev()
        || record.installation_root_inode != metadata.ino()
    {
        return Err(CredentialCommandError::Scope);
    }
    Ok(Some(record))
}

fn validate_scope(
    record: &PermissionRecord,
    installation: &InstallationId,
    name: &WorkspaceName,
) -> Result<(), CredentialCommandError> {
    if record.installation != *installation || record.workspace != *name {
        return Err(CredentialCommandError::Scope);
    }
    Ok(())
}

fn persist_private(path: &Path, record: &impl Serialize) -> Result<(), CredentialCommandError> {
    private_metadata(path, false)?;
    let bytes =
        serde_json::to_vec_pretty(record).map_err(|_| CredentialCommandError::InvalidRecord)?;
    if bytes.len() as u64 > MAX_PERMISSION_BYTES {
        return Err(CredentialCommandError::InvalidRecord);
    }
    atomic_write(path, &bytes, ManagedMode::PrivateFile)
        .map_err(|_| CredentialCommandError::Storage)
}

fn read_private<T: for<'de> Deserialize<'de>>(
    path: &Path,
) -> Result<Option<T>, CredentialCommandError> {
    let Some(metadata) = private_metadata(path, false)? else {
        return Ok(None);
    };
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .map_err(|_| CredentialCommandError::Storage)?;
    let opened = file
        .metadata()
        .map_err(|_| CredentialCommandError::Storage)?;
    if opened.dev() != metadata.dev() || opened.ino() != metadata.ino() {
        return Err(CredentialCommandError::UnsafeStorage);
    }
    let bytes = read_bounded(file)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| CredentialCommandError::InvalidRecord)
}

fn read_bounded(file: File) -> Result<Vec<u8>, CredentialCommandError> {
    let mut bytes = Vec::new();
    file.take(MAX_PERMISSION_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| CredentialCommandError::Storage)?;
    if bytes.len() as u64 > MAX_PERMISSION_BYTES {
        return Err(CredentialCommandError::InvalidRecord);
    }
    Ok(bytes)
}

fn private_socket_metadata(path: &Path) -> Result<Option<fs::Metadata>, CredentialCommandError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CredentialCommandError::Storage),
    };
    if !metadata.file_type().is_socket()
        || metadata.file_type().is_symlink()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.permissions().mode() & 0o7777 != 0o600
    {
        return Err(CredentialCommandError::UnsafeStorage);
    }
    Ok(Some(metadata))
}

fn private_metadata(
    path: &Path,
    directory: bool,
) -> Result<Option<fs::Metadata>, CredentialCommandError> {
    // Validate ancestors without requiring ownership of / or /tmp. The selected
    // root and every managed descendant are separately owner/mode checked.
    let mut missing_ancestor = false;
    for ancestor in path.ancestors().skip(1) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => missing_ancestor = true,
            _ => return Err(CredentialCommandError::UnsafeStorage),
        }
    }
    if missing_ancestor {
        return Ok(None);
    }
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(CredentialCommandError::Storage),
    };
    let expected_mode = if directory { 0o700 } else { 0o600 };
    if metadata.file_type().is_symlink()
        || metadata.uid() != nix::unistd::geteuid().as_raw()
        || metadata.permissions().mode() & 0o7777 != expected_mode
        || (directory && !metadata.is_dir())
        || (!directory && (!metadata.is_file() || metadata.nlink() != 1))
    {
        return Err(CredentialCommandError::UnsafeStorage);
    }
    Ok(Some(metadata))
}

fn reconcile_running_generation(
    root: &CdenvRoot,
    record: &PermissionRecord,
    revoking: bool,
) -> Result<(), CredentialCommandError> {
    let fail = || {
        if revoking {
            CredentialCommandError::RevocationUnconfirmed
        } else {
            CredentialCommandError::RuntimeUnavailable
        }
    };
    let installation = Installation::load_record_read_only(root).map_err(|_| fail())?;
    let Some(state) =
        load_existing_workspace(root, &record.workspace, installation.installation_id())?
    else {
        return if revoking {
            confirm_no_live_supervisor(root, &record.workspace)
        } else {
            Err(fail())
        };
    };
    let Some(active) = state.active() else {
        return if revoking {
            confirm_no_live_supervisor(root, &record.workspace)
        } else {
            Err(fail())
        };
    };
    let paths = root.workspace(&record.workspace);
    if private_metadata(&paths.supervisor_state_file(), false)?.is_none()
        || private_socket_metadata(&paths.supervisor_socket())?.is_none()
    {
        return if revoking {
            confirm_no_live_supervisor(root, &record.workspace)
        } else {
            Err(fail())
        };
    }
    let supervisor =
        crate::load_supervisor_state(&paths.supervisor_state_file()).map_err(|_| fail())?;
    let receipt =
        read_private::<BindingReceipt>(&paths.credential_binding_file())?.ok_or_else(fail)?;
    if !receipt.matches(&paths.root(), &state)? {
        return Err(fail());
    }
    let host_build =
        crate::agent_artifacts::AgentArtifactProvider::embedded_identity().map_err(|_| fail())?;
    let identity_matches = supervisor.installation == *state.installation_id()
        && supervisor.workspace == *state.name()
        && supervisor.generation == active.generation()
        && supervisor.host_build_id == host_build
        && supervisor.agent_build_id == *active.provisioned().agent_build_id()
        && supervisor.agent_protocol == active.provisioned().protocol_version()
        && supervisor.credential_lease.as_ref().is_none_or(|lease| {
            lease.container == *active.container_id()
                && lease.generation == active.generation()
                && lease.installation == *state.installation_id()
                && lease.workspace == *state.name()
                && lease.workspace_receipt == receipt.nonce
        });
    if !identity_matches {
        return Err(fail());
    }
    let token = crate::supervisor_control_token(&supervisor).to_owned();
    let claim = crate::SupervisorClaim {
        installation: &supervisor.installation,
        workspace: &supervisor.workspace,
        generation: supervisor.generation,
        host_build_id: &supervisor.host_build_id,
        agent_build_id: &supervisor.agent_build_id,
        agent_protocol: supervisor.agent_protocol,
    };
    let socket = paths.supervisor_socket();
    let grants = record.grants.clone();
    let revision = record.revision;
    let result = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .map_err(|_| ())?;
                runtime
                    .block_on(crate::reconcile_supervisor_credentials(
                        &socket, &token, &claim, revision, &grants,
                    ))
                    .map_err(|_| ())
            })
            .join()
            .map_err(|_| ())?
    });
    result.map(|_| ()).map_err(|()| fail())
}

fn confirm_no_live_supervisor(
    root: &CdenvRoot,
    name: &WorkspaceName,
) -> Result<(), CredentialCommandError> {
    let paths = root.workspace(name);
    let verify = || -> Result<(), CredentialCommandError> {
        if private_metadata(&paths.root(), true)?.is_none()
            || private_metadata(&paths.runtime_dir(), true)?.is_none()
        {
            return Ok(());
        }
        let lock = paths.supervisor_lifetime_lock();
        if private_metadata(&lock, false)?.is_none() {
            for path in [paths.supervisor_state_file(), paths.supervisor_socket()] {
                match fs::symlink_metadata(path) {
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    _ => return Err(CredentialCommandError::RevocationUnconfirmed),
                }
            }
            return Ok(());
        }
        let _guard = LockGuard::acquire(&lock, LockMode::Exclusive, LockBehavior::FailFast)
            .map_err(|_| CredentialCommandError::RevocationUnconfirmed)?;
        Ok(())
    };
    verify().map_err(|_| CredentialCommandError::RevocationUnconfirmed)
}

/// Executes/renders the credential group without passing through generic output.
#[must_use]
pub fn render_credentials_application(
    command_line: &CommandLine,
    root: &CdenvRoot,
    stdout: &mut (impl Write + ?Sized),
    stderr: &mut (impl Write + ?Sized),
) -> ExitCode {
    let crate::CliCommand::Credentials(arguments) = command_line.command() else {
        return ExitCode::FAILURE;
    };
    let command = &arguments.command;
    let result = if matches!(command, CredentialsCommand::Status { .. }) {
        inspect_status(root, command.name())
    } else {
        mutate_credentials(root, command)
    };
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            return render_application_result(
                command_line.output_format(),
                Err(ApplicationError::CredentialsFailed {
                    message: error.to_string(),
                }),
                stdout,
                stderr,
            );
        }
    };
    let rendered = if command_line.output_format() == OutputFormat::Json {
        render_json_success(stdout, &crate::SuccessEnvelope::new(&report)).map_err(io::Error::other)
    } else {
        render_human_credentials(stdout, &report, command)
    };
    if rendered.is_err() {
        let _ = writeln!(stderr, "cdenv: cannot write credential permission report");
        ExitCode::FAILURE
    } else if report.is_unavailable() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn render_human_credentials(
    output: &mut (impl Write + ?Sized),
    report: &CredentialStatusReport,
    command: &CredentialsCommand,
) -> io::Result<()> {
    if let CredentialsCommand::Enable { capability, .. } = command {
        writeln!(
            output,
            "{}: {}.",
            capability.capability(),
            capability.capability().authority()
        )?;
    }
    writeln!(
        output,
        "Credentials for {}: {}",
        report.workspace,
        report.summary()
    )?;
    for capability in [
        CredentialCapability::GitHttps,
        CredentialCapability::SshAgent,
        CredentialCapability::GitIdentity,
    ] {
        writeln!(
            output,
            "  {capability}: {}",
            if report.grants.enabled(capability) {
                "enabled"
            } else {
                "disabled"
            }
        )?;
    }
    if let Some(origins) = report.grants.https_origins() {
        for origin in origins {
            writeln!(output, "  allowed origin: {origin}")?;
        }
    }
    if let Some(message) = &report.message {
        writeln!(output, "{message}")?;
    }
    Ok(())
}
