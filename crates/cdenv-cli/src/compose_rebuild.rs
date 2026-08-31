//! Non-atomic Docker Compose rebuild orchestration and precise partial-state evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::future::Future;

use cdenv_core::{ContainerId, GenerationId};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ActiveGeneration, CancellationToken};

/// Health evidence for one desired Compose service.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeServiceHealth {
    /// Docker has not reported a health result.
    Unknown,
    /// Docker reports the service healthy, or it has no healthcheck and passed readiness.
    Healthy,
    /// Docker reports the service unhealthy.
    Unhealthy,
}

/// One non-selecting observed Compose service/container fact.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservedComposeService {
    /// Compose service label.
    pub service: String,
    /// Exact full container ID.
    pub container: ContainerId,
    /// Whether Docker reports the container running.
    pub running: bool,
    /// Current health evidence.
    pub health: ComposeServiceHealth,
}

/// Precise state of one desired service after non-atomic recreation.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ComposePartialService {
    /// No container exists for the desired service.
    Missing,
    /// Exactly one container exists.
    Present {
        /// Exact full container ID.
        container: ContainerId,
        /// Whether it is running.
        running: bool,
        /// Current health evidence.
        health: ComposeServiceHealth,
    },
    /// More than one container exists; none is selected.
    Ambiguous {
        /// Every matching full ID in stable order.
        containers: Vec<ContainerId>,
    },
}

/// Persistable, secret-free evidence from an interrupted Compose replacement.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComposePartialState {
    /// Replacement generation whose labels were inspected.
    pub generation: GenerationId,
    /// Desired services in deterministic order.
    pub services: BTreeMap<String, ComposePartialService>,
}

impl ComposePartialState {
    /// Returns true only when every desired service is uniquely running and healthy.
    #[must_use]
    pub fn is_complete_healthy(&self) -> bool {
        self.services.values().all(|service| {
            matches!(
                service,
                ComposePartialService::Present {
                    running: true,
                    health: ComposeServiceHealth::Healthy,
                    ..
                }
            )
        })
    }

    /// Returns the unique desired service/container set, if complete and healthy.
    #[must_use]
    pub fn healthy_managed_set(&self) -> Option<BTreeMap<String, ContainerId>> {
        self.is_complete_healthy().then(|| {
            self.services
                .iter()
                .filter_map(|(service, state)| match state {
                    ComposePartialService::Present { container, .. } => {
                        Some((service.clone(), container.clone()))
                    }
                    ComposePartialService::Missing | ComposePartialService::Ambiguous { .. } => {
                        None
                    }
                })
                .collect()
        })
    }
}

/// Classifies desired services without selecting among duplicate containers.
#[must_use]
pub fn classify_compose_partial_state(
    generation: GenerationId,
    desired_services: &[String],
    observed: &[ObservedComposeService],
) -> ComposePartialState {
    let mut grouped: BTreeMap<&str, Vec<&ObservedComposeService>> = BTreeMap::new();
    for service in observed {
        grouped.entry(&service.service).or_default().push(service);
    }
    let services = desired_services
        .iter()
        .map(|service| {
            let state = match grouped.get(service.as_str()).map(Vec::as_slice) {
                None | Some([]) => ComposePartialService::Missing,
                Some([observed]) => ComposePartialService::Present {
                    container: observed.container.clone(),
                    running: observed.running,
                    health: observed.health,
                },
                Some(matches) => {
                    let mut containers = matches
                        .iter()
                        .map(|observed| observed.container.clone())
                        .collect::<Vec<_>>();
                    containers.sort();
                    ComposePartialService::Ambiguous { containers }
                }
            };
            (service.clone(), state)
        })
        .collect();
    ComposePartialState {
        generation,
        services,
    }
}

/// Explicit phase reached by the intentionally non-atomic Compose replacement.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ComposeRebuildPhase {
    /// Complete Compose, Feature, and UID-derived image preparation.
    BuildFirst,
    /// Force-recreate may have replaced only part of the project.
    Recreation,
    /// The desired set was rediscovered and is awaiting health.
    ManagedSet,
    /// Lifecycle, agent, environment, and SSH readiness is running.
    Readiness,
    /// Forwarding is being transactionally handed off.
    Forwarding,
    /// The completely ready active state is being atomically committed.
    ActiveCommit,
}

