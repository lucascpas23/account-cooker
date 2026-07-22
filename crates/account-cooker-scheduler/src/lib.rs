#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use account_cooker_core::{
    AdapterContext, Agent, ExecutionPolicy, ExecutionState, InstructionSpec, PlannedAction,
    ProtocolAdapter,
};
use account_cooker_store::{Store, StoreError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use thiserror::Error;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::{info, warn};
use uuid::Uuid;

mod rpc;
pub use rpc::LoopbackRpcTransport;

#[derive(Debug, Error)]
pub enum SchedulerError {
    #[error("store error: {0}")]
    Store(#[from] StoreError),
    #[error("adapter is not registered: {0}")]
    AdapterMissing(String),
    #[error("transport error: {0}")]
    Transport(String),
    #[error("worker task failed: {0}")]
    Worker(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    DryRun,
    ExecuteLocal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubmissionOutcome {
    Confirmed { signature: String },
    Unknown { signature: Option<String> },
}

#[async_trait]
pub trait ExecutionTransport: Send + Sync {
    async fn simulate(
        &self,
        action: &PlannedAction,
        instructions: &[InstructionSpec],
    ) -> Result<(), SchedulerError>;
    async fn submit(
        &self,
        action: &PlannedAction,
        instructions: &[InstructionSpec],
    ) -> Result<SubmissionOutcome, SchedulerError>;
    async fn reconcile(&self, signature: &str) -> Result<Option<bool>, SchedulerError>;
}

#[derive(Debug, Default)]
pub struct SimulationOnlyTransport;

#[async_trait]
impl ExecutionTransport for SimulationOnlyTransport {
    async fn simulate(
        &self,
        _: &PlannedAction,
        instructions: &[InstructionSpec],
    ) -> Result<(), SchedulerError> {
        if instructions.is_empty() {
            return Err(SchedulerError::Transport(
                "adapter produced no instructions".into(),
            ));
        }
        Ok(())
    }

    async fn submit(
        &self,
        _: &PlannedAction,
        _: &[InstructionSpec],
    ) -> Result<SubmissionOutcome, SchedulerError> {
        Err(SchedulerError::Transport(
            "simulation-only transport cannot submit".into(),
        ))
    }

    async fn reconcile(&self, _: &str) -> Result<Option<bool>, SchedulerError> {
        Ok(None)
    }
}

#[derive(Default)]
pub struct AdapterRegistry {
    adapters: BTreeMap<String, Arc<dyn ProtocolAdapter>>,
}

impl AdapterRegistry {
    pub fn new(adapters: Vec<Box<dyn ProtocolAdapter>>) -> Self {
        Self {
            adapters: adapters
                .into_iter()
                .map(|adapter| (adapter.id().to_owned(), Arc::from(adapter)))
                .collect(),
        }
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ProtocolAdapter>> {
        self.adapters.get(id).cloned()
    }
}

#[derive(Debug, Default)]
pub struct SchedulerMetrics {
    pub claimed: AtomicU64,
    pub simulated: AtomicU64,
    pub submitted: AtomicU64,
    pub confirmed: AtomicU64,
    pub rejected: AtomicU64,
    pub unknown: AtomicU64,
    pub worker_failures: AtomicU64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SchedulerMetricsSnapshot {
    pub claimed: u64,
    pub simulated: u64,
    pub submitted: u64,
    pub confirmed: u64,
    pub rejected: u64,
    pub unknown: u64,
    pub worker_failures: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize, PartialEq, Eq)]
pub struct ReconciliationSummary {
    pub examined: usize,
    pub confirmed: usize,
    pub failed: usize,
    pub still_unknown: usize,
    pub missing_signature: usize,
}

impl SchedulerMetrics {
    pub fn snapshot(&self) -> SchedulerMetricsSnapshot {
        SchedulerMetricsSnapshot {
            claimed: self.claimed.load(Ordering::Relaxed),
            simulated: self.simulated.load(Ordering::Relaxed),
            submitted: self.submitted.load(Ordering::Relaxed),
            confirmed: self.confirmed.load(Ordering::Relaxed),
            rejected: self.rejected.load(Ordering::Relaxed),
            unknown: self.unknown.load(Ordering::Relaxed),
            worker_failures: self.worker_failures.load(Ordering::Relaxed),
        }
    }
}

pub struct Scheduler {
    store: Store,
    policy: ExecutionPolicy,
    registry: Arc<AdapterRegistry>,
    transport: Arc<dyn ExecutionTransport>,
    workers: usize,
    claim_batch: usize,
    lease_seconds: u64,
    metrics: Arc<SchedulerMetrics>,
}

impl Scheduler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        store: Store,
        policy: ExecutionPolicy,
        registry: AdapterRegistry,
        transport: Arc<dyn ExecutionTransport>,
        workers: usize,
        claim_batch: usize,
        lease_seconds: u64,
    ) -> Self {
        Self {
            store,
            policy,
            registry: Arc::new(registry),
            transport,
            workers: workers.max(1),
            claim_batch: claim_batch.max(1),
            lease_seconds: lease_seconds.max(1),
            metrics: Arc::new(SchedulerMetrics::default()),
        }
    }

    pub fn metrics(&self) -> SchedulerMetricsSnapshot {
        self.metrics.snapshot()
    }

    pub async fn run_cycle(
        &self,
        now: DateTime<Utc>,
        mode: RunMode,
    ) -> Result<SchedulerMetricsSnapshot, SchedulerError> {
        let worker_id = format!("worker-{}", Uuid::new_v4());
        let recovery = self.store.recover_interrupted_actions(now)?;
        if recovery.safe_to_retry > 0 || recovery.requires_reconciliation > 0 {
            info!(
                safe_to_retry = recovery.safe_to_retry,
                requires_reconciliation = recovery.requires_reconciliation,
                "recovered interrupted action checkpoints"
            );
        }
        let actions =
            self.store
                .claim_due(now, self.claim_batch, &worker_id, self.lease_seconds)?;
        self.metrics
            .claimed
            .fetch_add(actions.len() as u64, Ordering::Relaxed);
        let agents = if let Some(action) = actions.first() {
            self.store
                .agents(action.fleet_id)?
                .into_iter()
                .map(|agent| (agent.id, agent))
                .collect::<BTreeMap<_, _>>()
        } else {
            BTreeMap::new()
        };
        let semaphore = Arc::new(Semaphore::new(self.workers));
        let mut tasks = JoinSet::new();
        for action in actions {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| SchedulerError::Worker(e.to_string()))?;
            let store = self.store.clone();
            let policy = self.policy.clone();
            let registry = self.registry.clone();
            let transport = self.transport.clone();
            let metrics = self.metrics.clone();
            let agent = agents.get(&action.agent_id).cloned();
            let counterparty = action.counterparty.and_then(|id| agents.get(&id).cloned());
            tasks.spawn(async move {
                let _permit = permit;
                process_action(
                    store,
                    policy,
                    registry,
                    transport,
                    metrics,
                    action,
                    agent,
                    counterparty.map(|a| a.public_key),
                    mode,
                )
                .await
            });
        }
        while let Some(result) = tasks.join_next().await {
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => {
                    self.metrics.worker_failures.fetch_add(1, Ordering::Relaxed);
                    warn!(error = %error, "bounded worker failed");
                }
                Err(error) => {
                    self.metrics.worker_failures.fetch_add(1, Ordering::Relaxed);
                    warn!(error = %error, "bounded worker panicked or was cancelled");
                }
            }
        }
        Ok(self.metrics())
    }

    /// Resolves ambiguous outcomes by signature status only. No candidate is
    /// ever submitted again from this path.
    pub async fn reconcile_pending(
        &self,
        limit: usize,
    ) -> Result<ReconciliationSummary, SchedulerError> {
        let candidates = self.store.reconciliation_candidates(limit)?;
        let mut summary = ReconciliationSummary {
            examined: candidates.len(),
            ..ReconciliationSummary::default()
        };
        for candidate in candidates {
            let state = candidate.action.state;
            let Some(signature) = candidate.signature else {
                summary.missing_signature += 1;
                if state == ExecutionState::UnknownOutcome {
                    self.store.resolve_reconciliation(
                        candidate.action.id,
                        state,
                        ExecutionState::ReconciliationRequired,
                        "signature missing; manual evidence required; no resubmission",
                    )?;
                }
                continue;
            };
            match self.transport.reconcile(&signature).await? {
                Some(true) => {
                    self.store.resolve_reconciliation(
                        candidate.action.id,
                        state,
                        ExecutionState::Confirmed,
                        "signature confirmed by RPC reconciliation",
                    )?;
                    summary.confirmed += 1;
                }
                Some(false) => {
                    self.store.resolve_reconciliation(
                        candidate.action.id,
                        state,
                        ExecutionState::Exhausted,
                        "signature finalized with transaction error",
                    )?;
                    summary.failed += 1;
                }
                None => {
                    summary.still_unknown += 1;
                    if state == ExecutionState::UnknownOutcome {
                        self.store.resolve_reconciliation(
                            candidate.action.id,
                            state,
                            ExecutionState::ReconciliationRequired,
                            "signature not yet resolved; no resubmission",
                        )?;
                    }
                }
            }
        }
        Ok(summary)
    }
}

#[allow(clippy::too_many_arguments)]
async fn process_action(
    store: Store,
    policy: ExecutionPolicy,
    registry: Arc<AdapterRegistry>,
    transport: Arc<dyn ExecutionTransport>,
    metrics: Arc<SchedulerMetrics>,
    action: PlannedAction,
    agent: Option<Agent>,
    counterparty: Option<String>,
    mode: RunMode,
) -> Result<(), SchedulerError> {
    let Some(adapter) = registry.get(&action.adapter_id) else {
        store.transition_action(
            action.id,
            ExecutionState::Leased,
            ExecutionState::Rejected,
            "adapter missing",
        )?;
        metrics.rejected.fetch_add(1, Ordering::Relaxed);
        return Err(SchedulerError::AdapterMissing(action.adapter_id));
    };
    let signer_public_key = agent
        .as_ref()
        .map(|agent| agent.public_key.clone())
        .unwrap_or_default();
    let modeled_balance = agent
        .as_ref()
        .map(|agent| {
            agent
                .daily_budget_lamports
                .saturating_add(agent.fee_reserve_lamports)
        })
        .unwrap_or(
            policy
                .max_lamports_per_agent_day
                .saturating_add(policy.minimum_fee_reserve),
        );
    let context = AdapterContext {
        mint_public_key: Some(signer_public_key.clone()),
        signer_public_key,
        counterparty_public_key: counterparty,
        available_lamports: modeled_balance,
        available_tokens: u64::MAX / 2,
    };
    let mut preflight = policy.clone();
    preflight.simulation_required = false;
    let program_ids = adapter.required_program_ids();
    let observation = store.policy_observation(
        &action,
        action.adapter_id.clone(),
        program_ids.clone(),
        context.available_lamports,
        false,
    )?;
    if let Err(error) = preflight.validate(&action, &observation) {
        store.transition_action(
            action.id,
            ExecutionState::Leased,
            ExecutionState::Rejected,
            &error.to_string(),
        )?;
        metrics.rejected.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let instructions = match adapter.build_instructions(&action, &context) {
        Ok(instructions) => instructions,
        Err(error) => {
            store.transition_action(
                action.id,
                ExecutionState::Leased,
                ExecutionState::Rejected,
                &error.to_string(),
            )?;
            metrics.rejected.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
    };
    if let Err(error) = transport.simulate(&action, &instructions).await {
        store.transition_action(
            action.id,
            ExecutionState::Leased,
            ExecutionState::FailedBeforeSubmission,
            &error.to_string(),
        )?;
        return Ok(());
    }
    store.transition_action(
        action.id,
        ExecutionState::Leased,
        ExecutionState::Simulated,
        "simulation passed",
    )?;
    metrics.simulated.fetch_add(1, Ordering::Relaxed);
    if mode == RunMode::DryRun {
        return Ok(());
    }
    let post_simulation = store.policy_observation(
        &action,
        action.adapter_id.clone(),
        program_ids,
        context.available_lamports,
        true,
    )?;
    if let Err(error) = policy.validate(&action, &post_simulation) {
        store.transition_action(
            action.id,
            ExecutionState::Simulated,
            ExecutionState::Rejected,
            &error.to_string(),
        )?;
        metrics.rejected.fetch_add(1, Ordering::Relaxed);
        return Ok(());
    }
    let (agent_daily_limit, agent_hourly_limit, agent_daily_action_limit) = agent
        .as_ref()
        .map(|agent| {
            (
                agent.daily_budget_lamports,
                agent.actions_per_hour,
                agent.actions_per_day,
            )
        })
        .unwrap_or((
            policy.max_lamports_per_agent_day,
            policy.max_actions_per_hour,
            policy.max_actions_per_day,
        ));
    match store.reserve_budget(
        &action,
        &policy,
        agent_daily_limit,
        agent_hourly_limit,
        agent_daily_action_limit,
    ) {
        Ok(()) => {}
        Err(StoreError::BudgetExceeded(reason)) => {
            store.transition_action(
                action.id,
                ExecutionState::Simulated,
                ExecutionState::Rejected,
                &format!("durable {reason} budget reservation denied"),
            )?;
            metrics.rejected.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    }
    if let Err(error) = store.transition_action(
        action.id,
        ExecutionState::Simulated,
        ExecutionState::Submitted,
        "durable pre-submit intent",
    ) {
        store.release_budget(action.id)?;
        return Err(error.into());
    }
    store.commit_budget(action.id)?;
    metrics.submitted.fetch_add(1, Ordering::Relaxed);
    match transport.submit(&action, &instructions).await? {
        SubmissionOutcome::Confirmed { signature } => {
            store.record_signature(action.id, &signature)?;
            store.transition_action(
                action.id,
                ExecutionState::SignatureRecorded,
                ExecutionState::Confirmed,
                "confirmed by loopback RPC",
            )?;
            metrics.confirmed.fetch_add(1, Ordering::Relaxed);
        }
        SubmissionOutcome::Unknown {
            signature: Some(signature),
        } => {
            store.record_signature(action.id, &signature)?;
            store.transition_action(
                action.id,
                ExecutionState::SignatureRecorded,
                ExecutionState::UnknownOutcome,
                "RPC response ambiguous",
            )?;
            metrics.unknown.fetch_add(1, Ordering::Relaxed);
        }
        SubmissionOutcome::Unknown { signature: None } => {
            store.transition_action(
                action.id,
                ExecutionState::Submitted,
                ExecutionState::UnknownOutcome,
                "RPC response lost before signature persisted",
            )?;
            metrics.unknown.fetch_add(1, Ordering::Relaxed);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use account_cooker_config::Config;
    use account_cooker_core::{
        GraphConfig, PersonaKind, Planner, PlannerModel, RelationshipGraph, default_personas,
        deterministic_uuid,
    };
    use account_cooker_protocols::default_adapters;
    use chrono::Duration;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    use super::*;

    struct ConfirmingTransport;
    #[async_trait]
    impl ExecutionTransport for ConfirmingTransport {
        async fn simulate(
            &self,
            _: &PlannedAction,
            _: &[InstructionSpec],
        ) -> Result<(), SchedulerError> {
            Ok(())
        }
        async fn submit(
            &self,
            action: &PlannedAction,
            _: &[InstructionSpec],
        ) -> Result<SubmissionOutcome, SchedulerError> {
            Ok(SubmissionOutcome::Confirmed {
                signature: format!("local-signature-{}", action.id),
            })
        }
        async fn reconcile(&self, _: &str) -> Result<Option<bool>, SchedulerError> {
            Ok(Some(true))
        }
    }

    #[derive(Default)]
    struct ResponseLossTransport {
        submissions: AtomicUsize,
        reconciliations: AtomicUsize,
    }

    #[async_trait]
    impl ExecutionTransport for ResponseLossTransport {
        async fn simulate(
            &self,
            _: &PlannedAction,
            _: &[InstructionSpec],
        ) -> Result<(), SchedulerError> {
            Ok(())
        }

        async fn submit(
            &self,
            action: &PlannedAction,
            _: &[InstructionSpec],
        ) -> Result<SubmissionOutcome, SchedulerError> {
            self.submissions.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(SubmissionOutcome::Unknown {
                signature: Some(format!("landed-but-response-lost-{}", action.id)),
            })
        }

        async fn reconcile(&self, _: &str) -> Result<Option<bool>, SchedulerError> {
            self.reconciliations.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(Some(true))
        }
    }

    fn fixture() -> (tempfile::TempDir, Store, Config) {
        let dir = tempfile::tempdir().unwrap();
        let cfg = Config::safe_default(dir.path().join("state.db"));
        let store = Store::open(&cfg.storage.database_path, 1000).unwrap();
        let planner = Planner::new(default_personas()).unwrap();
        let fleet = deterministic_uuid(99, 0, b"fleet");
        let mix: BTreeMap<_, _> = PersonaKind::ALL.into_iter().map(|p| (p, 1.0)).collect();
        let agents = planner.create_fleet(fleet, 20, 99, &mix, Utc::now() - Duration::days(2));
        let graph = RelationshipGraph::generate(&agents, 99, &GraphConfig::default()).unwrap();
        store.create_fleet("scheduler", &agents, &graph).unwrap();
        let (actions, manifest) = planner.plan(
            fleet,
            &agents,
            &graph,
            Utc::now() - Duration::days(2),
            1,
            99,
            PlannerModel::PersonaSession,
        );
        store.insert_plan(&manifest, &actions).unwrap();
        (dir, store, cfg)
    }

    #[tokio::test]
    async fn bounded_cycle_simulates_without_submission() {
        let (_dir, store, cfg) = fixture();
        let scheduler = Scheduler::new(
            store.clone(),
            cfg.execution_policy(false),
            AdapterRegistry::new(default_adapters()),
            Arc::new(SimulationOnlyTransport),
            4,
            25,
            30,
        );
        let metrics = scheduler
            .run_cycle(Utc::now(), RunMode::DryRun)
            .await
            .unwrap();
        assert!(metrics.claimed <= 25);
        assert_eq!(metrics.submitted, 0);
        assert_eq!(store.summary().unwrap().duplicate_signatures, 0);
    }

    #[tokio::test]
    async fn durable_submission_records_unique_signature() {
        let (_dir, store, cfg) = fixture();
        let scheduler = Scheduler::new(
            store.clone(),
            cfg.execution_policy(false),
            AdapterRegistry::new(default_adapters()),
            Arc::new(ConfirmingTransport),
            4,
            10,
            30,
        );
        let metrics = scheduler
            .run_cycle(Utc::now(), RunMode::ExecuteLocal)
            .await
            .unwrap();
        assert_eq!(
            metrics.confirmed + metrics.rejected + metrics.worker_failures,
            metrics.claimed
        );
        assert_eq!(store.summary().unwrap().duplicate_signatures, 0);
    }

    #[tokio::test]
    async fn lost_rpc_response_is_reconciled_without_resubmission() {
        let (_dir, store, cfg) = fixture();
        let transport = Arc::new(ResponseLossTransport::default());
        let scheduler = Scheduler::new(
            store.clone(),
            cfg.execution_policy(false),
            AdapterRegistry::new(default_adapters()),
            transport.clone(),
            4,
            10,
            30,
        );
        let metrics = scheduler
            .run_cycle(Utc::now(), RunMode::ExecuteLocal)
            .await
            .unwrap();
        assert!(metrics.unknown > 0);
        let submissions_before = transport.submissions.load(AtomicOrdering::SeqCst);
        let reconciled = scheduler.reconcile_pending(100).await.unwrap();
        assert_eq!(reconciled.confirmed, metrics.unknown as usize);
        assert_eq!(
            transport.submissions.load(AtomicOrdering::SeqCst),
            submissions_before,
            "reconciliation must never submit"
        );
        assert_eq!(store.summary().unwrap().unknown_outcomes, 0);
        assert_eq!(
            store.summary().unwrap().reconciliation_records,
            metrics.unknown
        );
    }
}
