#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use account_cooker_config::Config;
use account_cooker_core::{
    ActionKind, ExecutionState, GraphConfig, PersonaKind, PlanManifest, PlannedAction, Planner,
    PlannerModel, RelationshipGraph, default_personas, deterministic_uuid, seed_tag, trace_hash,
};
use account_cooker_evaluator::Evaluator;
use account_cooker_protocols::default_adapters;
use account_cooker_scheduler::{
    AdapterRegistry, ExecutionTransport, LoopbackRpcTransport, RunMode, Scheduler, SchedulerError,
    SimulationOnlyTransport, SubmissionOutcome,
};
use account_cooker_store::Store;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Duration};
use clap::{Parser, Subcommand};
use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_keypair::Keypair;
use surfpool_sdk::Surfnet;
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "All-Rust project acceptance workflows")]
struct Cli {
    #[command(subcommand)]
    command: Task,
}

#[derive(Subcommand)]
enum Task {
    Setup,
    Demo,
    FullDemo,
    CleanDemo,
    VerifyEvidence,
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Task::Setup => setup(),
        Task::Demo => demo(100, 7, 0x0acc_0001).await,
        Task::FullDemo => demo(1_000, 30, 0x0acc_0030).await,
        Task::CleanDemo => clean_demo(),
        Task::VerifyEvidence => verify(Path::new("demo-output")),
    }
}

fn setup() -> Result<()> {
    for (tool, args) in [
        ("rustc", vec!["--version"]),
        ("cargo", vec!["--version"]),
        ("solana", vec!["--version"]),
        ("surfpool", vec!["--version"]),
    ] {
        let value = Command::new(tool)
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
            .unwrap_or_else(|| "not installed".into());
        println!("{tool}: {value}");
    }
    println!("Surfpool SDK: 1.5.0 (embedded, offline, dynamic loopback ports)");
    Ok(())
}

#[derive(Serialize)]
struct DemoEvidence {
    schema_version: u32,
    seed: u64,
    fleet_size: usize,
    virtual_days: u32,
    planned_actions: usize,
    trace_hash: String,
    deterministic_replay_verified: bool,
    secondary_seed: u64,
    secondary_planned_actions: usize,
    secondary_trace_hash: String,
    planning_elapsed_ms: u128,
    database_bytes: u64,
    simulated_actions: u64,
    locally_submitted: u64,
    locally_confirmed: u64,
    local_balance_before: Option<u64>,
    local_balance_after: Option<u64>,
    crash_injected_leases: usize,
    recovered_leases: usize,
    duplicate_signatures: u64,
    ambiguous_submissions: usize,
    reconciled_without_resubmission: usize,
    surfpool: String,
    acceptance_classification: &'static str,
}