/// Durable recovery evidence for an interrupted non-atomic Compose replacement.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComposeRebuildRecovery {
    /// Replacement generation that may now be partial.
    pub generation: GenerationId,
    /// Last phase reached.
    pub phase: ComposeRebuildPhase,
    /// Exact inspected state, absent only when post-failure inspection also failed.
    pub partial: Option<ComposePartialState>,
}

/// Inputs to one build-first Compose replacement.
pub struct ComposeRebuildRequest<'a, P> {
    /// Complete validated desired Compose plan.
    pub desired: &'a P,
    /// Exact desired managed service set, including dependencies.
    pub managed_services: &'a [String],
    /// Primary service within the managed set.
    pub primary_service: &'a str,
    /// Next positive generation.
    pub generation: GenerationId,
    /// Previously committed primary, if any.
    pub previous_primary: Option<&'a ContainerId>,
    /// Pass Compose/Docker no-cache behavior only when true.
    pub no_cache: bool,
}

/// Cleanup operations attempted only after successful replacement readiness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ComposeCleanupKind {
    /// Remove project orphans proven outside the desired managed set.
    Orphans,
    /// Apply bounded unreferenced cdenv-generated image retention.
    GeneratedImages,
}

/// Warning-only cleanup failure after active replacement success.
#[derive(Debug)]
pub struct ComposeCleanupWarning<E> {
    /// Cleanup operation that failed.
    pub kind: ComposeCleanupKind,
    /// Focused adapter source.
    pub source: E,
}

/// Successful fully ready Compose replacement.
#[derive(Debug)]
pub struct ComposeRebuildOutcome<E> {
    /// Complete active-generation record already committed through the runtime boundary.
    pub active: ActiveGeneration,
    /// Complete rediscovered desired service/container set.
    pub managed: BTreeMap<String, ContainerId>,
    /// Warning-only post-success cleanup failures.
    pub cleanup_warnings: Vec<ComposeCleanupWarning<E>>,
}

/// Partial evidence retained even when rediscovery itself failed.
#[derive(Debug)]
pub enum ComposePartialEvidence<E> {
    /// Exact desired-service state was inspected.
    Inspected(ComposePartialState),
    /// Inspection failed; no service was guessed or adopted.
    InspectionFailed(E),
}

/// Compose rebuild invariant failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ComposeRebuildInvariantError {
    /// Desired managed set is empty, duplicated, unsafe, or omits the primary.
    #[error("desired Compose managed service set is invalid")]
    InvalidManagedSet,
    /// The replacement primary reused the previously active container ID.
    #[error("Compose replacement primary container ID did not change")]
    ReusedPrimary,
    /// Complete readiness returned another generation or primary container.
    #[error("Compose readiness returned inconsistent generation or primary identity")]
    InvalidReadiness,
}

/// Non-atomic Compose replacement failure with precise live evidence.
#[derive(Debug, Error)]
pub enum ComposeRebuildError<E: Error + 'static> {
    /// Complete image preparation failed before recreation; old services were untouched.
    #[error("Compose build-first preparation failed: {source}")]
    Build {
        /// Focused adapter source.
        #[source]
        source: E,
    },
    /// Cancellation was observed before recreation.
    #[error("Compose rebuild was cancelled before recreation")]
    CancelledBeforeRecreation,
    /// Cancellation after recreation retains exact partial state where inspection succeeded.
    #[error("Compose rebuild was cancelled during {phase:?}")]
    Cancelled {
        /// Last reached phase.
        phase: ComposeRebuildPhase,
        /// Non-selecting post-cancellation evidence.
        evidence: ComposePartialEvidence<E>,
    },
    /// A non-atomic adapter phase failed and was inspected rather than rolled back.
    #[error("Compose rebuild failed during {phase:?}: {source}")]
    Partial {
        /// Last reached phase.
        phase: ComposeRebuildPhase,
        /// Focused original failure.
        #[source]
        source: E,
        /// Non-selecting live service evidence.
        evidence: ComposePartialEvidence<E>,
    },
    /// Live state or readiness violated a required replacement invariant.
    #[error("Compose rebuild invariant failed during {phase:?}: {source}")]
    Invariant {
        /// Last reached phase.
        phase: ComposeRebuildPhase,
        /// Exact invariant failure.
        #[source]
        source: ComposeRebuildInvariantError,
        /// Non-selecting live service evidence.
        evidence: ComposePartialEvidence<E>,
    },
}

