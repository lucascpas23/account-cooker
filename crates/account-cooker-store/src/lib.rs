#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration as StdDuration;

use account_cooker_core::{
    Agent, ExecutionPolicy, ExecutionState, LifecycleState, PersonaKind, PlanManifest,
    PlannedAction, PolicyObservation, RelationshipGraph,
};
use chrono::{DateTime, Duration, Utc};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use uuid::Uuid;

const MIGRATION_1: &str = include_str!("../../../migrations/0001_initial.sql");
const MIGRATION_2: &str = include_str!("../../../migrations/0002_budget_reservations.sql");
const MIGRATION_3: &str = include_str!("../../../migrations/0003_protocol_rate_limits.sql");

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error("filesystem error for {path}: {source}")]
    Filesystem {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid UUID in database: {0}")]
    InvalidUuid(#[from] uuid::Error),
    #[error("invalid timestamp in database: {0}")]
    InvalidTimestamp(#[from] chrono::ParseError),
    #[error("state conflict: expected {expected:?}, found {actual:?}")]
    StateConflict {
        expected: ExecutionState,
        actual: ExecutionState,
    },
    #[error("action not found: {0}")]
    ActionNotFound(Uuid),
    #[error("durable budget limit exceeded: {0}")]
    BudgetExceeded(&'static str),
}

#[derive(Debug, Clone)]
pub struct Store {
    path: PathBuf,
    busy_timeout: StdDuration,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreSummary {
    pub fleets: u64,
    pub agents: u64,
    pub planned_actions: u64,
    pub actions_by_state: Vec<(ExecutionState, u64)>,
    pub actions_by_protocol: Vec<(String, u64, u64)>,
    pub planned_lamports: u64,
    pub budget_reservations: u64,
    pub reserved_lamports: u64,
    pub committed_lamports: u64,
    pub unknown_outcomes: u64,
    pub reconciliation_records: u64,
    pub duplicate_signatures: u64,
    pub database_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FleetRecord {
    pub id: Uuid,
    pub name: String,
    pub created_at: DateTime<Utc>,
    pub seed_tag: String,
    pub agent_count: usize,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoverySummary {
    pub safe_to_retry: usize,
    pub requires_reconciliation: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReconciliationCandidate {
    pub action: PlannedAction,
    pub signature: Option<String>,
}

impl Store {
    pub fn open(path: impl Into<PathBuf>, busy_timeout_ms: u64) -> Result<Self, StoreError> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| StoreError::Filesystem {
                path: parent.to_owned(),
                source,
            })?;
        }
        let store = Self {
            path,
            busy_timeout: StdDuration::from_millis(busy_timeout_ms),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn connection(&self) -> Result<Connection, StoreError> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(self.busy_timeout)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        Ok(conn)
    }

    pub fn migrate(&self) -> Result<(), StoreError> {
        let conn = self.connection()?;
        conn.execute_batch(MIGRATION_1)?;
        conn.execute(
            "INSERT OR IGNORE INTO schema_migrations(version, applied_at) VALUES(1, ?1)",
            [Utc::now().to_rfc3339()],
        )?;
        for (version, migration) in [(2, MIGRATION_2), (3, MIGRATION_3)] {
            let applied: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM schema_migrations WHERE version=?1)",
                [version],
                |row| row.get(0),
            )?;
            if !applied {
                conn.execute_batch(migration)?;
                conn.execute(
                    "INSERT INTO schema_migrations(version, applied_at) VALUES(?1, ?2)",
                    params![version, Utc::now().to_rfc3339()],
                )?;
            }
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<u32, StoreError> {
        let conn = self.connection()?;
        Ok(conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
            [],
            |row| row.get(0),
        )?)
    }

    pub fn policy_observation(
        &self,
        action: &PlannedAction,
        adapter_id: String,
        program_ids: Vec<String>,
        current_balance: u64,
        was_simulated: bool,
    ) -> Result<PolicyObservation, StoreError> {
        let conn = self.connection()?;
        let (utc_day, utc_hour) = budget_periods(Utc::now());
        let active = "status IN ('reserved','committed')";
        let agent_spend_today = sum_lamports(
            &conn,
            &format!(
                "SELECT COALESCE(SUM(lamports),0) FROM budget_reservations WHERE agent_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_day,
        )?;
        let fleet_spend_today = sum_lamports(
            &conn,
            &format!(
                "SELECT COALESCE(SUM(lamports),0) FROM budget_reservations WHERE fleet_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.fleet_id.to_string(),
            &utc_day,
        )?;
        let actions_this_hour = count_reservations(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE agent_id=?1 AND utc_hour=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_hour,
        )?;
        let actions_today = count_reservations(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE agent_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_day,
        )?;
        let fleet_actions_this_hour = count_reservations(
            &conn,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE fleet_id=?1 AND utc_hour=?2 AND {active}"
            ),
            &action.fleet_id.to_string(),
            &utc_hour,
        )?;
        let protocol_actions_this_hour: u32 = conn
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM budget_reservations WHERE fleet_id=?1 AND adapter_id=?2 AND utc_hour=?3 AND {active}"
                ),
                params![action.fleet_id.to_string(), action.adapter_id, utc_hour],
                |row| row.get::<_, i64>(0),
            )?
            .max(0)
            .min(i64::from(u32::MAX)) as u32;
        Ok(PolicyObservation {
            agent_id: action.agent_id.to_string(),
            adapter_id,
            program_ids,
            agent_spend_today,
            fleet_spend_today,
            actions_this_hour,
            actions_today,
            fleet_actions_this_hour,
            protocol_actions_this_hour,
            current_balance,
            was_simulated,
        })
    }

    /// Atomically checks and reserves the spend/rate capacity used by a send.
    /// Existing reservations are idempotent, and ambiguous submissions remain
    /// charged until reconciliation proves that they did not land.
    pub fn reserve_budget(
        &self,
        action: &PlannedAction,
        policy: &ExecutionPolicy,
        agent_daily_limit: u64,
        agent_hourly_action_limit: u32,
        agent_daily_action_limit: u32,
    ) -> Result<(), StoreError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let action_id = action.id.to_string();
        if tx
            .query_row(
                "SELECT 1 FROM budget_reservations WHERE action_id=?1",
                [&action_id],
                |_| Ok(()),
            )
            .optional()?
            .is_some()
        {
            tx.commit()?;
            return Ok(());
        }
        // Budgets protect real execution, so periods are based on send time,
        // never on a possibly historical virtual schedule timestamp.
        let (utc_day, utc_hour) = budget_periods(Utc::now());
        let active = "status IN ('reserved','committed')";
        let agent_spend = sum_lamports(
            &tx,
            &format!(
                "SELECT COALESCE(SUM(lamports),0) FROM budget_reservations WHERE agent_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_day,
        )?;
        let fleet_spend = sum_lamports(
            &tx,
            &format!(
                "SELECT COALESCE(SUM(lamports),0) FROM budget_reservations WHERE fleet_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.fleet_id.to_string(),
            &utc_day,
        )?;
        let hour_actions = count_reservations(
            &tx,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE agent_id=?1 AND utc_hour=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_hour,
        )?;
        let day_actions = count_reservations(
            &tx,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE agent_id=?1 AND utc_day=?2 AND {active}"
            ),
            &action.agent_id.to_string(),
            &utc_day,
        )?;
        let fleet_hour_actions = count_reservations(
            &tx,
            &format!(
                "SELECT COUNT(*) FROM budget_reservations WHERE fleet_id=?1 AND utc_hour=?2 AND {active}"
            ),
            &action.fleet_id.to_string(),
            &utc_hour,
        )?;
        let protocol_hour_actions: u32 = tx
            .query_row(
                &format!(
                    "SELECT COUNT(*) FROM budget_reservations WHERE fleet_id=?1 AND adapter_id=?2 AND utc_hour=?3 AND {active}"
                ),
                params![action.fleet_id.to_string(), action.adapter_id, utc_hour],
                |row| row.get::<_, i64>(0),
            )?
            .max(0)
            .min(i64::from(u32::MAX)) as u32;
        if action.amount_lamports > policy.max_lamports_per_action {
            return Err(StoreError::BudgetExceeded("per action"));
        }
        if agent_spend.saturating_add(action.amount_lamports)
            > policy.max_lamports_per_agent_day.min(agent_daily_limit)
        {
            return Err(StoreError::BudgetExceeded("per agent day"));
        }
        if fleet_spend.saturating_add(action.amount_lamports) > policy.max_lamports_per_fleet_day {
            return Err(StoreError::BudgetExceeded("fleet day"));
        }
        if hour_actions >= policy.max_actions_per_hour.min(agent_hourly_action_limit)
            || day_actions >= policy.max_actions_per_day.min(agent_daily_action_limit)
        {
            return Err(StoreError::BudgetExceeded("action rate"));
        }
        if fleet_hour_actions >= policy.max_actions_per_fleet_hour {
            return Err(StoreError::BudgetExceeded("fleet action rate"));
        }
        let protocol_limit = policy
            .max_actions_per_protocol_hour
            .get(&action.adapter_id)
            .copied()
            .ok_or(StoreError::BudgetExceeded("protocol action rate missing"))?;
        if protocol_hour_actions >= protocol_limit {
            return Err(StoreError::BudgetExceeded("protocol action rate"));
        }
        let now = Utc::now().to_rfc3339();
        tx.execute(
            "INSERT INTO budget_reservations(action_id,fleet_id,agent_id,utc_day,utc_hour,lamports,status,created_at,updated_at,adapter_id) VALUES(?1,?2,?3,?4,?5,?6,'reserved',?7,?7,?8)",
            params![action_id, action.fleet_id.to_string(), action.agent_id.to_string(), utc_day, utc_hour, u64_to_i64(action.amount_lamports), now, action.adapter_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn commit_budget(&self, action_id: Uuid) -> Result<(), StoreError> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE budget_reservations SET status='committed',updated_at=?1 WHERE action_id=?2 AND status='reserved'",
            params![Utc::now().to_rfc3339(), action_id.to_string()],
        )?;
        Ok(())
    }

    pub fn release_budget(&self, action_id: Uuid) -> Result<(), StoreError> {
        let conn = self.connection()?;
        conn.execute(
            "UPDATE budget_reservations SET status='released',updated_at=?1 WHERE action_id=?2 AND status='reserved'",
            params![Utc::now().to_rfc3339(), action_id.to_string()],
        )?;
        Ok(())
    }

    pub fn create_fleet(
        &self,
        name: &str,
        agents: &[Agent],
        graph: &RelationshipGraph,
    ) -> Result<FleetRecord, StoreError> {
        let Some(first) = agents.first() else {
            return Err(StoreError::Sql(rusqlite::Error::InvalidParameterName(
                "fleet must contain at least one agent".into(),
            )));
        };
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let record = FleetRecord {
            id: first.fleet_id,
            name: name.into(),
            created_at: first.created_at,
            seed_tag: first
                .deterministic_seed_tag
                .clone()
                .unwrap_or_else(|| "production-random".into()),
            agent_count: agents.len(),
        };
        tx.execute(
            "INSERT INTO fleets(id,name,created_at,seed_tag,agent_count) VALUES(?1,?2,?3,?4,?5)",
            params![
                record.id.to_string(),
                record.name,
                record.created_at.to_rfc3339(),
                record.seed_tag,
                record.agent_count as i64
            ],
        )?;
        {
            let mut statement = tx.prepare(
                "INSERT INTO agents(id,fleet_id,public_key,signer_ref,persona,lifecycle,created_at,account_age_days,daily_budget_lamports,weekly_budget_lamports,fee_reserve_lamports,actions_per_hour,actions_per_day,next_action_at,failure_count,health,deterministic_seed_tag) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
            )?;
            for agent in agents {
                statement.execute(params![
                    agent.id.to_string(),
                    agent.fleet_id.to_string(),
                    agent.public_key,
                    agent.signer_ref,
                    to_db(&agent.persona)?,
                    to_db(&agent.lifecycle)?,
                    agent.created_at.to_rfc3339(),
                    i64::from(agent.account_age_days),
                    u64_to_i64(agent.daily_budget_lamports),
                    u64_to_i64(agent.weekly_budget_lamports),
                    u64_to_i64(agent.fee_reserve_lamports),
                    i64::from(agent.actions_per_hour),
                    i64::from(agent.actions_per_day),
                    agent.next_action_at.map(|v| v.to_rfc3339()),
                    i64::from(agent.failure_count),
                    agent.health,
                    agent.deterministic_seed_tag,
                ])?;
            }
        }
        {
            let mut statement = tx.prepare(
                "INSERT INTO relationships(fleet_id,a,b,strength,household,protocol_affinity) VALUES(?1,?2,?3,?4,?5,?6)",
            )?;
            for edge in &graph.edges {
                statement.execute(params![
                    record.id.to_string(),
                    edge.a.to_string(),
                    edge.b.to_string(),
                    edge.strength,
                    i64::from(edge.household),
                    edge.protocol_affinity,
                ])?;
            }
        }
        tx.execute(
            "INSERT INTO immutable_events(fleet_id,event_type,redacted_payload,created_at) VALUES(?1,'fleet_created',?2,?3)",
            params![record.id.to_string(), serde_json::to_string(&record)?, Utc::now().to_rfc3339()],
        )?;
        tx.commit()?;
        Ok(record)
    }

    pub fn latest_fleet(&self) -> Result<Option<FleetRecord>, StoreError> {
        let conn = self.connection()?;
        conn.query_row(
            "SELECT id,name,created_at,seed_tag,agent_count FROM fleets ORDER BY created_at DESC LIMIT 1",
            [],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?, row.get::<_, String>(2)?, row.get::<_, String>(3)?, row.get::<_, i64>(4)?)),
        )
        .optional()?
        .map(|(id, name, created_at, seed_tag, agent_count)| Ok(FleetRecord {
            id: Uuid::parse_str(&id)?, name, created_at: DateTime::parse_from_rfc3339(&created_at)?.with_timezone(&Utc), seed_tag, agent_count: agent_count.max(0) as usize,
        }))
        .transpose()
    }

    pub fn agents(&self, fleet_id: Uuid) -> Result<Vec<Agent>, StoreError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,fleet_id,public_key,signer_ref,persona,lifecycle,created_at,account_age_days,daily_budget_lamports,weekly_budget_lamports,fee_reserve_lamports,actions_per_hour,actions_per_day,next_action_at,failure_count,health,deterministic_seed_tag FROM agents WHERE fleet_id=?1 ORDER BY id")?;
        let rows = stmt.query_map([fleet_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, u32>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, u32>(11)?,
                row.get::<_, u32>(12)?,
                row.get::<_, Option<String>>(13)?,
                row.get::<_, u32>(14)?,
                row.get::<_, String>(15)?,
                row.get::<_, Option<String>>(16)?,
            ))
        })?;
        rows.map(|row| {
            let (
                id,
                fleet_id,
                public_key,
                signer_ref,
                persona,
                lifecycle,
                created_at,
                account_age_days,
                daily,
                weekly,
                reserve,
                per_hour,
                per_day,
                next,
                failures,
                health,
                seed,
            ) = row?;
            Ok(Agent {
                id: Uuid::parse_str(&id)?,
                fleet_id: Uuid::parse_str(&fleet_id)?,
                public_key,
                signer_ref,
                persona: from_db(&persona)?,
                lifecycle: from_db(&lifecycle)?,
                created_at: parse_time(&created_at)?,
                account_age_days,
                daily_budget_lamports: daily.max(0) as u64,
                weekly_budget_lamports: weekly.max(0) as u64,
                fee_reserve_lamports: reserve.max(0) as u64,
                actions_per_hour: per_hour,
                actions_per_day: per_day,
                next_action_at: next.map(|v| parse_time(&v)).transpose()?,
                failure_count: failures,
                health,
                deterministic_seed_tag: seed,
            })
        })
        .collect()
    }

    pub fn relationship_graph(&self, fleet_id: Uuid) -> Result<RelationshipGraph, StoreError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT a,b,strength,household,protocol_affinity FROM relationships WHERE fleet_id=?1 ORDER BY a,b")?;
        let rows = stmt.query_map([fleet_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, f64>(2)?,
                row.get::<_, u32>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })?;
        let mut edges = Vec::new();
        for row in rows {
            let (a, b, strength, household, protocol_affinity) = row?;
            edges.push(account_cooker_core::RelationshipEdge {
                a: Uuid::parse_str(&a)?,
                b: Uuid::parse_str(&b)?,
                strength,
                household,
                protocol_affinity,
            });
        }
        Ok(RelationshipGraph { edges })
    }

    pub fn insert_plan(
        &self,
        manifest: &PlanManifest,
        actions: &[PlannedAction],
    ) -> Result<(), StoreError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        {
            let mut stmt = tx.prepare("INSERT INTO planned_actions(id,fleet_id,agent_id,scheduled_at,kind,adapter_id,amount_lamports,counterparty,asset,state,idempotency_key,planner_model,seed_tag,session_id,updated_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)")?;
            for action in actions {
                stmt.execute(params![
                    action.id.to_string(),
                    action.fleet_id.to_string(),
                    action.agent_id.to_string(),
                    action.scheduled_at.to_rfc3339(),
                    to_db(&action.kind)?,
                    action.adapter_id,
                    u64_to_i64(action.amount_lamports),
                    action.counterparty.map(|v| v.to_string()),
                    action.asset,
                    to_db(&action.state)?,
                    action.idempotency_key,
                    to_db(&action.model)?,
                    action.seed_tag,
                    action.session_id.map(|v| v.to_string()),
                    Utc::now().to_rfc3339(),
                ])?;
            }
        }
        tx.execute("INSERT INTO immutable_events(fleet_id,event_type,redacted_payload,created_at) VALUES(?1,'plan_created',?2,?3)", params![manifest.fleet_id.to_string(), serde_json::to_string(manifest)?, Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn actions(&self, fleet_id: Uuid) -> Result<Vec<PlannedAction>, StoreError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare("SELECT id,fleet_id,agent_id,scheduled_at,kind,adapter_id,amount_lamports,counterparty,asset,state,idempotency_key,planner_model,seed_tag,session_id FROM planned_actions WHERE fleet_id=?1 ORDER BY scheduled_at,agent_id,id")?;
        let rows = stmt.query_map([fleet_id.to_string()], action_row)?;
        rows.map(|row| hydrate_action(row?)).collect()
    }

    pub fn claim_due(
        &self,
        now: DateTime<Utc>,
        limit: usize,
        worker: &str,
        lease_seconds: u64,
    ) -> Result<Vec<PlannedAction>, StoreError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let planned = to_db(&ExecutionState::Planned)?;
        let mut ids = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT id FROM (
                    SELECT candidate.id,candidate.agent_id,candidate.scheduled_at,
                           ROW_NUMBER() OVER (PARTITION BY candidate.agent_id ORDER BY candidate.scheduled_at,candidate.id) AS agent_rank
                    FROM planned_actions candidate
                    WHERE candidate.state=?1 AND candidate.scheduled_at<=?2
                      AND NOT EXISTS (
                          SELECT 1 FROM planned_actions active
                          WHERE active.agent_id=candidate.agent_id
                            AND active.lease_expires_at>?2
                      )
                 ) WHERE agent_rank=1 ORDER BY scheduled_at,agent_id LIMIT ?3",
            )?;
            for id in stmt.query_map(params![planned, now.to_rfc3339(), limit as i64], |row| {
                row.get::<_, String>(0)
            })? {
                ids.push(id?);
            }
        }
        let leased = to_db(&ExecutionState::Leased)?;
        let expires = now + Duration::seconds(lease_seconds.min(i64::MAX as u64) as i64);
        for id in &ids {
            tx.execute("UPDATE planned_actions SET state=?1,lease_owner=?2,lease_expires_at=?3,updated_at=?4 WHERE id=?5 AND state=?6", params![leased, worker, expires.to_rfc3339(), now.to_rfc3339(), id, planned])?;
        }
        tx.commit()?;
        let conn = self.connection()?;
        let mut actions = Vec::new();
        for id in ids {
            if let Some(action) = load_action(&conn, &id)? {
                actions.push(action);
            }
        }
        Ok(actions)
    }

    pub fn transition_action(
        &self,
        id: Uuid,
        expected: ExecutionState,
        next: ExecutionState,
        category: &str,
    ) -> Result<(), StoreError> {
        if !expected.can_transition_to(next) {
            return Err(StoreError::StateConflict {
                expected,
                actual: next,
            });
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let keep_lease = matches!(
            next,
            ExecutionState::Simulated
                | ExecutionState::Submitted
                | ExecutionState::SignatureRecorded
                | ExecutionState::UnknownOutcome
                | ExecutionState::ReconciliationRequired
        );
        let changed = if keep_lease {
            tx.execute(
                "UPDATE planned_actions SET state=?1,last_error=?2,updated_at=?3 WHERE id=?4 AND state=?5",
                params![to_db(&next)?, category, Utc::now().to_rfc3339(), id.to_string(), to_db(&expected)?],
            )?
        } else {
            tx.execute(
                "UPDATE planned_actions SET state=?1,lease_owner=NULL,lease_expires_at=NULL,last_error=?2,updated_at=?3 WHERE id=?4 AND state=?5",
                params![to_db(&next)?, category, Utc::now().to_rfc3339(), id.to_string(), to_db(&expected)?],
            )?
        };
        if changed != 1 {
            let actual = tx
                .query_row(
                    "SELECT state FROM planned_actions WHERE id=?1",
                    [id.to_string()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            return match actual {
                Some(actual) => Err(StoreError::StateConflict {
                    expected,
                    actual: from_db(&actual)?,
                }),
                None => Err(StoreError::ActionNotFound(id)),
            };
        }
        tx.execute("INSERT INTO transaction_attempts(action_id,attempt,phase,result_category,created_at) VALUES(?1,(SELECT retry_count FROM planned_actions WHERE id=?1),?2,?3,?4)", params![id.to_string(), to_db(&next)?, category, Utc::now().to_rfc3339()])?;
        tx.execute("INSERT INTO immutable_events(action_id,event_type,redacted_payload,created_at) VALUES(?1,'execution_transition',?2,?3)", params![id.to_string(), format!("{{\"from\":\"{}\",\"to\":\"{}\"}}", to_db(&expected)?, to_db(&next)?), Utc::now().to_rfc3339()])?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_signature(&self, id: Uuid, signature: &str) -> Result<(), StoreError> {
        let conn = self.connection()?;
        conn.execute("UPDATE planned_actions SET signature=?1,state=?2,updated_at=?3 WHERE id=?4 AND state IN (?5,?6)", params![signature, to_db(&ExecutionState::SignatureRecorded)?, Utc::now().to_rfc3339(), id.to_string(), to_db(&ExecutionState::Submitted)?, to_db(&ExecutionState::UnknownOutcome)?])?;
        Ok(())
    }

    pub fn recover_expired_leases(&self, now: DateTime<Utc>) -> Result<usize, StoreError> {
        Ok(self.recover_interrupted_actions(now)?.safe_to_retry)
    }

    /// Recovers only checkpoints that are provably before network submission.
    /// Once durable submit intent exists, an expired worker is routed to
    /// reconciliation and is never made claimable again.
    pub fn recover_interrupted_actions(
        &self,
        now: DateTime<Utc>,
    ) -> Result<RecoverySummary, StoreError> {
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let timestamp = now.to_rfc3339();
        let safe_to_retry = tx.execute(
            "UPDATE planned_actions SET state=?1,lease_owner=NULL,lease_expires_at=NULL,retry_count=retry_count+1,last_error='expired pre-submit checkpoint recovered',updated_at=?2 WHERE state IN (?3,?4) AND lease_expires_at<?2",
            params![
                to_db(&ExecutionState::Planned)?,
                timestamp,
                to_db(&ExecutionState::Leased)?,
                to_db(&ExecutionState::Simulated)?
            ],
        )?;
        let requires_reconciliation = tx.execute(
            "UPDATE planned_actions SET state=?1,lease_owner=NULL,lease_expires_at=NULL,last_error='expired post-submit checkpoint requires reconciliation',updated_at=?2 WHERE state IN (?3,?4,?5) AND lease_expires_at<?2",
            params![
                to_db(&ExecutionState::ReconciliationRequired)?,
                timestamp,
                to_db(&ExecutionState::Submitted)?,
                to_db(&ExecutionState::SignatureRecorded)?,
                to_db(&ExecutionState::UnknownOutcome)?
            ],
        )?;
        tx.commit()?;
        Ok(RecoverySummary {
            safe_to_retry,
            requires_reconciliation,
        })
    }

    pub fn reconciliation_candidates(
        &self,
        limit: usize,
    ) -> Result<Vec<ReconciliationCandidate>, StoreError> {
        let conn = self.connection()?;
        let mut stmt = conn.prepare(
            "SELECT id,signature FROM planned_actions WHERE state IN (?1,?2) ORDER BY updated_at,id LIMIT ?3",
        )?;
        let pairs = stmt
            .query_map(
                params![
                    to_db(&ExecutionState::UnknownOutcome)?,
                    to_db(&ExecutionState::ReconciliationRequired)?,
                    limit.min(i64::MAX as usize) as i64
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        pairs
            .into_iter()
            .map(|(id, signature)| {
                let action = load_action(&conn, &id)?.ok_or_else(|| {
                    StoreError::ActionNotFound(Uuid::parse_str(&id).unwrap_or_else(|_| Uuid::nil()))
                })?;
                Ok(ReconciliationCandidate { action, signature })
            })
            .collect()
    }

    pub fn resolve_reconciliation(
        &self,
        action_id: Uuid,
        expected: ExecutionState,
        resolved: ExecutionState,
        evidence: &str,
    ) -> Result<(), StoreError> {
        if !expected.can_transition_to(resolved) {
            return Err(StoreError::StateConflict {
                expected,
                actual: resolved,
            });
        }
        let mut conn = self.connection()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let signature: Option<String> = tx
            .query_row(
                "SELECT signature FROM planned_actions WHERE id=?1 AND state=?2",
                params![action_id.to_string(), to_db(&expected)?],
                |row| row.get(0),
            )
            .optional()?
            .ok_or(StoreError::StateConflict {
                expected,
                actual: expected,
            })?;
        tx.execute(
            "UPDATE planned_actions SET state=?1,lease_owner=NULL,lease_expires_at=NULL,last_error=?2,updated_at=?3 WHERE id=?4 AND state=?5",
            params![to_db(&resolved)?, evidence, Utc::now().to_rfc3339(), action_id.to_string(), to_db(&expected)?],
        )?;
        tx.execute(
            "INSERT INTO reconciliation_records(action_id,previous_state,resolved_state,signature,evidence,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
            params![action_id.to_string(), to_db(&expected)?, to_db(&resolved)?, signature, evidence, Utc::now().to_rfc3339()],
        )?;
        if resolved == ExecutionState::Exhausted {
            tx.execute(
                "UPDATE budget_reservations SET status='released',updated_at=?1 WHERE action_id=?2",
                params![Utc::now().to_rfc3339(), action_id.to_string()],
            )?;
        } else if resolved == ExecutionState::Confirmed {
            tx.execute(
                "UPDATE budget_reservations SET status='committed',updated_at=?1 WHERE action_id=?2",
                params![Utc::now().to_rfc3339(), action_id.to_string()],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_lifecycle(
        &self,
        fleet: Uuid,
        agent: Option<Uuid>,
        from: &[LifecycleState],
        to: LifecycleState,
    ) -> Result<usize, StoreError> {
        let conn = self.connection()?;
        let allowed: Vec<String> = from.iter().map(to_db).collect::<Result<_, _>>()?;
        let changed = if let Some(agent) = agent {
            conn.execute(&format!("UPDATE agents SET lifecycle=?1 WHERE fleet_id=?2 AND id=?3 AND lifecycle IN ({})", placeholders(allowed.len(), 4)), rusqlite::params_from_iter([to_db(&to)?, fleet.to_string(), agent.to_string()].into_iter().chain(allowed)))?
        } else {
            conn.execute(
                &format!(
                    "UPDATE agents SET lifecycle=?1 WHERE fleet_id=?2 AND lifecycle IN ({})",
                    placeholders(allowed.len(), 3)
                ),
                rusqlite::params_from_iter(
                    [to_db(&to)?, fleet.to_string()].into_iter().chain(allowed),
                ),
            )?
        };
        Ok(changed)
    }

    pub fn set_lifecycle_by_persona(
        &self,
        fleet: Uuid,
        persona: PersonaKind,
        from: &[LifecycleState],
        to: LifecycleState,
    ) -> Result<usize, StoreError> {
        let conn = self.connection()?;
        let allowed: Vec<String> = from.iter().map(to_db).collect::<Result<_, _>>()?;
        let changed = conn.execute(
            &format!(
                "UPDATE agents SET lifecycle=?1 WHERE fleet_id=?2 AND persona=?3 AND lifecycle IN ({})",
                placeholders(allowed.len(), 4)
            ),
            rusqlite::params_from_iter(
                [to_db(&to)?, fleet.to_string(), to_db(&persona)?]
                    .into_iter()
                    .chain(allowed),
            ),
        )?;
        Ok(changed)
    }

    pub fn summary(&self) -> Result<StoreSummary, StoreError> {
        let conn = self.connection()?;
        let count = |table: &str| -> Result<u64, StoreError> {
            let value: i64 =
                conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                    row.get(0)
                })?;
            Ok(value.max(0) as u64)
        };
        let mut stmt = conn
            .prepare("SELECT state,COUNT(*) FROM planned_actions GROUP BY state ORDER BY state")?;
        let mut actions_by_state = Vec::new();
        for row in stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })? {
            let (state, n) = row?;
            actions_by_state.push((from_db(&state)?, n.max(0) as u64));
        }
        let duplicate_signatures: i64 = conn.query_row("SELECT COUNT(*) FROM (SELECT signature FROM planned_actions WHERE signature IS NOT NULL GROUP BY signature HAVING COUNT(*) > 1)", [], |row| row.get(0))?;
        let mut protocol_stmt = conn.prepare(
            "SELECT adapter_id,COUNT(*),COALESCE(SUM(amount_lamports),0) FROM planned_actions GROUP BY adapter_id ORDER BY adapter_id",
        )?;
        let actions_by_protocol = protocol_stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?.max(0) as u64,
                    row.get::<_, i64>(2)?.max(0) as u64,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let planned_lamports: i64 = conn.query_row(
            "SELECT COALESCE(SUM(amount_lamports),0) FROM planned_actions",
            [],
            |row| row.get(0),
        )?;
        let budget_lamports = |status: &str| -> Result<u64, StoreError> {
            let value: i64 = conn.query_row(
                "SELECT COALESCE(SUM(lamports),0) FROM budget_reservations WHERE status=?1",
                [status],
                |row| row.get(0),
            )?;
            Ok(value.max(0) as u64)
        };
        Ok(StoreSummary {
            fleets: count("fleets")?,
            agents: count("agents")?,
            planned_actions: count("planned_actions")?,
            actions_by_protocol,
            planned_lamports: planned_lamports.max(0) as u64,
            budget_reservations: count("budget_reservations")?,
            reserved_lamports: budget_lamports("reserved")?,
            committed_lamports: budget_lamports("committed")?,
            unknown_outcomes: conn
                .query_row::<i64, _, _>(
                    "SELECT COUNT(*) FROM planned_actions WHERE state IN (?1,?2)",
                    params![
                        to_db(&ExecutionState::UnknownOutcome)?,
                        to_db(&ExecutionState::ReconciliationRequired)?
                    ],
                    |row| row.get(0),
                )?
                .max(0) as u64,
            reconciliation_records: count("reconciliation_records")?,
            duplicate_signatures: duplicate_signatures.max(0) as u64,
            actions_by_state,
            database_bytes: fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0),
        })
    }
}

type ActionTuple = (
    String,
    String,
    String,
    String,
    String,
    String,
    i64,
    Option<String>,
    String,
    String,
    String,
    String,
    String,
    Option<String>,
);

fn action_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ActionTuple> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
        row.get(10)?,
        row.get(11)?,
        row.get(12)?,
        row.get(13)?,
    ))
}

