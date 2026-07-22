#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant};

use account_cooker_config::Config;
use account_cooker_core::{
    GraphConfig, LifecycleState, PersonaKind, Planner, PlannerModel, RelationshipGraph,
    default_personas, deterministic_uuid,
};
use account_cooker_evaluator::Evaluator;
use account_cooker_protocols::default_adapters;
use account_cooker_scheduler::{
    AdapterRegistry, LoopbackRpcTransport, RunMode, Scheduler, SimulationOnlyTransport,
};
use account_cooker_store::Store;
use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Serialize;
use sha2::{Digest, Sha256};
use solana_keypair::Keypair;
use tracing::info;
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "account-cooker",
    version,
    about = "Policy-caged autonomous Solana activity fleet manager"
)]
struct Cli {
    #[arg(long, global = true, default_value = "account-cooker.toml")]
    config: PathBuf,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Create safe configuration and an empty WAL-mode database.
    Init,
    /// Validate configuration, storage, tools, RPC policy, and execution guards.
    Doctor,
    /// Create or inspect a clanker fleet.
    Fleet(FleetArgs),
    /// Generate a deterministic, versioned virtual-time plan without network calls.
    Plan(PlanArgs),
    /// Process due actions using a bounded worker pool.
    Run(RunArgs),
    /// Pause one agent, one persona group, or the entire latest fleet.
    Pause(TargetArgs),
    /// Resume one paused agent, one persona group, or the entire latest fleet.
    Resume(TargetArgs),
    /// Stop new actions and move agents toward retirement; never moves public funds implicitly.
    Drain(DrainArgs),
    /// Inspect ambiguous outcomes; never blindly resubmits.
    Reconcile,
    /// Run the adversarial synthetic observer against all three planners.
    Evaluate(EvaluateArgs),
    /// Report durable fleet, budget, failure, and execution state totals.
    Report(OutputArgs),
    /// Create a sanitized reproducibility package.
    Evidence(EvidenceArgs),
    /// Verify evidence file hashes and reject tampering.
    VerifyEvidence { directory: PathBuf },
}

#[derive(Debug, Args)]
struct FleetArgs {
    #[command(subcommand)]
    command: FleetCommands,
}

#[derive(Debug, Subcommand)]
enum FleetCommands {
    Create {
        #[arg(long, default_value_t = 100)]
        count: usize,
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long, default_value = "default-fleet")]
        name: String,
    },
    Inspect(OutputArgs),
}