async fn demo(count: usize, days: u32, seed: u64) -> Result<()> {
    setup()?;
    let output = PathBuf::from("demo-output");
    if output.exists() {
        bail!("demo-output already exists; run `cargo xtask clean-demo` first");
    }
    fs::create_dir_all(&output)?;
    let temp = tempfile::tempdir()?;
    let config = Config::safe_default(temp.path().join("state.db"));
    let store = Store::open(
        &config.storage.database_path,
        config.storage.busy_timeout_ms,
    )?;
    let planner = Planner::new(default_personas())?;
    let fleet = deterministic_uuid(seed, 0, b"canonical-fleet");
    let mix: BTreeMap<PersonaKind, f64> = PersonaKind::ALL
        .into_iter()
        .map(|persona| (persona, 1.0))
        .collect();
    let start =
        DateTime::from_timestamp(1_767_225_600, 0).context("invalid canonical timestamp")?;
    let agents = planner.create_fleet(fleet, count, seed, &mix, start - Duration::days(400));
    let graph = RelationshipGraph::generate(&agents, seed, &GraphConfig::default())?;
    store.create_fleet("canonical-demo", &agents, &graph)?;
    let timer = Instant::now();
    let (actions, manifest) = planner.plan(
        fleet,
        &agents,
        &graph,
        start,
        days,
        seed,
        PlannerModel::PersonaSession,
    );
    let elapsed = timer.elapsed();
    store.insert_plan(&manifest, &actions)?;

    let mut datasets = BTreeMap::new();
    for model in [
        PlannerModel::NaiveUniform,
        PlannerModel::IndependentWeighted,
        PlannerModel::PersonaSession,
    ] {
        datasets.insert(
            model,
            planner
                .plan(fleet, &agents, &graph, start, days, seed, model)
                .0,
        );
    }
    let evaluation = Evaluator::evaluate(
        seed,
        "canonical-virtual-time",
        &agents,
        &datasets,
        &[1, 7, 14, 30],
    )?;
    Evaluator::write_outputs(&evaluation, &output)?;
    let replay_trace = trace_hash(
        datasets
            .get(&PlannerModel::PersonaSession)
            .context("persona-session replay dataset missing")?,
    );
    let deterministic_replay_verified = replay_trace == manifest.trace_hash;
    if !deterministic_replay_verified {
        bail!("same-seed persona-session replay changed trace hash");
    }
    drop(datasets);
    let secondary_seed = seed ^ 0x5eed_5eed_d15c_a11e;
    let (secondary_actions, secondary_manifest) = planner.plan(
        fleet,
        &agents,
        &graph,
        start,
        days,
        secondary_seed,
        PlannerModel::PersonaSession,
    );
    if secondary_manifest.trace_hash == manifest.trace_hash {
        bail!("different deterministic seeds unexpectedly produced the same trace");
    }

    let due = start + Duration::days(i64::from(days + 1));
    let crash_claimed = store.claim_due(due, 7, "crash-injection-worker", 1)?;
    let recovered = store.recover_expired_leases(due + Duration::seconds(2))?;
    let scheduler = Scheduler::new(
        store.clone(),
        config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        Arc::new(SimulationOnlyTransport),
        config.scheduler.worker_concurrency,
        config.scheduler.claim_batch_size,
        config.storage.lease_seconds,
    );
    let scheduler_metrics = scheduler.run_cycle(due, RunMode::DryRun).await?;
    let summary = store.summary()?;
    let surfpool = "surfpool-sdk 1.5.0 (embedded offline Surfnet)".to_owned();
    let (locally_submitted, locally_confirmed, balance_before, balance_after) =
        local_surfpool_acceptance(temp.path(), &config, &planner, start, seed).await?;
    let (ambiguous_submissions, reconciled_without_resubmission) =
        ambiguous_restart_acceptance(temp.path(), &config, &planner, start, seed).await?;
    let (local_balance_before, local_balance_after) = (Some(balance_before), Some(balance_after));
    let evidence = DemoEvidence {
        schema_version: store.schema_version()?,
        seed,
        fleet_size: count,
        virtual_days: days,
        planned_actions: actions.len(),
        trace_hash: manifest.trace_hash,
        deterministic_replay_verified,
        secondary_seed,
        secondary_planned_actions: secondary_actions.len(),
        secondary_trace_hash: secondary_manifest.trace_hash,
        planning_elapsed_ms: elapsed.as_millis(),
        database_bytes: summary.database_bytes,
        simulated_actions: scheduler_metrics.simulated,
        locally_submitted,
        locally_confirmed,
        local_balance_before,
        local_balance_after,
        crash_injected_leases: crash_claimed.len(),
        recovered_leases: recovered,
        duplicate_signatures: summary.duplicate_signatures,
        ambiguous_submissions,
        reconciled_without_resubmission,
        surfpool,
        acceptance_classification: "Embedded offline Surfpool acceptance: the exact signed transaction was simulated, submitted once, confirmed by signature, and debited the ephemeral fee payer.",
    };
    let run_path = output.join("run.json");
    fs::write(&run_path, serde_json::to_vec_pretty(&evidence)?)?;
    let mut sums = Vec::new();
    for name in [
        "evaluation.csv",
        "evaluation.json",
        "evaluation.md",
        "run.json",
    ] {
        let bytes = fs::read(output.join(name))?;
        sums.push(format!("{}  {name}", sha256(&bytes)));
    }
    fs::write(output.join("SHA256SUMS"), sums.join("\n") + "\n")?;
    verify(&output)?;
    println!(
        "fleet={count} days={days} actions={} trace={} elapsed_ms={} db_bytes={}",
        actions.len(),
        evidence.trace_hash,
        evidence.planning_elapsed_ms,
        evidence.database_bytes
    );
    println!(
        "crash_leases={} recovered={} simulated={} submitted={} confirmed={}",
        evidence.crash_injected_leases,
        evidence.recovered_leases,
        evidence.simulated_actions,
        evidence.locally_submitted,
        evidence.locally_confirmed
    );
    println!("sanitized evidence: {}", output.display());
    Ok(())
}

