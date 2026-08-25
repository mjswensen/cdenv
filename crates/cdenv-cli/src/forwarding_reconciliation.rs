//! Transactional reconciliation of configuration-declared forwarding intent.

use std::error::Error;
use std::future::Future;
use std::net::Ipv4Addr;

use cdenv_core::{
    AgentBuildId, ForwardingEndpoint, ForwardingEndpointAssignment, InstallationId, TcpPort,
};
use cdenv_devcontainer::{ForwardRequest, ForwardTargetHost, PortPlan, PortProtocol};
use thiserror::Error;

use crate::{
    ActiveForwarding, CdenvRoot, DeclaredForward, ForwardProtocol, ForwardingStopOutcome,
    ForwardingSupervisorError, ForwardingSupervisorStop, ManagedPathState, SupervisorClaim,
    inspect_supervisor_runtime, load_supervisor_state, stop_supervisor, supervisor_control_token,
};

/// Lowest non-privileged TCP port on supported Unix hosts.
pub const FIRST_UNPRIVILEGED_PORT: u16 = 1024;

/// A fully validated desired declared-forwarding plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DesiredForwardingPlan {
    requested: Vec<DeclaredForward>,
}

impl DesiredForwardingPlan {
    /// Resolves planned targets to agent-visible host names without binding sockets.
    ///
    /// Container targets become `localhost`; Compose targets retain their validated service name.
    /// Privileged listeners are refused rather than silently elevating.
    ///
    /// # Errors
    ///
    /// Returns a focused privileged-port or duplicate-listener error.
    pub fn from_port_plan(plan: &PortPlan) -> Result<Self, ForwardingPlanError> {
        let mut requested = Vec::with_capacity(plan.forwards.len());
        for forward in &plan.forwards {
            let local = forward.requested_local.get();
            if local < FIRST_UNPRIVILEGED_PORT {
                return Err(ForwardingPlanError::PrivilegedPort { port: local });
            }
            if requested
                .iter()
                .any(|existing: &DeclaredForward| existing.requested().port().get() == local)
            {
                return Err(ForwardingPlanError::DuplicateLocalPort { port: local });
            }
            let target_host = match &forward.target_host {
                ForwardTargetHost::ContainerLoopback => "localhost".to_owned(),
                ForwardTargetHost::ComposeService(service) => service.clone(),
            };
            requested.push(declared_forward(forward, target_host));
        }
        Ok(Self { requested })
    }

    /// Borrows desired forwarding requests in configuration order.
    #[must_use]
    pub fn requested(&self) -> &[DeclaredForward] {
        &self.requested
    }

    /// Returns whether no supervisor is needed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.requested.is_empty()
    }
}

fn declared_forward(forward: &ForwardRequest, target_host: String) -> DeclaredForward {
    let requested = ForwardingEndpoint::loopback(
        TcpPort::from_u32(u32::from(forward.requested_local.get()))
            .expect("PortNumber is already a validated nonzero u16"),
    );
    let target_port = TcpPort::from_u32(u32::from(forward.target_port.get()))
        .expect("PortNumber is already a validated nonzero u16");
    let protocol = match forward.attributes.protocol {
        None => ForwardProtocol::Tcp,
        Some(PortProtocol::Http) => ForwardProtocol::Http,
        Some(PortProtocol::Https) => ForwardProtocol::Https,
    };
    DeclaredForward::new(
        requested,
        target_host,
        target_port,
        forward.attributes.require_local_port,
        forward.attributes.label.clone(),
        protocol,
    )
}

/// Focused desired-plan validation failure.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ForwardingPlanError {
    /// V1 never silently elevates to acquire a privileged listener.
    #[error(
        "declared local port {port} is privileged; choose a port at least 1024 (cdenv never elevates forwarding automatically)"
    )]
    PrivilegedPort {
        /// Rejected local port.
        port: u16,
    },
    /// One transactional plan cannot own the same local endpoint twice.
    #[error("declared local port {port} is requested more than once")]
    DuplicateLocalPort {
        /// Repeated local port.
        port: u16,
    },
}

