//! Actual-origin authorization and trusted host lookup adapter for the credential broker.

use std::pin::Pin;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use cdenv_core::credential_broker::{BrokerFrameKind, CredentialLeaseIdentity};
use cdenv_core::credential_protocol::GitCredentialRequest;
use cdenv_core::credentials::HttpsOrigin;
use zeroize::Zeroizing;

use crate::{
    BrokerBackendError, BrokerBackendFuture, BrokerByteStream, CancellationToken,
    CredentialBrokerBackend, HostGitCredentialAdapter, HostGitCredentialOutcome,
};

/// Current host authority for one normalized HTTPS origin.
///
/// Implementations are expected to consult live policy. The backend calls this both before host
/// delegation and immediately before releasing a returned credential.
pub trait GitCredentialAuthorizer: Send + Sync + 'static {
    /// Returns whether the exact lease and normalized actual origin are currently granted.
    fn is_origin_authorized(
        &self,
        identity: &CredentialLeaseIdentity,
        origin: &HttpsOrigin,
    ) -> bool;
}

impl<F> GitCredentialAuthorizer for F
where
    F: Fn(&CredentialLeaseIdentity, &HttpsOrigin) -> bool + Send + Sync + 'static,
{
    fn is_origin_authorized(
        &self,
        identity: &CredentialLeaseIdentity,
        origin: &HttpsOrigin,
    ) -> bool {
        self(identity, origin)
    }
}

/// Broker backend that exposes only trusted, uncached host Git credential lookups.
///
/// It has no SSH-agent or identity backend. The parsed request's normalized origin—not the
/// workspace repository URL—is the authorization input, preserving path and username solely as
/// credential lookup context.
pub struct AuthorizedHostGitBackend<A> {
    identity: CredentialLeaseIdentity,
    authorizer: Arc<A>,
    adapter: HostGitCredentialAdapter,
}

impl<A> AuthorizedHostGitBackend<A>
where
    A: GitCredentialAuthorizer,
{
    /// Binds the host adapter and live authorizer to one exact authenticated lease.
    #[must_use]
    pub fn new(
        identity: CredentialLeaseIdentity,
        authorizer: Arc<A>,
        adapter: HostGitCredentialAdapter,
    ) -> Self {
        Self {
            identity,
            authorizer,
            adapter,
        }
    }
}

impl<A> CredentialBrokerBackend for AuthorizedHostGitBackend<A>
where
    A: GitCredentialAuthorizer,
{
    fn is_authorized(
        &self,
        identity: &CredentialLeaseIdentity,
        operation: BrokerFrameKind,
    ) -> bool {
        &self.identity == identity && operation == BrokerFrameKind::CredentialLookup
    }

    fn credential_lookup(&self, body: Vec<u8>) -> BrokerBackendFuture<'_, Vec<u8>> {
        Box::pin(async move {
            let body = Zeroizing::new(body);
            let request = GitCredentialRequest::parse(&body).map_err(|_| BrokerBackendError)?;
            if !self
                .authorizer
                .is_origin_authorized(&self.identity, request.origin())
            {
                return Err(BrokerBackendError);
            }
            let cancellation = CancellationToken::default();
            let outcome = self
                .adapter
                .lookup(&request, unix_time(), &cancellation)
                .await
                .map_err(|_| BrokerBackendError)?;
            let HostGitCredentialOutcome::Available(response) = outcome else {
                return Err(BrokerBackendError);
            };
            if !self
                .authorizer
                .is_origin_authorized(&self.identity, request.origin())
                || !response.is_current(unix_time())
            {
                return Err(BrokerBackendError);
            }
            let mut output = Zeroizing::new(Vec::new());
            response
                .write_private(&mut *output)
                .map_err(|_| BrokerBackendError)?;
            Ok(std::mem::take(&mut *output))
        })
    }

    fn connect_agent(&self) -> BrokerBackendFuture<'_, Pin<Box<dyn BrokerByteStream>>> {
        Box::pin(async { Err(BrokerBackendError) })
    }

    fn identity_metadata(&self) -> BrokerBackendFuture<'_, Vec<u8>> {
        Box::pin(async { Err(BrokerBackendError) })
    }
}

fn unix_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsString;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;

    use cdenv_core::{
        AgentBuildId, ContainerId, GenerationId, InstallationId, ProtocolVersion, WorkspaceName,
    };

    use super::*;
    use crate::{HostGitCredentialContext, HostGitLaunchEnvironment};

    #[derive(Default)]
    struct Environment(BTreeMap<String, OsString>);

    impl HostGitLaunchEnvironment for Environment {
        fn variable(&self, name: &str) -> Option<OsString> {
            self.0.get(name).cloned()
        }
    }

    fn identity() -> CredentialLeaseIdentity {
        CredentialLeaseIdentity {
            installation: InstallationId::parse("install-1").expect("installation"),
            workspace: WorkspaceName::parse("project").expect("workspace"),
            workspace_receipt: "0123456789abcdef0123456789abcdef".to_owned(),
            container: ContainerId::parse(&"a".repeat(64)).expect("container"),
            generation: GenerationId::new(1).expect("generation"),
            user: cdenv_core::credential_broker::CredentialUserIdentity {
                uid: 1000,
                gid: 1000,
            },
            host_build: AgentBuildId::parse("build-1").expect("build"),
            agent_build: AgentBuildId::parse("build-1").expect("build"),
            agent_protocol: ProtocolVersion::new(1).expect("protocol"),
            broker_protocol: 1,
            grant_revision: 2,
        }
    }

    #[tokio::test]
    async fn actual_normalized_origin_is_checked_before_lookup_and_before_release() {
        let temporary = tempfile::tempdir().expect("temporary");
        let neutral = temporary.path().join("neutral");
        fs::create_dir(&neutral).expect("neutral");
        let marker = temporary.path().join("executed");
        let executable = temporary.path().join("git-fixture");
        fs::write(
            &executable,
            format!(
                "#!/bin/sh\ncat >/dev/null\ntouch '{}'\nprintf 'username=alice\\npassword=ROTATED-TOKEN\\n\\n'\n",
                marker.display()
            ),
        )
        .expect("script");
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).expect("mode");
        let environment = Environment(BTreeMap::from([(
            "PATH".to_owned(),
            OsString::from("/usr/bin:/bin"),
        )]));
        let context =
            HostGitCredentialContext::resolve(executable, &neutral, &environment).expect("context");
        let expected = identity();
        let backend = AuthorizedHostGitBackend::new(
            expected,
            Arc::new(|_: &CredentialLeaseIdentity, origin: &HttpsOrigin| {
                origin.as_str() == "https://granted.example:443"
            }),
            HostGitCredentialAdapter::new(context),
        );

        let denied = backend
            .credential_lookup(
                b"protocol=https\nhost=denied.example\npath=repo.git\nusername=alice\n\n".to_vec(),
            )
            .await;
        assert!(denied.is_err());
        assert!(!marker.exists());

        let granted = backend
            .credential_lookup(
                b"protocol=https\nhost=granted.example\npath=second/repo.git\nusername=alice\n\n"
                    .to_vec(),
            )
            .await
            .expect("credential");
        assert_eq!(granted, b"username=alice\npassword=ROTATED-TOKEN\n\n");
        assert!(marker.exists());
    }
}
