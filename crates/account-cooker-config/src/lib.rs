#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use account_cooker_core::{ActionKind, ExecutionPolicy, PersonaKind};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const SYSTEM_PROGRAM_ID: &str = "11111111111111111111111111111111";
pub const MEMO_PROGRAM_ID: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";
pub const TOKEN_PROGRAM_ID: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
pub const ATA_PROGRAM_ID: &str = "ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL";
pub const STAKE_PROGRAM_ID: &str = "Stake11111111111111111111111111111111111111";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("unable to read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unable to parse configuration: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("invalid configuration: {0}")]
    Validation(String),
    #[error("unable to serialize configuration: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("unable to write configuration {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionMode {
    PlanningOnly,
    Local,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub execution: ExecutionConfig,
    pub storage: StorageConfig,
    pub fleet: FleetConfig,
    pub scheduler: SchedulerConfig,
    pub budgets: BudgetConfig,
    pub protocols: ProtocolConfig,
    pub behavior: BehaviorConfig,
    pub evaluator: EvaluatorConfig,
    pub logging: LoggingConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    pub mode: ExecutionMode,
    pub rpc_url: String,
    pub loopback_only: bool,
    pub send_transactions: bool,
    pub public_cluster_enabled: bool,
    pub simulation_required: bool,
    pub max_compute_units: u32,
    pub max_priority_fee_micro_lamports: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    pub database_path: PathBuf,
    pub lease_seconds: u64,
    pub busy_timeout_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct FleetConfig {
    pub max_agents: usize,
    pub default_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SchedulerConfig {
    pub worker_concurrency: usize,
    pub rpc_concurrency: usize,
    pub claim_batch_size: usize,
    pub max_retries: u32,
    pub reconciliation_interval_seconds: u64,
    pub max_runtime_seconds: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    pub max_lamports_per_action: u64,
    pub max_lamports_per_agent_day: u64,
    pub max_lamports_per_fleet_day: u64,
    pub minimum_fee_reserve: u64,
    pub max_actions_per_hour: u32,
    pub max_actions_per_day: u32,
    pub max_actions_per_fleet_hour: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolConfig {
    pub allowlist: BTreeSet<String>,
    pub program_id_allowlist: BTreeSet<String>,
    pub action_allowlist: BTreeSet<ActionKind>,
    pub paused: BTreeSet<String>,
    pub max_actions_per_hour: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BehaviorConfig {
    pub persona_mix: BTreeMap<PersonaKind, f64>,
    pub graph_cross_group_probability: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct EvaluatorConfig {
    pub longitudinal_windows_days: Vec<u32>,
    pub classification_threshold: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct LoggingConfig {
    pub format: String,
    pub level: String,
    pub redact_agent_ids: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self::safe_default(PathBuf::from(".account-cooker/state.db"))
    }
}

impl Config {
    pub fn safe_default(database_path: PathBuf) -> Self {
        Self {
            schema_version: 1,
            execution: ExecutionConfig {
                mode: ExecutionMode::PlanningOnly,
                rpc_url: "http://127.0.0.1:8899".into(),
                loopback_only: true,
                send_transactions: false,
                public_cluster_enabled: false,
                simulation_required: true,
                max_compute_units: 200_000,
                max_priority_fee_micro_lamports: 0,
            },
            storage: StorageConfig {
                database_path,
                lease_seconds: 30,
                busy_timeout_ms: 5_000,
            },
            fleet: FleetConfig {
                max_agents: 10_000,
                default_count: 100,
            },
            scheduler: SchedulerConfig {
                worker_concurrency: 16,
                rpc_concurrency: 8,
                claim_batch_size: 64,
                max_retries: 3,
                reconciliation_interval_seconds: 30,
                max_runtime_seconds: 300,
            },
            budgets: BudgetConfig {
                max_lamports_per_action: 20_000_000,
                max_lamports_per_agent_day: 100_000_000,
                max_lamports_per_fleet_day: 10_000_000_000,
                minimum_fee_reserve: 500_000,
                max_actions_per_hour: 20,
                max_actions_per_day: 100,
                max_actions_per_fleet_hour: 5_000,
            },
            protocols: ProtocolConfig {
                allowlist: ["native-sol", "memo", "spl-token", "native-stake"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                program_id_allowlist: [
                    SYSTEM_PROGRAM_ID,
                    MEMO_PROGRAM_ID,
                    TOKEN_PROGRAM_ID,
                    ATA_PROGRAM_ID,
                    STAKE_PROGRAM_ID,
                ]
                .into_iter()
                .map(str::to_owned)
                .collect(),
                action_allowlist: [
                    ActionKind::NativeTransfer,
                    ActionKind::Memo,
                    ActionKind::SplTokenTransfer,
                    ActionKind::StakeCreate,
                    ActionKind::StakeDeactivate,
                    ActionKind::StakeWithdraw,
                    ActionKind::Browse,
                    ActionKind::Consolidate,
                ]
                .into(),
                paused: BTreeSet::new(),
                max_actions_per_hour: [
                    ("native-sol".into(), 2_000),
                    ("memo".into(), 1_000),
                    ("spl-token".into(), 2_000),
                    ("native-stake".into(), 500),
                ]
                .into(),
            },
            behavior: BehaviorConfig {
                persona_mix: PersonaKind::ALL
                    .into_iter()
                    .map(|persona| (persona, 1.0))
                    .collect(),
                graph_cross_group_probability: 0.018,
            },
            evaluator: EvaluatorConfig {
                longitudinal_windows_days: vec![1, 7, 14, 30],
                classification_threshold: 0.5,
            },
            logging: LoggingConfig {
                format: "text".into(),
                level: "info".into(),
                redact_agent_ids: true,
            },
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let raw = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.to_owned(),
            source,
        })?;
        let config: Self = toml::from_str(&raw)?;
        config.validate()?;
        Ok(config)
    }

    pub fn save_new(&self, path: &Path) -> Result<(), ConfigError> {
        self.validate()?;
        if path.exists() {
            return Err(ConfigError::Validation(format!(
                "refusing to overwrite {}",
                path.display()
            )));
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
                path: parent.to_owned(),
                source,
            })?;
        }
        let data = toml::to_string_pretty(self)?;
        fs::write(path, data).map_err(|source| ConfigError::Write {
            path: path.to_owned(),
            source,
        })
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if self.schema_version != 1 {
            return Err(ConfigError::Validation("unsupported schema_version".into()));
        }
        if self.fleet.max_agents == 0
            || self.fleet.default_count == 0
            || self.fleet.default_count > self.fleet.max_agents
        {
            return Err(ConfigError::Validation("invalid fleet limits".into()));
        }
        if self.scheduler.worker_concurrency == 0
            || self.scheduler.rpc_concurrency == 0
            || self.scheduler.claim_batch_size == 0
        {
            return Err(ConfigError::Validation(
                "scheduler concurrency must be bounded and non-zero".into(),
            ));
        }
        if self.budgets.max_lamports_per_action == 0
            || self.budgets.max_lamports_per_agent_day > self.budgets.max_lamports_per_fleet_day
            || self.budgets.max_actions_per_fleet_hour == 0
        {
            return Err(ConfigError::Validation("invalid budgets".into()));
        }
        if self.protocols.allowlist.is_empty()
            || self.protocols.program_id_allowlist.is_empty()
            || self.protocols.action_allowlist.is_empty()
        {
            return Err(ConfigError::Validation(
                "protocol, action, and program allowlists must be explicit".into(),
            ));
        }
        if self.protocols.max_actions_per_hour.len() != self.protocols.allowlist.len()
            || self.protocols.allowlist.iter().any(|protocol| {
                self.protocols
                    .max_actions_per_hour
                    .get(protocol)
                    .is_none_or(|limit| *limit == 0)
            })
        {
            return Err(ConfigError::Validation(
                "every allowlisted protocol requires a positive hourly rate limit".into(),
            ));
        }
        if self.behavior.persona_mix.values().any(|w| *w <= 0.0)
            || self.behavior.persona_mix.values().sum::<f64>() <= 0.0
        {
            return Err(ConfigError::Validation("invalid persona mix".into()));
        }
        if self.execution.send_transactions {
            if self.execution.mode != ExecutionMode::Local
                || !self.execution.loopback_only
                || !self.execution.simulation_required
                || self.execution.public_cluster_enabled
            {
                return Err(ConfigError::Validation(
                    "sending fails closed unless local, loopback-only, simulation-required, and public clusters are disabled"
                        .into(),
                ));
            }
            let policy = self.execution_policy(false);
            if !policy.is_loopback() {
                return Err(ConfigError::Validation(
                    "transaction sending requires a loopback RPC URL".into(),
                ));
            }
        }
        Ok(())
    }

    pub fn execution_policy(&self, public_acknowledged: bool) -> ExecutionPolicy {
        ExecutionPolicy {
            rpc_url: self.execution.rpc_url.clone(),
            loopback_only: self.execution.loopback_only,
            public_cluster_enabled: self.execution.public_cluster_enabled,
            public_cluster_acknowledged: public_acknowledged,
            simulation_required: self.execution.simulation_required,
            allowed_actions: self.protocols.action_allowlist.clone(),
            allowed_program_ids: self.protocols.program_id_allowlist.clone(),
            max_lamports_per_action: self.budgets.max_lamports_per_action,
            max_lamports_per_agent_day: self.budgets.max_lamports_per_agent_day,
            max_lamports_per_fleet_day: self.budgets.max_lamports_per_fleet_day,
            minimum_fee_reserve: self.budgets.minimum_fee_reserve,
            max_actions_per_hour: self.budgets.max_actions_per_hour,
            max_actions_per_day: self.budgets.max_actions_per_day,
            max_actions_per_fleet_hour: self.budgets.max_actions_per_fleet_hour,
            max_actions_per_protocol_hour: self.protocols.max_actions_per_hour.clone(),
            max_compute_units: self.execution.max_compute_units,
            max_priority_fee_micro_lamports: self.execution.max_priority_fee_micro_lamports,
            emergency_stop: false,
            paused_agents: BTreeSet::new(),
            paused_protocols: self.protocols.paused.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_fail_closed() {
        let cfg = Config::default();
        assert!(!cfg.execution.send_transactions);
        assert!(cfg.execution.loopback_only);
        assert!(!cfg.execution.public_cluster_enabled);
        assert!(cfg.execution.simulation_required);
        cfg.validate().unwrap();
    }

    #[test]
    fn rejects_public_send() {
        let mut cfg = Config::default();
        cfg.execution.mode = ExecutionMode::Local;
        cfg.execution.send_transactions = true;
        cfg.execution.loopback_only = false;
        cfg.execution.rpc_url = "https://api.mainnet-beta.solana.com".into();
        assert!(cfg.validate().is_err());
    }

    #[test]
    fn rejects_unknown_fields() {
        let text = toml::to_string_pretty(&Config::default()).unwrap() + "\nunknown = true\n";
        assert!(toml::from_str::<Config>(&text).is_err());
    }

    #[test]
    fn shipped_configs_are_valid() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        Config::load(&root.join("examples/dry-run-1000-agents.toml")).unwrap();
        Config::load(&root.join("examples/local-surfpool.toml")).unwrap();
    }
}