#[derive(Default)]
struct AmbiguousTransport {
    submissions: AtomicUsize,
}

#[async_trait::async_trait]
impl ExecutionTransport for AmbiguousTransport {
    async fn simulate(
        &self,
        _: &PlannedAction,
        _: &[account_cooker_core::InstructionSpec],
    ) -> Result<(), SchedulerError> {
        Ok(())
    }

    async fn submit(
        &self,
        action: &PlannedAction,
        _: &[account_cooker_core::InstructionSpec],
    ) -> Result<SubmissionOutcome, SchedulerError> {
        self.submissions.fetch_add(1, Ordering::SeqCst);
        Ok(SubmissionOutcome::Unknown {
            signature: Some(format!("ambiguous-landed-{}", action.id)),
        })
    }

    async fn reconcile(&self, _: &str) -> Result<Option<bool>, SchedulerError> {
        Ok(Some(true))
    }
}

async fn ambiguous_restart_acceptance(
    directory: &Path,
    config: &Config,
    planner: &Planner,
    at: DateTime<chrono::Utc>,
    seed: u64,
) -> Result<(usize, usize)> {
    let store = Store::open(directory.join("ambiguous-restart.db"), 1_000)?;
    let fleet = deterministic_uuid(seed, 2, b"ambiguous-restart-fleet");
    let mix = [(PersonaKind::CasualHolder, 1.0)].into_iter().collect();
    let agents = planner.create_fleet(fleet, 1, seed ^ 2, &mix, at);
    store.create_fleet("ambiguous-restart", &agents, &RelationshipGraph::default())?;
    let action = PlannedAction {
        id: deterministic_uuid(seed, 2, b"ambiguous-restart-action"),
        fleet_id: fleet,
        agent_id: agents[0].id,
        scheduled_at: at,
        kind: ActionKind::Memo,
        adapter_id: "memo".into(),
        amount_lamports: 0,
        counterparty: None,
        asset: "SOL".into(),
        state: ExecutionState::Planned,
        idempotency_key: format!("ambiguous-{}", seed_tag(seed)),
        model: PlannerModel::PersonaSession,
        seed_tag: seed_tag(seed),
        session_id: Some(deterministic_uuid(seed, 2, b"ambiguous-session")),
    };
    let manifest = PlanManifest {
        schema_version: 1,
        fleet_id: fleet,
        model: PlannerModel::PersonaSession,
        seed,
        seed_tag: seed_tag(seed),
        starts_at: at,
        ends_at: at,
        agent_count: 1,
        action_count: 1,
        trace_hash: trace_hash(std::slice::from_ref(&action)),
    };
    store.insert_plan(&manifest, &[action])?;
    let transport = Arc::new(AmbiguousTransport::default());
    let first_process = Scheduler::new(
        store.clone(),
        config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        transport.clone(),
        1,
        1,
        30,
    );
    let first = first_process
        .run_cycle(at + Duration::seconds(1), RunMode::ExecuteLocal)
        .await?;
    if first.unknown != 1 {
        bail!("ambiguous-response injection did not persist unknown outcome");
    }
    drop(first_process);

    let restarted_process = Scheduler::new(
        store.clone(),
        config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        transport.clone(),
        1,
        1,
        30,
    );
    let reconciled = restarted_process.reconcile_pending(10).await?;
    let submissions = transport.submissions.load(Ordering::SeqCst);
    let summary = store.summary()?;
    if submissions != 1
        || reconciled.confirmed != 1
        || summary.unknown_outcomes != 0
        || summary.duplicate_signatures != 0
    {
        bail!("ambiguous restart acceptance violated no-duplicate invariants");
    }
    Ok((submissions, reconciled.confirmed))
}