#[derive(Debug, Args)]
struct PlanArgs {
    #[arg(long, default_value_t = 30)]
    days: u32,
    #[arg(long)]
    seed: Option<u64>,
    #[arg(long, value_enum, default_value_t = ModelArg::PersonaSession)]
    model: ModelArg,
    #[arg(long)]
    starts_at: Option<DateTime<Utc>>,
    #[arg(long)]
    manifest: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModelArg {
    NaiveUniform,
    IndependentWeighted,
    PersonaSession,
}

impl From<ModelArg> for PlannerModel {
    fn from(value: ModelArg) -> Self {
        match value {
            ModelArg::NaiveUniform => Self::NaiveUniform,
            ModelArg::IndependentWeighted => Self::IndependentWeighted,
            ModelArg::PersonaSession => Self::PersonaSession,
        }
    }
}

#[derive(Debug, Args)]
struct RunArgs {
    #[arg(long, default_value_t = true)]
    dry_run: bool,
    #[arg(long, default_value_t = 1)]
    bounded_cycles: u32,
    #[arg(long)]
    daemon: bool,
}

#[derive(Debug, Args)]
struct TargetArgs {
    #[arg(long, conflicts_with = "persona")]
    agent: Option<Uuid>,
    #[arg(long, value_enum, conflicts_with = "agent")]
    persona: Option<PersonaArg>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum PersonaArg {
    CasualHolder,
    ActiveTrader,
    StakingOriented,
    TokenExplorer,
    LowFrequencyLongTerm,
}

impl From<PersonaArg> for PersonaKind {
    fn from(value: PersonaArg) -> Self {
        match value {
            PersonaArg::CasualHolder => Self::CasualHolder,
            PersonaArg::ActiveTrader => Self::ActiveTrader,
            PersonaArg::StakingOriented => Self::StakingOriented,
            PersonaArg::TokenExplorer => Self::TokenExplorer,
            PersonaArg::LowFrequencyLongTerm => Self::LowFrequencyLongTerm,
        }
    }
}

#[derive(Debug, Args)]
struct DrainArgs {
    #[arg(long)]
    agent: Option<Uuid>,
    #[arg(long)]
    confirm: bool,
}

#[derive(Debug, Args)]
struct EvaluateArgs {
    #[arg(long, default_value_t = 30)]
    days: u32,
    #[arg(long)]
    seed: u64,
    #[arg(long, default_value = "evaluation-output")]
    output: PathBuf,
}

#[derive(Debug, Clone, Args)]
struct OutputArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct EvidenceArgs {
    #[arg(long, default_value = "evidence/generated")]
    output: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    init_tracing();
    match cli.command {
        Commands::Init => init(&cli.config),
        Commands::Doctor => doctor(&cli.config).await,
        Commands::Fleet(args) => fleet(&cli.config, args),
        Commands::Plan(args) => plan(&cli.config, args),
        Commands::Run(args) => run(&cli.config, args).await,
        Commands::Pause(args) => set_pause(&cli.config, args, true),
        Commands::Resume(args) => set_pause(&cli.config, args, false),
        Commands::Drain(args) => drain(&cli.config, args),
        Commands::Reconcile => reconcile(&cli.config).await,
        Commands::Evaluate(args) => evaluate(&cli.config, args),
        Commands::Report(args) => report(&cli.config, args),
        Commands::Evidence(args) => evidence(&cli.config, &args.output),
        Commands::VerifyEvidence { directory } => verify_evidence(&directory),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn init(path: &Path) -> Result<()> {
    let state_dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".account-cooker");
    let config = Config::safe_default(state_dir.join("state.db"));
    config
        .save_new(path)
        .with_context(|| format!("failed to initialize {}", path.display()))?;
    let store = open_store(&config)?;
    println!(
        "initialized safe planning-only configuration at {}",
        path.display()
    );
    println!(
        "database schema version {} at {}",
        store.schema_version()?,
        store.path().display()
    );
    Ok(())
}

async fn doctor(path: &Path) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let policy = config.execution_policy(false);
    let solana = tool_version("solana", &["--version"]);
    let surfpool = tool_version("surfpool", &["--version"]);
    let rpc_reachable = reqwest_status(&config.execution.rpc_url).await;
    #[derive(Serialize)]
    struct Doctor<'a> {
        configuration: &'a str,
        schema_version: u32,
        rpc_url: &'a str,
        rpc_loopback: bool,
        rpc_reachable: bool,
        solana: String,
        surfpool: String,
        transaction_execution_allowed: bool,
        signer_setup: &'a str,
    }
    let result = Doctor {
        configuration: "valid",
        schema_version: store.schema_version()?,
        rpc_url: &config.execution.rpc_url,
        rpc_loopback: policy.is_loopback(),
        rpc_reachable,
        solana,
        surfpool,
        transaction_execution_allowed: config.execution.send_transactions
            && policy.is_loopback()
            && config.execution.simulation_required,
        signer_setup: "external references only; no plaintext key material in database",
    };
    println!("{}", serde_json::to_string_pretty(&result)?);
    if config.execution.send_transactions && !rpc_reachable {
        bail!("transaction sending is configured but loopback RPC is not reachable");
    }
    Ok(())
}

async fn reqwest_status(url: &str) -> bool {
    let body = serde_json::json!({"jsonrpc":"2.0","id":1,"method":"getHealth"});
    match tokio::time::timeout(
        StdDuration::from_secs(2),
        reqwest::Client::new().post(url).json(&body).send(),
    )
    .await
    {
        Ok(Ok(response)) => response.status().is_success(),
        _ => false,
    }
}

fn fleet(path: &Path, args: FleetArgs) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    match args.command {
        FleetCommands::Create { count, seed, name } => {
            if count == 0 || count > config.fleet.max_agents {
                bail!(
                    "fleet count must be between 1 and {}",
                    config.fleet.max_agents
                );
            }
            let (seed, entropy_source) = seed
                .map(|seed| (seed, "explicit deterministic seed"))
                .unwrap_or_else(|| (rand::random(), "OS-seeded CSPRNG"));
            let planner = Planner::new(default_personas())?;
            let fleet_id = deterministic_uuid(seed, Utc::now().timestamp_millis() as u64, b"fleet");
            let agents = planner.create_fleet(
                fleet_id,
                count,
                seed,
                &config.behavior.persona_mix,
                Utc::now(),
            );
            let graph_config = GraphConfig {
                cross_group_probability: config.behavior.graph_cross_group_probability,
                ..GraphConfig::default()
            };
            let graph = RelationshipGraph::generate(&agents, seed, &graph_config)?;
            let metrics = graph.metrics(&agents);
            let record = store.create_fleet(&name, &agents, &graph)?;
            println!(
                "{}",
                serde_json::to_string_pretty(
                    &serde_json::json!({"fleet":record,"graph":metrics,"planning_seed":seed,"entropy_source":entropy_source})
                )?
            );
        }
        FleetCommands::Inspect(output) => {
            let record = latest(&store)?;
            let agents = store.agents(record.id)?;
            let graph = store.relationship_graph(record.id)?;
            let value =
                serde_json::json!({"fleet":record,"agents":agents,"graph":graph.metrics(&agents)});
            print_value(&value, output.json)?;
        }
    }
    Ok(())
}