fn hydrate_action(row: ActionTuple) -> Result<PlannedAction, StoreError> {
    let (
        id,
        fleet,
        agent,
        scheduled,
        kind,
        adapter,
        amount,
        counterparty,
        asset,
        state,
        key,
        model,
        seed,
        session,
    ) = row;
    Ok(PlannedAction {
        id: Uuid::parse_str(&id)?,
        fleet_id: Uuid::parse_str(&fleet)?,
        agent_id: Uuid::parse_str(&agent)?,
        scheduled_at: parse_time(&scheduled)?,
        kind: from_db(&kind)?,
        adapter_id: adapter,
        amount_lamports: amount.max(0) as u64,
        counterparty: counterparty.map(|v| Uuid::parse_str(&v)).transpose()?,
        asset,
        state: from_db(&state)?,
        idempotency_key: key,
        model: from_db(&model)?,
        seed_tag: seed,
        session_id: session.map(|v| Uuid::parse_str(&v)).transpose()?,
    })
}

fn load_action(conn: &Connection, id: &str) -> Result<Option<PlannedAction>, StoreError> {
    conn.query_row("SELECT id,fleet_id,agent_id,scheduled_at,kind,adapter_id,amount_lamports,counterparty,asset,state,idempotency_key,planner_model,seed_tag,session_id FROM planned_actions WHERE id=?1", [id], action_row).optional()?.map(hydrate_action).transpose()
}