impl<E: Error + 'static> ComposeRebuildError<E> {
    /// Converts a post-recreation failure into durable, secret-free recovery evidence.
    #[must_use]
    pub fn recovery(&self, generation: GenerationId) -> Option<ComposeRebuildRecovery> {
        let (phase, evidence) = match self {
            Self::Cancelled {
                phase, evidence, ..
            }
            | Self::Partial {
                phase, evidence, ..
            }
            | Self::Invariant {
                phase, evidence, ..
            } => {
                if *phase == ComposeRebuildPhase::BuildFirst {
                    return None;
                }
                (*phase, evidence)
            }
            Self::Build { .. } | Self::CancelledBeforeRecreation => return None,
        };
        let partial = match evidence {
            ComposePartialEvidence::Inspected(state) => Some(state.clone()),
            ComposePartialEvidence::InspectionFailed(_) => None,
        };
        Some(ComposeRebuildRecovery {
            generation,
            phase,
            partial,
        })
    }
}

/// Compose rebuild adapter with explicit non-atomic boundaries.
#[doc(hidden)]
pub trait ComposeRebuildRuntime<P>: Send + Sync {
    /// Complete prepared image claim.
    type Build: Send + Sync;
    /// Focused adapter error that does not contain interpolated Compose model bytes.
    type Error: Error + Send + Sync + 'static;

    /// Resolves/builds Compose, Features, and final UID-derived primary image.
    fn build_complete<'a>(
        &'a self,
        desired: &'a P,
        generation: GenerationId,
        no_cache: bool,
    ) -> impl Future<Output = Result<Self::Build, Self::Error>> + Send + 'a;

    /// Force-recreates using explicit project/files/endpoint, `--no-build`, and `--pull never`.
    fn force_recreate<'a>(
        &'a self,
        desired: &'a P,
        build: &'a Self::Build,
        generation: GenerationId,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Rediscovers all matching replacement-generation project services.
    fn inspect_services(
        &self,
        generation: GenerationId,
    ) -> impl Future<Output = Result<Vec<ObservedComposeService>, Self::Error>> + Send;

    /// Runs lifecycle, agent, effective-environment, and stable SSH identity provisioning.
    fn complete_readiness<'a>(
        &'a self,
        desired: &'a P,
        build: &'a Self::Build,
        generation: GenerationId,
        managed: &'a BTreeMap<String, ContainerId>,
    ) -> impl Future<Output = Result<ActiveGeneration, Self::Error>> + Send + 'a;

    /// Transactionally hands declared forwarding to the completely ready generation.
    fn handoff_forwarding<'a>(
        &'a self,
        active: &'a ActiveGeneration,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Atomically commits active state only after complete readiness and forwarding handoff.
    fn commit_active<'a>(
        &'a self,
        active: &'a ActiveGeneration,
        managed: &'a BTreeMap<String, ContainerId>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Removes only exact isolated-project orphans and never removes volumes.
    fn remove_orphans<'a>(
        &'a self,
        desired_managed: &'a BTreeMap<String, ContainerId>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'a;

    /// Applies shared bounded generated-image cleanup without pruning cache/base/tagged images.
    fn cleanup_generated_images(&self) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// Static-dispatch Compose rebuild coordinator.
pub struct ComposeRebuildOrchestrator<R> {
    runtime: R,
}

impl<R> ComposeRebuildOrchestrator<R> {
    /// Constructs an orchestrator from an explicit runtime seam.
    #[must_use]
    pub const fn new(runtime: R) -> Self {
        Self { runtime }
    }
}