fn plan(path: &Path, args: PlanArgs) -> Result<()> {
    if args.days == 0 || args.days > 365 {
        bail!("days must be between 1 and 365");
    }
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let fleet = latest(&store)?;
    let agents = store.agents(fleet.id)?;
    let graph = store.relationship_graph(fleet.id)?;
    let planner = Planner::new(default_personas())?;
    let (seed, entropy_source) = args
        .seed
        .map(|seed| (seed, "explicit deterministic seed"))
        .unwrap_or_else(|| (rand::random(), "OS-seeded CSPRNG"));
    let started = Instant::now();
    let (actions, manifest) = planner.plan(
        fleet.id,
        &agents,
        &graph,
        args.starts_at.unwrap_or_else(Utc::now),
        args.days,
        seed,
        args.model.into(),
    );
    store.insert_plan(&manifest, &actions)?;
    if let Some(path) = args.manifest {
        write_new(&path, serde_json::to_vec_pretty(&manifest)?)?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"manifest":manifest,"entropy_source":entropy_source,"elapsed_ms":started.elapsed().as_millis(),"database_bytes":store.summary()?.database_bytes})
        )?
    );
    Ok(())
}

async fn run(path: &Path, args: RunArgs) -> Result<()> {
    let config = load_config(path)?;
    if !args.dry_run {
        bail!(
            "broadcast is unavailable in this build path; enable and pass the audited loopback transport after `doctor` succeeds"
        );
    }
    let store = open_store(&config)?;
    let scheduler = Scheduler::new(
        store,
        config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        Arc::new(SimulationOnlyTransport),
        config.scheduler.worker_concurrency,
        config.scheduler.claim_batch_size,
        config.storage.lease_seconds,
    );
    if args.daemon {
        let deadline = tokio::time::Instant::now()
            + StdDuration::from_secs(config.scheduler.max_runtime_seconds);
        let shutdown = shutdown_signal();
        tokio::pin!(shutdown);
        loop {
            let metrics = scheduler.run_cycle(Utc::now(), RunMode::DryRun).await?;
            println!("{}", serde_json::to_string(&metrics)?);
            tokio::select! {
                result = &mut shutdown => {
                    result?;
                    info!("graceful shutdown signal received after durable workers completed");
                    break;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    info!("configured daemon runtime limit reached");
                    break;
                }
                _ = tokio::time::sleep(StdDuration::from_millis(500)) => {}
            }
        }
    } else {
        let cycles = args.bounded_cycles.max(1);
        for index in 0..cycles {
            let metrics = scheduler.run_cycle(Utc::now(), RunMode::DryRun).await?;
            println!("{}", serde_json::to_string(&metrics)?);
            if index + 1 < cycles {
                tokio::time::sleep(StdDuration::from_millis(100)).await;
            }
        }
    }
    Ok(())
}

async fn shutdown_signal() -> Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await?;
    Ok(())
}