/// Request passed to the private runtime adapter for one atomic switch.
pub struct ForwardingRuntimeRequest<'a> {
    /// Complete desired plan.
    pub desired: &'a DesiredForwardingPlan,
    /// Last committed complete plan, if any.
    pub previous: Option<&'a ActiveForwarding>,
    /// Current host/supervisor build identity.
    pub supervisor_build_id: &'a AgentBuildId,
}

/// Result of a supervisor runtime transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ForwardingRuntimeOutcome {
    /// Existing authenticated supervisor already owns this exact plan.
    Current {
        /// Authoritative live assignments.
        assigned: Vec<ForwardingEndpointAssignment>,
    },
    /// A missing/crashed/reboot-lost supervisor was started.
    Started {
        /// Authoritative live assignments.
        assigned: Vec<ForwardingEndpointAssignment>,
    },
    /// A complete changed plan replaced the prior plan atomically.
    Replaced {
        /// Authoritative live assignments.
        assigned: Vec<ForwardingEndpointAssignment>,
    },
    /// At least one required requested port was unavailable.
    ///
    /// Callers commit these diagnostics while leaving the environment running, then report a
    /// nonzero `up` result.
    RequiredPortUnavailable {
        /// Request-ordered assignments; unavailable required listeners have no assignment.
        assigned: Vec<ForwardingEndpointAssignment>,
    },
    /// No forwarding is configured and any authenticated prior supervisor stopped.
    NotConfigured,
}

impl ForwardingRuntimeOutcome {
    fn assignments(&self) -> &[ForwardingEndpointAssignment] {
        match self {
            Self::Current { assigned }
            | Self::Started { assigned }
            | Self::Replaced { assigned }
            | Self::RequiredPortUnavailable { assigned } => assigned,
            Self::NotConfigured => &[],
        }
    }
}

/// Private supervisor adapter with an all-or-nothing replacement contract.
///
/// Implementations must prebind every new/changed listener while the previous complete plan is
/// still active. They may reuse unchanged listeners. On any error they must retain the previous
/// supervisor and all of its listeners.
#[doc(hidden)]
pub trait DeclaredForwardingRuntime: Send + Sync {
    /// Focused control, listener, process, or transport failure.
    type Error: Error + Send + Sync + 'static;

    /// Verifies, starts, restores, or atomically replaces one scoped supervisor.
    fn reconcile<'a>(
        &'a self,
        request: ForwardingRuntimeRequest<'a>,
    ) -> impl Future<Output = Result<ForwardingRuntimeOutcome, Self::Error>> + Send + 'a;
}

/// Successful forwarding reconciliation facts safe for the active-generation commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ForwardingReconciliationOutcome {
    /// Complete persistence-safe desired and assigned state.
    pub forwarding: ActiveForwarding,
    /// Runtime transition used for diagnostics.
    pub runtime: ForwardingRuntimeOutcome,
}

impl ForwardingReconciliationOutcome {
    /// Returns whether `up` must exit nonzero after committing degraded forwarding facts.
    #[must_use]
    pub fn required_port_unavailable(&self) -> bool {
        matches!(
            self.runtime,
            ForwardingRuntimeOutcome::RequiredPortUnavailable { .. }
        )
    }
}