impl<R> ComposeRebuildOrchestrator<R> {
    /// Performs build-first non-atomic replacement with one active-state commit boundary.
    ///
    /// Orphan and generated-image cleanup occurs only after health, complete readiness, and
    /// forwarding handoff. Cleanup failure is warning-only.
    ///
    /// # Errors
    ///
    /// Returns pre-recreation build/cancellation errors or post-recreation failures carrying exact
    /// non-selecting partial-state evidence.
    #[expect(
        clippy::too_many_lines,
        reason = "the non-atomic phase order and evidence capture remain auditable"
    )]
    pub async fn rebuild<P>(
        &self,
        request: ComposeRebuildRequest<'_, P>,
        cancellation: &CancellationToken,
    ) -> Result<ComposeRebuildOutcome<R::Error>, ComposeRebuildError<R::Error>>
    where
        R: ComposeRebuildRuntime<P>,
    {
        validate_request(&request)?;
        if cancellation.is_cancelled() {
            return Err(ComposeRebuildError::CancelledBeforeRecreation);
        }
        let build = self
            .runtime
            .build_complete(request.desired, request.generation, request.no_cache)
            .await
            .map_err(|source| ComposeRebuildError::Build { source })?;
        if cancellation.is_cancelled() {
            return Err(ComposeRebuildError::CancelledBeforeRecreation);
        }
        if let Err(source) = self
            .runtime
            .force_recreate(request.desired, &build, request.generation)
            .await
        {
            return Err(self
                .partial_error(ComposeRebuildPhase::Recreation, source, &request)
                .await);
        }
        if cancellation.is_cancelled() {
            return Err(self
                .cancelled(ComposeRebuildPhase::Recreation, &request)
                .await);
        }
        let observed = self
            .runtime
            .inspect_services(request.generation)
            .await
            .map_err(|source| ComposeRebuildError::Partial {
                phase: ComposeRebuildPhase::ManagedSet,
                source,
                evidence: ComposePartialEvidence::Inspected(classify_compose_partial_state(
                    request.generation,
                    request.managed_services,
                    &[],
                )),
            })?;
        let partial =
            classify_compose_partial_state(request.generation, request.managed_services, &observed);
        let Some(managed) = partial.healthy_managed_set() else {
            return Err(ComposeRebuildError::Invariant {
                phase: ComposeRebuildPhase::ManagedSet,
                source: ComposeRebuildInvariantError::InvalidReadiness,
                evidence: ComposePartialEvidence::Inspected(partial),
            });
        };
        let primary = &managed[request.primary_service];
        if request.previous_primary == Some(primary) {
            return Err(ComposeRebuildError::Invariant {
                phase: ComposeRebuildPhase::ManagedSet,
                source: ComposeRebuildInvariantError::ReusedPrimary,
                evidence: ComposePartialEvidence::Inspected(partial),
            });
        }
        if cancellation.is_cancelled() {
            return Err(ComposeRebuildError::Cancelled {
                phase: ComposeRebuildPhase::ManagedSet,
                evidence: ComposePartialEvidence::Inspected(partial),
            });
        }
        let active = self
            .runtime
            .complete_readiness(request.desired, &build, request.generation, &managed)
            .await
            .map_err(|source| ComposeRebuildError::Partial {
                phase: ComposeRebuildPhase::Readiness,
                source,
                evidence: ComposePartialEvidence::Inspected(partial.clone()),
            })?;
        if active.generation() != request.generation || active.container_id() != primary {
            return Err(ComposeRebuildError::Invariant {
                phase: ComposeRebuildPhase::Readiness,
                source: ComposeRebuildInvariantError::InvalidReadiness,
                evidence: ComposePartialEvidence::Inspected(partial),
            });
        }
        if cancellation.is_cancelled() {
            return Err(self
                .cancelled(ComposeRebuildPhase::Readiness, &request)
                .await);
        }
        self.runtime
            .handoff_forwarding(&active)
            .await
            .map_err(|source| ComposeRebuildError::Partial {
                phase: ComposeRebuildPhase::Forwarding,
                source,
                evidence: ComposePartialEvidence::Inspected(partial.clone()),
            })?;
        self.runtime
            .commit_active(&active, &managed)
            .await
            .map_err(|source| ComposeRebuildError::Partial {
                phase: ComposeRebuildPhase::ActiveCommit,
                source,
                evidence: ComposePartialEvidence::Inspected(partial),
            })?;

        let mut cleanup_warnings = Vec::new();
        if let Err(source) = self.runtime.remove_orphans(&managed).await {
            cleanup_warnings.push(ComposeCleanupWarning {
                kind: ComposeCleanupKind::Orphans,
                source,
            });
        }
        if let Err(source) = self.runtime.cleanup_generated_images().await {
            cleanup_warnings.push(ComposeCleanupWarning {
                kind: ComposeCleanupKind::GeneratedImages,
                source,
            });
        }
        Ok(ComposeRebuildOutcome {
            active,
            managed,
            cleanup_warnings,
        })
    }

    async fn partial_error<P>(
        &self,
        phase: ComposeRebuildPhase,
        source: R::Error,
        request: &ComposeRebuildRequest<'_, P>,
    ) -> ComposeRebuildError<R::Error>
    where
        R: ComposeRebuildRuntime<P>,
    {
        ComposeRebuildError::Partial {
            phase,
            source,
            evidence: self.evidence(request).await,
        }
    }

    async fn cancelled<P>(
        &self,
        phase: ComposeRebuildPhase,
        request: &ComposeRebuildRequest<'_, P>,
    ) -> ComposeRebuildError<R::Error>
    where
        R: ComposeRebuildRuntime<P>,
    {
        ComposeRebuildError::Cancelled {
            phase,
            evidence: self.evidence(request).await,
        }
    }

    async fn evidence<P>(
        &self,
        request: &ComposeRebuildRequest<'_, P>,
    ) -> ComposePartialEvidence<R::Error>
    where
        R: ComposeRebuildRuntime<P>,
    {
        match self.runtime.inspect_services(request.generation).await {
            Ok(observed) => ComposePartialEvidence::Inspected(classify_compose_partial_state(
                request.generation,
                request.managed_services,
                &observed,
            )),
            Err(error) => ComposePartialEvidence::InspectionFailed(error),
        }
    }
}