async fn local_surfpool_acceptance(
    directory: &Path,
    config: &Config,
    planner: &Planner,
    at: DateTime<chrono::Utc>,
    seed: u64,
) -> Result<(u64, u64, u64, u64)> {
    let signer = Arc::new(Keypair::new());
    let surfnet_payer = Keypair::try_from(signer.to_bytes().as_slice())?;
    let mut surfnet = Surfnet::builder()
        .offline(true)
        .payer(surfnet_payer)
        .airdrop_sol(1_000_000_000)
        .start()
        .await?;
    let mut local_config = config.clone();
    local_config.execution.rpc_url = surfnet.rpc_url().to_owned();
    let transport = Arc::new(LoopbackRpcTransport::new(surfnet.rpc_url(), signer)?);
    transport.validate_identity().await?;
    let balance_before = transport.balance().await?;
    let local_store = Store::open(directory.join("local-acceptance.db"), 1_000)?;
    let fleet = deterministic_uuid(seed, 1, b"local-acceptance-fleet");
    let mix = [(PersonaKind::CasualHolder, 1.0)].into_iter().collect();
    let mut agents = planner.create_fleet(fleet, 1, seed ^ 1, &mix, at);
    agents[0].public_key = transport.signer_public_key();
    agents[0].signer_ref = "ephemeral:memory-only".into();
    local_store.create_fleet("local-surfpool", &agents, &RelationshipGraph::default())?;
    let action = PlannedAction {
        id: deterministic_uuid(seed, 1, b"local-memo-action"),
        fleet_id: fleet,
        agent_id: agents[0].id,
        scheduled_at: at,
        kind: ActionKind::Memo,
        adapter_id: "memo".into(),
        amount_lamports: 0,
        counterparty: None,
        asset: "SOL".into(),
        state: ExecutionState::Planned,
        idempotency_key: format!("local-{}", seed_tag(seed)),
        model: PlannerModel::PersonaSession,
        seed_tag: seed_tag(seed),
        session_id: Some(Uuid::new_v4()),
    };
    let manifest = PlanManifest {
        schema_version: 1,
        fleet_id: fleet,
        model: PlannerModel::PersonaSession,
        seed,
        seed_tag: seed_tag(seed),
        starts_at: at,
        ends_at: at,
        agent_count: 1,
        action_count: 1,
        trace_hash: trace_hash(std::slice::from_ref(&action)),
    };
    local_store.insert_plan(&manifest, &[action])?;
    let scheduler = Scheduler::new(
        local_store,
        local_config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        transport.clone(),
        1,
        1,
        30,
    );
    let metrics = scheduler
        .run_cycle(at + Duration::seconds(1), RunMode::ExecuteLocal)
        .await?;
    let balance_after = transport.balance().await?;
    if metrics.confirmed > 0 && balance_after >= balance_before {
        bail!("confirmed local transaction did not debit the fee payer balance");
    }
    if metrics.submitted != 1 || metrics.confirmed != 1 {
        bail!(
            "embedded Surfpool acceptance expected exactly one submission and confirmation, got submitted={} confirmed={}",
            metrics.submitted,
            metrics.confirmed
        );
    }
    surfnet.stop()?;
    Ok((
        metrics.submitted,
        metrics.confirmed,
        balance_before,
        balance_after,
    ))
}

fn clean_demo() -> Result<()> {
    let target = PathBuf::from("demo-output");
    if target.exists() {
        fs::remove_dir_all(&target)?;
        println!("removed generated, reproducible demo-output directory");
    } else {
        println!("demo-output does not exist");
    }
    Ok(())
}

fn verify(directory: &Path) -> Result<()> {
    let sums = fs::read_to_string(directory.join("SHA256SUMS"))?;
    let mut count = 0;
    for line in sums.lines() {
        let (expected, name) = line.split_once("  ").context("malformed SHA256SUMS")?;
        if name.contains('/') || name.contains("..") {
            bail!("unsafe evidence path {name}");
        }
        let actual = sha256(&fs::read(directory.join(name))?);
        if actual != expected {
            bail!("checksum mismatch for {name}");
        }
        count += 1;
    }
    if count == 0 {
        bail!("no evidence files listed");
    }
    println!("verified {count} evidence files");
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