/// Layered plan, runtime, or runtime-contract failure.
#[derive(Debug, Error)]
pub enum ForwardingReconciliationError<R: Error + 'static> {
    /// Desired forwarding plan is invalid before runtime mutation.
    #[error(transparent)]
    Plan(#[from] ForwardingPlanError),
    /// The supervisor could not switch; its prior complete plan remains active.
    #[error("declared forwarding reconciliation failed; previous listeners were retained: {0}")]
    Runtime(#[source] R),
    /// A runtime returned assignments that do not correspond exactly to desired requests.
    #[error("forwarding supervisor returned an invalid or incomplete assignment set")]
    InvalidAssignments,
}

/// Reconciles one desired plan through an all-or-nothing private runtime seam.
///
/// No active state is mutated by this function. Callers commit the returned [`ActiveForwarding`]
/// only with the surrounding successful readiness transaction. Runtime errors therefore preserve
/// both persisted and live prior plans.
///
/// # Errors
///
/// Returns desired-plan, supervisor-runtime, or assignment-contract errors.
pub async fn reconcile_declared_forwarding<R: DeclaredForwardingRuntime>(
    plan: &PortPlan,
    previous: Option<&ActiveForwarding>,
    supervisor_build_id: &AgentBuildId,
    runtime: &R,
) -> Result<ForwardingReconciliationOutcome, ForwardingReconciliationError<R::Error>> {
    let desired = DesiredForwardingPlan::from_port_plan(plan)?;
    let runtime_outcome = runtime
        .reconcile(ForwardingRuntimeRequest {
            desired: &desired,
            previous,
            supervisor_build_id,
        })
        .await
        .map_err(ForwardingReconciliationError::Runtime)?;
    if !valid_assignments(&desired, &runtime_outcome) {
        return Err(ForwardingReconciliationError::InvalidAssignments);
    }
    let build = (!desired.is_empty()).then(|| supervisor_build_id.clone());
    Ok(ForwardingReconciliationOutcome {
        forwarding: ActiveForwarding::new(
            build,
            desired.requested,
            runtime_outcome.assignments().to_vec(),
        ),
        runtime: runtime_outcome,
    })
}

/// Production authenticated `down` adapter for one installation root.
///
/// It never signals a PID. Malformed or mismatched private state is classified as degraded and
/// left alone; only exact installation, workspace, generation, host, and agent identity receives
/// an authenticated control request.
#[derive(Clone, Debug)]
pub struct ScopedForwardingSupervisor {
    root: CdenvRoot,
    installation: InstallationId,
    host_build_id: AgentBuildId,
}

impl ScopedForwardingSupervisor {
    /// Creates a scoped controller from verified installation and host identities.
    #[must_use]
    pub const fn new(
        root: CdenvRoot,
        installation: InstallationId,
        host_build_id: AgentBuildId,
    ) -> Self {
        Self {
            root,
            installation,
            host_build_id,
        }
    }
}

impl ForwardingSupervisorStop for ScopedForwardingSupervisor {
    type Error = ForwardingSupervisorError;

    async fn stop(
        &self,
        workspace: &cdenv_core::WorkspaceName,
        active: Option<&crate::ActiveGeneration>,
    ) -> Result<ForwardingStopOutcome, Self::Error> {
        let Some(active) = active else {
            return Ok(ForwardingStopOutcome::Missing);
        };
        let paths = self.root.workspace(workspace);
        let inspection = inspect_supervisor_runtime(paths);
        if matches!(
            inspection,
            Ok(runtime)
                if runtime.directory == ManagedPathState::Missing
                    || runtime.socket == ManagedPathState::Missing
                    || runtime.state_file == ManagedPathState::Missing
                    || runtime.lifetime_lock == ManagedPathState::Missing
        ) {
            return Ok(ForwardingStopOutcome::Missing);
        }
        if inspection.is_err() {
            return Ok(ForwardingStopOutcome::Degraded);
        }
        let Ok(state) = load_supervisor_state(&paths.supervisor_state_file()) else {
            return Ok(ForwardingStopOutcome::Degraded);
        };
        let provisioned = active.provisioned();
        if state.installation != self.installation
            || state.workspace != *workspace
            || state.generation != active.generation()
            || state.host_build_id != self.host_build_id
            || state.agent_build_id != *provisioned.agent_build_id()
            || state.agent_protocol != provisioned.protocol_version()
        {
            return Ok(ForwardingStopOutcome::Degraded);
        }
        let claim = SupervisorClaim {
            installation: &self.installation,
            workspace,
            generation: active.generation(),
            host_build_id: &self.host_build_id,
            agent_build_id: provisioned.agent_build_id(),
            agent_protocol: provisioned.protocol_version(),
        };
        stop_supervisor(
            &paths.supervisor_socket(),
            supervisor_control_token(&state),
            &claim,
        )
        .await?;
        Ok(ForwardingStopOutcome::Stopped)
    }
}

fn valid_assignments(desired: &DesiredForwardingPlan, outcome: &ForwardingRuntimeOutcome) -> bool {
    if desired.is_empty() {
        return matches!(outcome, ForwardingRuntimeOutcome::NotConfigured);
    }
    let assignments = outcome.assignments();
    assignments.len() == desired.requested.len()
        && assignments
            .iter()
            .zip(&desired.requested)
            .all(|(assignment, request)| {
                assignment.requested() == request.requested()
                    && assignment.assigned().map_or_else(
                        || {
                            request.require_local_port()
                                && matches!(
                                    outcome,
                                    ForwardingRuntimeOutcome::RequiredPortUnavailable { .. }
                                )
                        },
                        |assigned| {
                            assigned.address() == Ipv4Addr::LOCALHOST
                                && (assigned.port() == request.requested().port()
                                    || !request.require_local_port())
                        },
                    )
            })
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use cdenv_devcontainer::{
        AutoForwardAction, EffectivePortAttributes, ForwardTargetHost, PortNumber,
    };

    use super::*;

    #[derive(Clone)]
    struct FakeRuntime {
        result: Arc<Mutex<Option<Result<ForwardingRuntimeOutcome, io::Error>>>>,
        observed_previous: Arc<Mutex<Option<ActiveForwarding>>>,
    }

    impl DeclaredForwardingRuntime for FakeRuntime {
        type Error = io::Error;

        async fn reconcile(
            &self,
            request: ForwardingRuntimeRequest<'_>,
        ) -> Result<ForwardingRuntimeOutcome, Self::Error> {
            *self.observed_previous.lock().expect("previous") = request.previous.cloned();
            self.result
                .lock()
                .expect("result")
                .take()
                .expect("one runtime result")
        }
    }

    fn request(local: u16, target: ForwardTargetHost, required: bool) -> ForwardRequest {
        ForwardRequest {
            requested_local: PortNumber::new(local).expect("local"),
            assigned_local: None,
            target_host: target,
            target_port: PortNumber::new(5432).expect("target"),
            attributes: EffectivePortAttributes {
                on_auto_forward: AutoForwardAction::Notify,
                elevate_if_needed: false,
                label: Some("Database".to_owned()),
                require_local_port: required,
                protocol: None,
            },
        }
    }

    fn plan(forwards: Vec<ForwardRequest>) -> PortPlan {
        PortPlan {
            publications: Vec::new(),
            forwards,
            warnings: Vec::new(),
        }
    }

    fn assignment(requested: u16, assigned: u16) -> ForwardingEndpointAssignment {
        let requested = ForwardingEndpoint::loopback(
            TcpPort::from_u32(u32::from(requested)).expect("requested"),
        );
        let assigned =
            ForwardingEndpoint::loopback(TcpPort::from_u32(u32::from(assigned)).expect("assigned"));
        ForwardingEndpointAssignment::new(requested, Some(assigned))
    }

    fn runtime(result: Result<ForwardingRuntimeOutcome, io::Error>) -> FakeRuntime {
        FakeRuntime {
            result: Arc::new(Mutex::new(Some(result))),
            observed_previous: Arc::new(Mutex::new(None)),
        }
    }

    #[test]
    fn plan_resolves_primary_and_compose_targets() {
        let plan = plan(vec![
            request(3000, ForwardTargetHost::ContainerLoopback, false),
            request(
                5432,
                ForwardTargetHost::ComposeService("database".to_owned()),
                true,
            ),
        ]);
        let desired = DesiredForwardingPlan::from_port_plan(&plan).expect("desired");

        assert_eq!(desired.requested()[0].target_host(), "localhost");
        assert_eq!(desired.requested()[1].target_host(), "database");
    }

    #[test]
    fn plan_rejects_privileged_listener_without_elevation() {
        let error = DesiredForwardingPlan::from_port_plan(&plan(vec![request(
            443,
            ForwardTargetHost::ContainerLoopback,
            false,
        )]))
        .expect_err("privileged listener");

        assert_eq!(error, ForwardingPlanError::PrivilegedPort { port: 443 });
    }

    #[tokio::test]
    async fn alternate_assignment_is_committed_for_optional_local_port() {
        let adapter = runtime(Ok(ForwardingRuntimeOutcome::Started {
            assigned: vec![assignment(3000, 49152)],
        }));
        let build = AgentBuildId::parse("build-1").expect("build");

        let outcome = reconcile_declared_forwarding(
            &plan(vec![request(
                3000,
                ForwardTargetHost::ContainerLoopback,
                false,
            )]),
            None,
            &build,
            &adapter,
        )
        .await
        .expect("alternate assignment");

        assert_eq!(outcome.forwarding.assigned(), &[assignment(3000, 49152)]);
    }

    #[tokio::test]
    async fn required_local_port_commits_degraded_facts_for_nonzero_up_result() {
        let requested =
            ForwardingEndpoint::loopback(TcpPort::from_u32(3000).expect("requested port"));
        let adapter = runtime(Ok(ForwardingRuntimeOutcome::RequiredPortUnavailable {
            assigned: vec![ForwardingEndpointAssignment::pending(requested)],
        }));
        let build = AgentBuildId::parse("build-1").expect("build");

        let outcome = reconcile_declared_forwarding(
            &plan(vec![request(
                3000,
                ForwardTargetHost::ContainerLoopback,
                true,
            )]),
            None,
            &build,
            &adapter,
        )
        .await
        .expect("degraded readiness facts");

        assert!(outcome.required_port_unavailable());
    }

    #[tokio::test]
    async fn failed_switch_passes_and_retains_the_previous_complete_plan() {
        let previous = ActiveForwarding::new(
            Some(AgentBuildId::parse("build-1").expect("build")),
            Vec::new(),
            vec![assignment(3000, 3000)],
        );
        let adapter = runtime(Err(io::Error::new(io::ErrorKind::AddrInUse, "occupied")));
        let observed = Arc::clone(&adapter.observed_previous);
        let build = AgentBuildId::parse("build-1").expect("build");

        let result = reconcile_declared_forwarding(
            &plan(vec![request(
                4000,
                ForwardTargetHost::ContainerLoopback,
                true,
            )]),
            Some(&previous),
            &build,
            &adapter,
        )
        .await;

        assert!(matches!(
            result,
            Err(ForwardingReconciliationError::Runtime(_))
        ));
        assert_eq!(*observed.lock().expect("observed"), Some(previous));
    }

    #[tokio::test]
    async fn current_supervisor_reuses_stable_assignment() {
        let adapter = runtime(Ok(ForwardingRuntimeOutcome::Current {
            assigned: vec![assignment(3000, 49152)],
        }));
        let build = AgentBuildId::parse("build-1").expect("build");

        let outcome = reconcile_declared_forwarding(
            &plan(vec![request(
                3000,
                ForwardTargetHost::ContainerLoopback,
                false,
            )]),
            None,
            &build,
            &adapter,
        )
        .await
        .expect("current supervisor");

        assert_eq!(outcome.forwarding.assigned(), &[assignment(3000, 49152)]);
    }

    #[tokio::test]
    async fn crashed_or_reboot_lost_supervisor_restores_previous_assignment() {
        let previous = ActiveForwarding::new(
            Some(AgentBuildId::parse("build-1").expect("build")),
            Vec::new(),
            vec![assignment(3000, 49152)],
        );
        let adapter = runtime(Ok(ForwardingRuntimeOutcome::Started {
            assigned: vec![assignment(3000, 49152)],
        }));
        let build = AgentBuildId::parse("build-1").expect("build");

        let outcome = reconcile_declared_forwarding(
            &plan(vec![request(
                3000,
                ForwardTargetHost::ContainerLoopback,
                false,
            )]),
            Some(&previous),
            &build,
            &adapter,
        )
        .await
        .expect("restored supervisor");

        assert!(matches!(
            outcome.runtime,
            ForwardingRuntimeOutcome::Started { .. }
        ));
    }

    #[tokio::test]
    async fn empty_plan_stops_scoped_runtime_and_commits_not_configured() {
        let adapter = runtime(Ok(ForwardingRuntimeOutcome::NotConfigured));
        let build = AgentBuildId::parse("build-1").expect("build");

        let outcome = reconcile_declared_forwarding(&plan(Vec::new()), None, &build, &adapter)
            .await
            .expect("not configured");

        assert!(outcome.forwarding.supervisor_build_id().is_none());
    }
}