fn set_pause(path: &Path, target: TargetArgs, pause: bool) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let fleet = latest(&store)?;
    let (from, to) = if pause {
        (
            vec![
                LifecycleState::Dormant,
                LifecycleState::Waking,
                LifecycleState::Browsing,
                LifecycleState::ActiveSession,
                LifecycleState::CoolingDown,
                LifecycleState::Sleeping,
                LifecycleState::Failed,
            ],
            LifecycleState::Paused,
        )
    } else {
        (vec![LifecycleState::Paused], LifecycleState::Dormant)
    };
    let changed = if let Some(persona) = target.persona {
        store.set_lifecycle_by_persona(fleet.id, persona.into(), &from, to)?
    } else {
        store.set_lifecycle(fleet.id, target.agent, &from, to)?
    };
    println!("changed {changed} agents to {to:?}");
    Ok(())
}

fn drain(path: &Path, args: DrainArgs) -> Result<()> {
    if !args.confirm {
        bail!(
            "drain changes lifecycle state; rerun with --confirm (it never transfers assets implicitly)"
        );
    }
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let fleet = latest(&store)?;
    let changed = store.set_lifecycle(
        fleet.id,
        args.agent,
        &[
            LifecycleState::Dormant,
            LifecycleState::Sleeping,
            LifecycleState::Paused,
            LifecycleState::ActiveSession,
        ],
        LifecycleState::Draining,
    )?;
    println!("marked {changed} agents as draining; fee reserves and state are preserved");
    Ok(())
}

async fn reconcile(path: &Path) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let pending = store.reconciliation_candidates(10_000)?;
    if pending.is_empty() {
        println!("no ambiguous outcomes require reconciliation");
        return Ok(());
    }
    let transport = Arc::new(LoopbackRpcTransport::new(
        &config.execution.rpc_url,
        Arc::new(Keypair::new()),
    )?);
    transport.validate_identity().await?;
    let scheduler = Scheduler::new(
        store,
        config.execution_policy(false),
        AdapterRegistry::new(default_adapters()),
        transport,
        1,
        1,
        config.storage.lease_seconds,
    );
    let summary = scheduler.reconcile_pending(10_000).await?;
    println!("{}", serde_json::to_string_pretty(&summary)?);
    Ok(())
}

fn evaluate(path: &Path, args: EvaluateArgs) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let fleet = latest(&store)?;
    let agents = store.agents(fleet.id)?;
    let graph = store.relationship_graph(fleet.id)?;
    let planner = Planner::new(default_personas())?;
    let start =
        DateTime::from_timestamp(1_767_225_600, 0).context("fixed evaluation timestamp invalid")?;
    let mut datasets = BTreeMap::new();
    for model in [
        PlannerModel::NaiveUniform,
        PlannerModel::IndependentWeighted,
        PlannerModel::PersonaSession,
    ] {
        datasets.insert(
            model,
            planner
                .plan(
                    fleet.id, &agents, &graph, start, args.days, args.seed, model,
                )
                .0,
        );
    }
    let report = Evaluator::evaluate(
        args.seed,
        "fleet-baseline-comparison",
        &agents,
        &datasets,
        &config.evaluator.longitudinal_windows_days,
    )?;
    Evaluator::write_outputs(&report, &args.output)?;
    println!(
        "wrote evaluation.json, evaluation.csv, and evaluation.md to {}",
        args.output.display()
    );
    for model in report.models {
        println!(
            "{:?}: actions={} ARI={:.3} NMI={:.3} ROC_AUC={:.3}",
            model.planner_model,
            model.dataset_size,
            model.metrics.adjusted_rand_index,
            model.metrics.normalized_mutual_information,
            model.metrics.roc_auc
        );
    }
    Ok(())
}

fn report(path: &Path, output: OutputArgs) -> Result<()> {
    let config = load_config(path)?;
    let store = open_store(&config)?;
    let value = serde_json::to_value(store.summary()?)?;
    print_value(&value, output.json)
}

#[derive(Debug, Serialize)]
struct EvidenceManifest {
    schema_version: u32,
    generated_at: String,
    working_tree_identity: String,
    rustc: String,
    cargo: String,
    solana: String,
    surfpool: String,
    configuration_sha256: String,
    fleet_size: u64,
    planned_actions: u64,
    locally_submitted: u64,
    locally_confirmed: u64,
    unknown: u64,
    reconciliation_records: u64,
    duplicate_signatures: u64,
    database_bytes: u64,
    classification: &'static str,
    commands: Vec<&'static str>,
}