fn to_db<T: Serialize>(value: &T) -> Result<String, StoreError> {
    Ok(serde_json::to_string(value)?.trim_matches('"').to_owned())
}

fn from_db<T: DeserializeOwned>(value: &str) -> Result<T, StoreError> {
    Ok(serde_json::from_str(&format!("\"{value}\""))?)
}

fn parse_time(value: &str) -> Result<DateTime<Utc>, StoreError> {
    Ok(DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc))
}

fn u64_to_i64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

fn budget_periods(at: DateTime<Utc>) -> (String, String) {
    (
        at.format("%Y-%m-%d").to_string(),
        at.format("%Y-%m-%dT%H").to_string(),
    )
}

fn sum_lamports(
    conn: &Connection,
    sql: &str,
    identity: &str,
    period: &str,
) -> Result<u64, StoreError> {
    let value: i64 = conn.query_row(sql, params![identity, period], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}

fn count_reservations(
    conn: &Connection,
    sql: &str,
    identity: &str,
    period: &str,
) -> Result<u32, StoreError> {
    let value: i64 = conn.query_row(sql, params![identity, period], |row| row.get(0))?;
    Ok(value.max(0).min(i64::from(u32::MAX)) as u32)
}

fn placeholders(count: usize, start: usize) -> String {
    (start..start + count)
        .map(|i| format!("?{i}"))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use account_cooker_core::{
        ExecutionPolicy, GraphConfig, PersonaKind, Planner, PlannerModel, RelationshipGraph,
        default_personas, deterministic_uuid,
    };
    use std::collections::BTreeMap;
    use std::sync::{Arc, Barrier};

    use super::*;

    fn fixture() -> (tempfile::TempDir, Store, FleetRecord, Vec<PlannedAction>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("state.db"), 1000).unwrap();
        let planner = Planner::new(default_personas()).unwrap();
        let fleet = deterministic_uuid(7, 0, b"fleet");
        let mix: BTreeMap<_, _> = PersonaKind::ALL.into_iter().map(|p| (p, 1.0)).collect();
        let agents = planner.create_fleet(fleet, 12, 7, &mix, Utc::now());
        let graph = RelationshipGraph::generate(&agents, 7, &GraphConfig::default()).unwrap();
        let record = store.create_fleet("test", &agents, &graph).unwrap();
        let (actions, manifest) = planner.plan(
            fleet,
            &agents,
            &graph,
            Utc::now() - Duration::days(2),
            1,
            7,
            PlannerModel::PersonaSession,
        );
        store.insert_plan(&manifest, &actions).unwrap();
        (dir, store, record, actions)
    }

    #[test]
    fn migration_is_idempotent_and_wal_enabled() {
        let (_dir, store, _, _) = fixture();
        store.migrate().unwrap();
        assert_eq!(store.schema_version().unwrap(), 3);
    }

    #[test]
    fn idempotency_key_is_unique() {
        let (_dir, store, record, actions) = fixture();
        let manifest = PlanManifest {
            schema_version: 1,
            fleet_id: record.id,
            model: account_cooker_core::PlannerModel::PersonaSession,
            seed: 7,
            seed_tag: "x".into(),
            starts_at: Utc::now(),
            ends_at: Utc::now(),
            agent_count: 12,
            action_count: actions.len(),
            trace_hash: "x".into(),
        };
        assert!(store.insert_plan(&manifest, &actions).is_err());
    }

    #[test]
    fn expired_lease_recovers_without_duplication() {
        let (_dir, store, _, _) = fixture();
        let claimed = store.claim_due(Utc::now(), 5, "worker-1", 1).unwrap();
        let recovered = store
            .recover_expired_leases(Utc::now() + Duration::seconds(2))
            .unwrap();
        assert_eq!(recovered, claimed.len());
        assert_eq!(
            store.summary().unwrap().planned_actions as usize,
            store
                .summary()
                .unwrap()
                .actions_by_state
                .iter()
                .map(|(_, n)| *n as usize)
                .sum::<usize>()
        );
    }

    #[test]
    fn a_claim_batch_serializes_each_agent() {
        let (_dir, store, _, _) = fixture();
        let claimed = store.claim_due(Utc::now(), 100, "worker-1", 30).unwrap();
        let unique: std::collections::BTreeSet<_> =
            claimed.iter().map(|action| action.agent_id).collect();
        assert_eq!(unique.len(), claimed.len());
        let second = store.claim_due(Utc::now(), 100, "worker-2", 30).unwrap();
        assert!(
            second
                .iter()
                .all(|action| !unique.contains(&action.agent_id))
        );
    }

    #[test]
    fn lifecycle_controls_can_target_a_persona_group() {
        let (_dir, store, fleet, _) = fixture();
        let before = store.agents(fleet.id).unwrap();
        let persona = before[0].persona;
        let expected = before
            .iter()
            .filter(|agent| agent.persona == persona)
            .count();

        let changed = store
            .set_lifecycle_by_persona(
                fleet.id,
                persona,
                &[LifecycleState::Dormant],
                LifecycleState::Paused,
            )
            .unwrap();

        assert_eq!(changed, expected);
        let after = store.agents(fleet.id).unwrap();
        assert!(after.iter().all(|agent| {
            (agent.persona == persona && agent.lifecycle == LifecycleState::Paused)
                || (agent.persona != persona && agent.lifecycle == LifecycleState::Dormant)
        }));
    }

    #[test]
    fn recovery_distinguishes_pre_submit_from_post_submit_checkpoints() {
        let (_dir, store, _, _) = fixture();
        let now = Utc::now();
        let first = store.claim_due(now, 1, "worker-1", 1).unwrap().remove(0);
        store
            .transition_action(
                first.id,
                ExecutionState::Leased,
                ExecutionState::Simulated,
                "checkpoint",
            )
            .unwrap();
        let recovered = store
            .recover_interrupted_actions(now + Duration::seconds(2))
            .unwrap();
        assert_eq!(recovered.safe_to_retry, 1);
        assert_eq!(recovered.requires_reconciliation, 0);

        let second = store
            .claim_due(now + Duration::seconds(3), 1, "worker-2", 1)
            .unwrap()
            .remove(0);
        store
            .transition_action(
                second.id,
                ExecutionState::Leased,
                ExecutionState::Simulated,
                "checkpoint",
            )
            .unwrap();
        store
            .transition_action(
                second.id,
                ExecutionState::Simulated,
                ExecutionState::Submitted,
                "durable intent",
            )
            .unwrap();
        let recovered = store
            .recover_interrupted_actions(now + Duration::seconds(5))
            .unwrap();
        assert_eq!(recovered.safe_to_retry, 0);
        assert_eq!(recovered.requires_reconciliation, 1);
        assert!(
            store
                .reconciliation_candidates(10)
                .unwrap()
                .iter()
                .any(|candidate| candidate.action.id == second.id)
        );
    }

    #[test]
    fn concurrent_budget_reservations_cannot_overspend_fleet_limit() {
        let (_dir, store, _, actions) = fixture();
        let mut selected = None;
        for (index, first) in actions.iter().enumerate() {
            for second in &actions[index + 1..] {
                if first.agent_id != second.agent_id
                    && first.amount_lamports > 0
                    && second.amount_lamports > 0
                    && budget_periods(first.scheduled_at).0 == budget_periods(second.scheduled_at).0
                {
                    selected = Some((first.clone(), second.clone()));
                    break;
                }
            }
            if selected.is_some() {
                break;
            }
        }
        let (first, second) = selected.expect("fixture must include two funded peer actions");
        let fleet_limit = first.amount_lamports.max(second.amount_lamports);
        let policy = ExecutionPolicy {
            rpc_url: "http://127.0.0.1:8899".into(),
            loopback_only: true,
            public_cluster_enabled: false,
            public_cluster_acknowledged: false,
            simulation_required: true,
            allowed_actions: [first.kind, second.kind].into(),
            allowed_program_ids: Default::default(),
            max_lamports_per_action: fleet_limit,
            max_lamports_per_agent_day: u64::MAX,
            max_lamports_per_fleet_day: fleet_limit,
            minimum_fee_reserve: 0,
            max_actions_per_hour: u32::MAX,
            max_actions_per_day: u32::MAX,
            max_actions_per_fleet_hour: u32::MAX,
            max_actions_per_protocol_hour: [
                (first.adapter_id.clone(), u32::MAX),
                (second.adapter_id.clone(), u32::MAX),
            ]
            .into(),
            max_compute_units: 200_000,
            max_priority_fee_micro_lamports: 0,
            emergency_stop: false,
            paused_agents: Default::default(),
            paused_protocols: Default::default(),
        };
        let barrier = Arc::new(Barrier::new(3));
        let handles = [first, second].map(|action| {
            let store = store.clone();
            let policy = policy.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                store.reserve_budget(&action, &policy, u64::MAX, u32::MAX, u32::MAX)
            })
        });
        barrier.wait();
        let results = handles.map(|handle| handle.join().unwrap());
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            results
                .iter()
                .filter(|result| matches!(result, Err(StoreError::BudgetExceeded("fleet day"))))
                .count(),
            1
        );
    }

    #[test]
    fn durable_protocol_hour_limit_is_enforced_across_agents() {
        let (_dir, store, _, actions) = fixture();
        let (first, second) = actions
            .iter()
            .enumerate()
            .find_map(|(index, first)| {
                actions[index + 1..]
                    .iter()
                    .find(|second| {
                        first.agent_id != second.agent_id && first.adapter_id == second.adapter_id
                    })
                    .map(|second| (first.clone(), second.clone()))
            })
            .expect("fixture must contain a shared protocol across agents");
        let mut policy = ExecutionPolicy {
            rpc_url: "http://127.0.0.1:8899".into(),
            loopback_only: true,
            public_cluster_enabled: false,
            public_cluster_acknowledged: false,
            simulation_required: true,
            allowed_actions: [first.kind, second.kind].into(),
            allowed_program_ids: Default::default(),
            max_lamports_per_action: u64::MAX,
            max_lamports_per_agent_day: u64::MAX,
            max_lamports_per_fleet_day: u64::MAX,
            minimum_fee_reserve: 0,
            max_actions_per_hour: u32::MAX,
            max_actions_per_day: u32::MAX,
            max_actions_per_fleet_hour: u32::MAX,
            max_actions_per_protocol_hour: Default::default(),
            max_compute_units: 200_000,
            max_priority_fee_micro_lamports: 0,
            emergency_stop: false,
            paused_agents: Default::default(),
            paused_protocols: Default::default(),
        };
        policy
            .max_actions_per_protocol_hour
            .insert(first.adapter_id.clone(), 1);
        store
            .reserve_budget(&first, &policy, u64::MAX, u32::MAX, u32::MAX)
            .unwrap();
        assert!(matches!(
            store.reserve_budget(&second, &policy, u64::MAX, u32::MAX, u32::MAX),
            Err(StoreError::BudgetExceeded("protocol action rate"))
        ));
    }
}