fn validate_request<P, E>(
    request: &ComposeRebuildRequest<'_, P>,
) -> Result<(), ComposeRebuildError<E>>
where
    E: Error + 'static,
{
    let unique = request.managed_services.iter().collect::<BTreeSet<_>>();
    if request.managed_services.is_empty()
        || unique.len() != request.managed_services.len()
        || !request
            .managed_services
            .iter()
            .any(|service| service == request.primary_service)
        || request.managed_services.iter().any(|service| {
            service.is_empty() || service.starts_with('-') || service.chars().any(char::is_control)
        })
    {
        return Err(ComposeRebuildError::Invariant {
            phase: ComposeRebuildPhase::BuildFirst,
            source: ComposeRebuildInvariantError::InvalidManagedSet,
            evidence: ComposePartialEvidence::Inspected(ComposePartialState {
                generation: request.generation,
                services: BTreeMap::new(),
            }),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::{Arc, Mutex};

    use super::*;

    const FIRST: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SECOND: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn id(value: &str) -> ContainerId {
        ContainerId::parse(value).expect("container ID")
    }

    fn active() -> ActiveGeneration {
        crate::decode_workspace_state(
            std::path::Path::new("fixture.json"),
            include_bytes!("../tests/fixtures/state-compose-background.json"),
        )
        .expect("state fixture")
        .state()
        .active()
        .expect("active fixture")
        .clone()
    }

    #[derive(Clone)]
    struct FakeRuntime {
        state: Arc<Mutex<FakeState>>,
    }

    struct FakeState {
        fail: Option<&'static str>,
        observed: Vec<ObservedComposeService>,
        calls: Vec<&'static str>,
        no_cache: Option<bool>,
    }

    impl FakeRuntime {
        fn new(fail: Option<&'static str>, observed: Vec<ObservedComposeService>) -> Self {
            Self {
                state: Arc::new(Mutex::new(FakeState {
                    fail,
                    observed,
                    calls: Vec::new(),
                    no_cache: None,
                })),
            }
        }

        fn calls(&self) -> Vec<&'static str> {
            self.state.lock().expect("fake lock").calls.clone()
        }
    }

    impl ComposeRebuildRuntime<()> for FakeRuntime {
        type Build = ();
        type Error = io::Error;

        async fn build_complete(
            &self,
            _desired: &(),
            _generation: GenerationId,
            no_cache: bool,
        ) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("build");
            state.no_cache = Some(no_cache);
            if state.fail == Some("build") {
                Err(io::Error::other("build failed"))
            } else {
                Ok(())
            }
        }

        async fn force_recreate(
            &self,
            _desired: &(),
            _build: &(),
            _generation: GenerationId,
        ) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("recreate");
            if state.fail == Some("recreate") {
                Err(io::Error::other("recreate failed"))
            } else {
                Ok(())
            }
        }

        async fn inspect_services(
            &self,
            _generation: GenerationId,
        ) -> Result<Vec<ObservedComposeService>, io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("inspect");
            if state.fail == Some("inspect") {
                Err(io::Error::other("inspect failed"))
            } else {
                Ok(state.observed.clone())
            }
        }

        async fn complete_readiness(
            &self,
            _desired: &(),
            _build: &(),
            _generation: GenerationId,
            _managed: &BTreeMap<String, ContainerId>,
        ) -> Result<ActiveGeneration, io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("readiness");
            if state.fail == Some("readiness") {
                Err(io::Error::other("readiness failed"))
            } else {
                Ok(active())
            }
        }

        async fn handoff_forwarding(&self, _active: &ActiveGeneration) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("handoff");
            if state.fail == Some("handoff") {
                Err(io::Error::other("handoff failed"))
            } else {
                Ok(())
            }
        }

        async fn commit_active(
            &self,
            _active: &ActiveGeneration,
            _managed: &BTreeMap<String, ContainerId>,
        ) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("commit");
            if state.fail == Some("commit") {
                Err(io::Error::other("commit failed"))
            } else {
                Ok(())
            }
        }

        async fn remove_orphans(
            &self,
            _desired_managed: &BTreeMap<String, ContainerId>,
        ) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("orphans");
            if state.fail == Some("orphans") {
                Err(io::Error::other("orphan cleanup failed"))
            } else {
                Ok(())
            }
        }

        async fn cleanup_generated_images(&self) -> Result<(), io::Error> {
            let mut state = self.state.lock().expect("fake lock");
            state.calls.push("images");
            if state.fail == Some("images") {
                Err(io::Error::other("image cleanup failed"))
            } else {
                Ok(())
            }
        }
    }

    fn observed_healthy() -> Vec<ObservedComposeService> {
        vec![
            ObservedComposeService {
                service: "app".to_owned(),
                container: id(FIRST),
                running: true,
                health: ComposeServiceHealth::Healthy,
            },
            ObservedComposeService {
                service: "db".to_owned(),
                container: id(SECOND),
                running: true,
                health: ComposeServiceHealth::Healthy,
            },
        ]
    }

    fn request(services: &[String]) -> ComposeRebuildRequest<'_, ()> {
        ComposeRebuildRequest {
            desired: &(),
            managed_services: services,
            primary_service: "app",
            generation: GenerationId::new(3).expect("generation"),
            previous_primary: None,
            no_cache: true,
        }
    }

    #[test]
    fn partial_state_retains_missing_stopped_unhealthy_and_ambiguous_services() {
        let desired = vec!["app".to_owned(), "db".to_owned(), "cache".to_owned()];
        let observed = vec![
            ObservedComposeService {
                service: "app".to_owned(),
                container: id(FIRST),
                running: true,
                health: ComposeServiceHealth::Unhealthy,
            },
            ObservedComposeService {
                service: "db".to_owned(),
                container: id(FIRST),
                running: false,
                health: ComposeServiceHealth::Unknown,
            },
            ObservedComposeService {
                service: "db".to_owned(),
                container: id(SECOND),
                running: true,
                health: ComposeServiceHealth::Healthy,
            },
        ];
        let state = classify_compose_partial_state(
            GenerationId::new(2).expect("generation"),
            &desired,
            &observed,
        );
        assert!(matches!(
            state.services["cache"],
            ComposePartialService::Missing
        ));
        assert!(matches!(
            state.services["db"],
            ComposePartialService::Ambiguous { .. }
        ));
        assert!(!state.is_complete_healthy());
    }

    #[test]
    fn complete_health_returns_the_exact_managed_set() {
        let desired = vec!["app".to_owned(), "db".to_owned()];
        let observed = observed_healthy();
        let state = classify_compose_partial_state(
            GenerationId::new(2).expect("generation"),
            &desired,
            &observed,
        );
        assert_eq!(
            state.healthy_managed_set().expect("healthy")["app"],
            id(FIRST)
        );
    }

    #[test]
    fn interrupted_partial_evidence_round_trips_in_workspace_state() {
        let partial = classify_compose_partial_state(
            GenerationId::new(4).expect("generation"),
            &["app".to_owned()],
            &[],
        );
        let error = ComposeRebuildError::<io::Error>::Cancelled {
            phase: ComposeRebuildPhase::Recreation,
            evidence: ComposePartialEvidence::Inspected(partial),
        };
        let mut state = crate::decode_workspace_state(
            std::path::Path::new("fixture.json"),
            include_bytes!("../tests/fixtures/state-compose-background.json"),
        )
        .expect("state fixture")
        .into_state();
        state.set_interrupted_compose_rebuild(
            error.recovery(GenerationId::new(4).expect("generation")),
        );
        let bytes = serde_json::to_vec(&state).expect("state JSON");
        let decoded = crate::decode_workspace_state(std::path::Path::new("state.json"), &bytes)
            .expect("persisted recovery state");
        assert_eq!(
            decoded
                .state()
                .interrupted_compose_rebuild()
                .expect("recovery")
                .phase,
            ComposeRebuildPhase::Recreation
        );
    }

    #[tokio::test]
    async fn recreation_failure_is_inspected_without_cleanup_or_rollback_claims() {
        let services = vec!["app".to_owned(), "db".to_owned()];
        let runtime = FakeRuntime::new(Some("recreate"), observed_healthy());
        let error = ComposeRebuildOrchestrator::new(runtime.clone())
            .rebuild(request(&services), &CancellationToken::default())
            .await
            .expect_err("recreation should fail");
        assert!(matches!(
            error,
            ComposeRebuildError::Partial {
                phase: ComposeRebuildPhase::Recreation,
                evidence: ComposePartialEvidence::Inspected(_),
                ..
            }
        ));
        assert_eq!(runtime.calls(), ["build", "recreate", "inspect"]);
    }

    #[tokio::test]
    async fn unhealthy_partial_set_never_runs_readiness_or_orphan_cleanup() {
        let services = vec!["app".to_owned(), "db".to_owned()];
        let mut observed = observed_healthy();
        observed[1].health = ComposeServiceHealth::Unhealthy;
        let runtime = FakeRuntime::new(None, observed);
        let error = ComposeRebuildOrchestrator::new(runtime.clone())
            .rebuild(request(&services), &CancellationToken::default())
            .await
            .expect_err("unhealthy set should fail");
        assert!(matches!(
            error,
            ComposeRebuildError::Invariant {
                phase: ComposeRebuildPhase::ManagedSet,
                ..
            }
        ));
        assert_eq!(runtime.calls(), ["build", "recreate", "inspect"]);
    }

    #[tokio::test]
    async fn cleanup_failure_is_warning_only_after_readiness_and_handoff() {
        let services = vec!["app".to_owned(), "db".to_owned()];
        let runtime = FakeRuntime::new(Some("orphans"), observed_healthy());
        let outcome = ComposeRebuildOrchestrator::new(runtime.clone())
            .rebuild(request(&services), &CancellationToken::default())
            .await
            .expect("replacement should succeed");
        assert_eq!(outcome.cleanup_warnings.len(), 1);
        assert_eq!(
            runtime.calls(),
            [
                "build",
                "recreate",
                "inspect",
                "readiness",
                "handoff",
                "commit",
                "orphans",
                "images"
            ]
        );
    }

    #[tokio::test]
    async fn cancellation_before_recreation_leaves_active_services_untouched() {
        let services = vec!["app".to_owned(), "db".to_owned()];
        let runtime = FakeRuntime::new(None, observed_healthy());
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let error = ComposeRebuildOrchestrator::new(runtime.clone())
            .rebuild(request(&services), &cancellation)
            .await
            .expect_err("cancelled rebuild should fail");
        assert!(matches!(
            error,
            ComposeRebuildError::CancelledBeforeRecreation
        ));
        assert!(runtime.calls().is_empty());
    }
}