fn evidence(config_path: &Path, output: &Path) -> Result<()> {
    if output.exists() && fs::read_dir(output)?.next().is_some() {
        bail!(
            "refusing to overwrite non-empty evidence directory {}",
            output.display()
        );
    }
    let config = load_config(config_path)?;
    let store = open_store(&config)?;
    let summary = store.summary()?;
    let config_bytes = fs::read(config_path)?;
    let manifest = EvidenceManifest {
        schema_version: 1,
        generated_at: Utc::now().to_rfc3339(),
        working_tree_identity: tool_version("git", &["describe", "--always", "--dirty"]),
        rustc: tool_version("rustc", &["--version"]),
        cargo: tool_version("cargo", &["--version"]),
        solana: tool_version("solana", &["--version"]),
        surfpool: tool_version("surfpool", &["--version"]),
        configuration_sha256: sha256(&config_bytes),
        fleet_size: summary.agents,
        planned_actions: summary.planned_actions,
        locally_submitted: state_count(&summary, account_cooker_core::ExecutionState::Submitted)
            + state_count(
                &summary,
                account_cooker_core::ExecutionState::SignatureRecorded,
            )
            + state_count(&summary, account_cooker_core::ExecutionState::Confirmed),
        locally_confirmed: state_count(&summary, account_cooker_core::ExecutionState::Confirmed),
        unknown: summary.unknown_outcomes,
        reconciliation_records: summary.reconciliation_records,
        duplicate_signatures: summary.duplicate_signatures,
        database_bytes: summary.database_bytes,
        classification: "Counts are separated into planning-only, simulated, submitted, and locally confirmed states; no public-chain claim is made.",
        commands: vec![
            "cargo fmt --all -- --check",
            "cargo clippy --workspace --all-targets --all-features -- -D warnings",
            "cargo test --workspace --all-features",
            "cargo xtask verify-evidence",
        ],
    };
    fs::create_dir_all(output)?;
    let manifest_path = output.join("manifest.json");
    fs::write(&manifest_path, serde_json::to_vec_pretty(&manifest)?)?;
    let checksum = format!("{}  manifest.json\n", sha256(&fs::read(&manifest_path)?));
    fs::write(output.join("SHA256SUMS"), checksum)?;
    verify_evidence(output)?;
    println!("wrote sanitized evidence to {}", output.display());
    Ok(())
}

fn verify_evidence(directory: &Path) -> Result<()> {
    let checksums = fs::read_to_string(directory.join("SHA256SUMS"))?;
    let mut checked = 0;
    for line in checksums.lines() {
        let (expected, name) = line
            .split_once("  ")
            .ok_or_else(|| anyhow!("malformed checksum line"))?;
        if name.contains('/') || name.contains("..") {
            bail!("unsafe evidence filename {name}");
        }
        let actual = sha256(&fs::read(directory.join(name))?);
        if actual != expected {
            bail!("evidence checksum mismatch for {name}");
        }
        checked += 1;
    }
    if checked == 0 {
        bail!("no evidence checksums found");
    }
    println!("verified {checked} evidence files");
    Ok(())
}

fn state_count(
    summary: &account_cooker_store::StoreSummary,
    state: account_cooker_core::ExecutionState,
) -> u64 {
    summary
        .actions_by_state
        .iter()
        .find(|(s, _)| *s == state)
        .map(|(_, n)| *n)
        .unwrap_or(0)
}

fn open_store(config: &Config) -> Result<Store> {
    Store::open(
        &config.storage.database_path,
        config.storage.busy_timeout_ms,
    )
    .map_err(Into::into)
}

fn load_config(path: &Path) -> Result<Config> {
    Config::load(path).with_context(|| {
        format!(
            "unable to use configuration {}; run `account-cooker init --config {}` first",
            path.display(),
            path.display()
        )
    })
}

fn latest(store: &Store) -> Result<account_cooker_store::FleetRecord> {
    store
        .latest_fleet()?
        .context("no fleet exists; run `account-cooker fleet create --seed <seed>`")
}

fn tool_version(program: &str, args: &[&str]) -> String {
    Command::new(program)
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "not installed".into())
}

fn print_value(value: &serde_json::Value, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{}", serde_json::to_string(value)?);
    }
    Ok(())
}

fn write_new(path: &Path, bytes: Vec<u8>) -> Result<()> {
    if path.exists() {
        bail!("refusing to overwrite {}", path.display());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)?;
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
