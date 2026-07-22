use std::collections::{BTreeMap, BTreeSet};
use std::net::IpAddr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ActionKind, PlannedAction};

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PolicyError {
    #[error("emergency stop is active")]
    EmergencyStop,
    #[error("agent or protocol is paused")]
    Paused,
    #[error("RPC host is not allowed: {0}")]
    RpcHostDenied(String),
    #[error("public-cluster execution is not compiled and acknowledged")]
    PublicClusterDenied,
    #[error("action type is not allowed: {0:?}")]
    ActionDenied(ActionKind),
    #[error("program is not allowlisted: {0}")]
    ProgramDenied(String),
    #[error("budget exceeded: {0}")]
    BudgetExceeded(&'static str),
    #[error("simulation before send is mandatory")]
    SimulationRequired,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutionPolicy {
    pub rpc_url: String,
    pub loopback_only: bool,
    pub public_cluster_enabled: bool,
    pub public_cluster_acknowledged: bool,
    pub simulation_required: bool,
    pub allowed_actions: BTreeSet<ActionKind>,
    pub allowed_program_ids: BTreeSet<String>,
    pub max_lamports_per_action: u64,
    pub max_lamports_per_agent_day: u64,
    pub max_lamports_per_fleet_day: u64,
    pub minimum_fee_reserve: u64,
    pub max_actions_per_hour: u32,
    pub max_actions_per_day: u32,
    pub max_actions_per_fleet_hour: u32,
    pub max_actions_per_protocol_hour: BTreeMap<String, u32>,
    pub max_compute_units: u32,
    pub max_priority_fee_micro_lamports: u64,
    pub emergency_stop: bool,
    pub paused_agents: BTreeSet<String>,
    pub paused_protocols: BTreeSet<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyObservation {
    pub agent_id: String,
    pub adapter_id: String,
    pub program_ids: Vec<String>,
    pub agent_spend_today: u64,
    pub fleet_spend_today: u64,
    pub actions_this_hour: u32,
    pub actions_today: u32,
    pub fleet_actions_this_hour: u32,
    pub protocol_actions_this_hour: u32,
    pub current_balance: u64,
    pub was_simulated: bool,
}

impl ExecutionPolicy {
    pub fn is_loopback(&self) -> bool {
        let Some(authority) = self
            .rpc_url
            .split("//")
            .nth(1)
            .and_then(|value| value.split('/').next())
        else {
            return false;
        };
        let host = authority.split(':').next().unwrap_or(authority);
        host == "localhost"
            || host
                .parse::<IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false)
    }

    pub fn validate(
        &self,
        action: &PlannedAction,
        observation: &PolicyObservation,
    ) -> Result<(), PolicyError> {
        if self.emergency_stop {
            return Err(PolicyError::EmergencyStop);
        }
        if self.paused_agents.contains(&observation.agent_id)
            || self.paused_protocols.contains(&observation.adapter_id)
        {
            return Err(PolicyError::Paused);
        }
        if self.loopback_only && !self.is_loopback() {
            return Err(PolicyError::RpcHostDenied(self.rpc_url.clone()));
        }
        if !self.is_loopback() && !(self.public_cluster_enabled && self.public_cluster_acknowledged)
        {
            return Err(PolicyError::PublicClusterDenied);
        }
        if !self.allowed_actions.contains(&action.kind) {
            return Err(PolicyError::ActionDenied(action.kind));
        }
        if let Some(program) = observation
            .program_ids
            .iter()
            .find(|program| !self.allowed_program_ids.contains(*program))
        {
            return Err(PolicyError::ProgramDenied(program.clone()));
        }
        if action.amount_lamports > self.max_lamports_per_action {
            return Err(PolicyError::BudgetExceeded("per action"));
        }
        if observation
            .agent_spend_today
            .saturating_add(action.amount_lamports)
            > self.max_lamports_per_agent_day
        {
            return Err(PolicyError::BudgetExceeded("per agent day"));
        }
        if observation
            .fleet_spend_today
            .saturating_add(action.amount_lamports)
            > self.max_lamports_per_fleet_day
        {
            return Err(PolicyError::BudgetExceeded("fleet day"));
        }
        if observation.actions_this_hour >= self.max_actions_per_hour
            || observation.actions_today >= self.max_actions_per_day
        {
            return Err(PolicyError::BudgetExceeded("action rate"));
        }
        if observation.fleet_actions_this_hour >= self.max_actions_per_fleet_hour {
            return Err(PolicyError::BudgetExceeded("fleet action rate"));
        }
        let Some(protocol_limit) = self
            .max_actions_per_protocol_hour
            .get(&observation.adapter_id)
        else {
            return Err(PolicyError::BudgetExceeded("protocol action rate missing"));
        };
        if observation.protocol_actions_this_hour >= *protocol_limit {
            return Err(PolicyError::BudgetExceeded("protocol action rate"));
        }
        if observation.current_balance
            < action
                .amount_lamports
                .saturating_add(self.minimum_fee_reserve)
        {
            return Err(PolicyError::BudgetExceeded("fee reserve"));
        }
        if self.simulation_required && !observation.was_simulated {
            return Err(PolicyError::SimulationRequired);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::{ExecutionState, PlannerModel};

    fn action() -> PlannedAction {
        PlannedAction {
            id: Uuid::new_v4(),
            fleet_id: Uuid::new_v4(),
            agent_id: Uuid::new_v4(),
            scheduled_at: Utc::now(),
            kind: ActionKind::NativeTransfer,
            adapter_id: "native-sol".into(),
            amount_lamports: 100,
            counterparty: None,
            asset: "SOL".into(),
            state: ExecutionState::Planned,
            idempotency_key: "k".into(),
            model: PlannerModel::PersonaSession,
            seed_tag: "test".into(),
            session_id: None,
        }
    }

    #[test]
    fn loopback_detection_is_strict() {
        let mut p = test_policy();
        assert!(p.is_loopback());
        p.rpc_url = "https://api.mainnet-beta.solana.com".into();
        assert!(!p.is_loopback());
    }

    #[test]
    fn fails_closed_without_simulation() {
        let p = test_policy();
        let obs = PolicyObservation {
            agent_id: "a".into(),
            adapter_id: "native-sol".into(),
            program_ids: vec!["system".into()],
            agent_spend_today: 0,
            fleet_spend_today: 0,
            actions_this_hour: 0,
            actions_today: 0,
            fleet_actions_this_hour: 0,
            protocol_actions_this_hour: 0,
            current_balance: 10_000,
            was_simulated: false,
        };
        assert_eq!(
            p.validate(&action(), &obs),
            Err(PolicyError::SimulationRequired)
        );
    }

    fn test_policy() -> ExecutionPolicy {
        ExecutionPolicy {
            rpc_url: "http://127.0.0.1:8899".into(),
            loopback_only: true,
            public_cluster_enabled: false,
            public_cluster_acknowledged: false,
            simulation_required: true,
            allowed_actions: [ActionKind::NativeTransfer].into(),
            allowed_program_ids: ["system".into()].into(),
            max_lamports_per_action: 1_000,
            max_lamports_per_agent_day: 5_000,
            max_lamports_per_fleet_day: 50_000,
            minimum_fee_reserve: 500,
            max_actions_per_hour: 10,
            max_actions_per_day: 100,
            max_actions_per_fleet_hour: 1_000,
            max_actions_per_protocol_hour: [("native-sol".into(), 500)].into(),
            max_compute_units: 200_000,
            max_priority_fee_micro_lamports: 0,
            emergency_stop: false,
            paused_agents: BTreeSet::new(),
            paused_protocols: BTreeSet::new(),
        }
    }
}
