use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum DomainError {
    #[error("invalid lifecycle transition from {from:?} to {to:?}")]
    InvalidLifecycleTransition {
        from: LifecycleState,
        to: LifecycleState,
    },
    #[error("invalid execution transition from {from:?} to {to:?}")]
    InvalidExecutionTransition {
        from: ExecutionState,
        to: ExecutionState,
    },
    #[error("persona validation failed: {0}")]
    InvalidPersona(String),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum LifecycleState {
    Dormant,
    Waking,
    Browsing,
    ActiveSession,
    CoolingDown,
    Sleeping,
    Paused,
    Draining,
    Retired,
    Failed,
}

impl LifecycleState {
    pub fn can_transition_to(self, next: Self) -> bool {
        use LifecycleState::*;
        matches!(
            (self, next),
            (Dormant, Waking | Paused | Draining)
                | (Waking, Browsing | Sleeping | Failed | Paused)
                | (Browsing, ActiveSession | CoolingDown | Failed | Paused)
                | (ActiveSession, CoolingDown | Failed | Paused | Draining)
                | (
                    CoolingDown,
                    Sleeping | ActiveSession | Failed | Paused | Draining
                )
                | (Sleeping, Waking | Paused | Draining)
                | (Paused, Dormant | Sleeping | Draining | Retired)
                | (Draining, Retired | Failed)
                | (Failed, Paused | Dormant | Retired)
        ) || self == next
    }

    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        self.can_transition_to(next).then_some(next).ok_or(
            DomainError::InvalidLifecycleTransition {
                from: self,
                to: next,
            },
        )
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionState {
    Planned,
    Leased,
    Simulated,
    Submitted,
    SignatureRecorded,
    Confirmed,
    Rejected,
    FailedBeforeSubmission,
    UnknownOutcome,
    ReconciliationRequired,
    Exhausted,
    Cancelled,
}

impl ExecutionState {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Confirmed
                | Self::Rejected
                | Self::FailedBeforeSubmission
                | Self::Exhausted
                | Self::Cancelled
        )
    }

    pub fn can_transition_to(self, next: Self) -> bool {
        use ExecutionState::*;
        matches!(
            (self, next),
            (Planned, Leased | Cancelled)
                | (
                    Leased,
                    Planned | Simulated | Rejected | FailedBeforeSubmission | Exhausted
                )
                | (
                    Simulated,
                    Planned | Submitted | Confirmed | Rejected | FailedBeforeSubmission
                )
                | (
                    Submitted,
                    SignatureRecorded | UnknownOutcome | ReconciliationRequired
                )
                | (
                    SignatureRecorded,
                    Confirmed | UnknownOutcome | ReconciliationRequired
                )
                | (
                    UnknownOutcome,
                    ReconciliationRequired | Confirmed | FailedBeforeSubmission | Exhausted
                )
                | (
                    ReconciliationRequired,
                    Confirmed | FailedBeforeSubmission | UnknownOutcome | Exhausted
                )
        ) || self == next
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum PersonaKind {
    CasualHolder,
    ActiveTrader,
    StakingOriented,
    TokenExplorer,
    LowFrequencyLongTerm,
}

impl PersonaKind {
    pub const ALL: [Self; 5] = [
        Self::CasualHolder,
        Self::ActiveTrader,
        Self::StakingOriented,
        Self::TokenExplorer,
        Self::LowFrequencyLongTerm,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PersonaProfile {
    pub kind: PersonaKind,
    pub utc_active_hour_mean: f64,
    pub utc_active_hour_stddev: f64,
    pub weekday_sessions_mean: f64,
    pub weekend_multiplier: f64,
    pub session_minutes_mean: f64,
    pub inter_action_seconds_median: f64,
    pub preferred_protocols: BTreeMap<String, f64>,
    pub preferred_assets: Vec<String>,
    pub min_value_lamports: u64,
    pub max_value_lamports: u64,
    pub rare_event_probability: f64,
    pub peer_interaction_probability: f64,
    pub consolidation_probability: f64,
    pub max_daily_spend_lamports: u64,
    pub max_weekly_spend_lamports: u64,
    pub account_age_days: u32,
    pub activity_intensity: f64,
    pub risk_tolerance: f64,
}

impl PersonaProfile {
    pub fn validate(&self) -> Result<(), DomainError> {
        if !(0.0..24.0).contains(&self.utc_active_hour_mean)
            || self.utc_active_hour_stddev <= 0.0
            || self.weekday_sessions_mean < 0.0
            || self.session_minutes_mean <= 0.0
            || self.inter_action_seconds_median <= 0.0
        {
            return Err(DomainError::InvalidPersona(
                "invalid time or session distribution".into(),
            ));
        }
        for (label, p) in [
            ("rare_event_probability", self.rare_event_probability),
            (
                "peer_interaction_probability",
                self.peer_interaction_probability,
            ),
            ("consolidation_probability", self.consolidation_probability),
            ("activity_intensity", self.activity_intensity),
            ("risk_tolerance", self.risk_tolerance),
        ] {
            if !(0.0..=1.0).contains(&p) {
                return Err(DomainError::InvalidPersona(format!(
                    "{label} must be in [0,1]"
                )));
            }
        }
        if self.min_value_lamports == 0
            || self.min_value_lamports > self.max_value_lamports
            || self.max_daily_spend_lamports > self.max_weekly_spend_lamports
            || self.preferred_protocols.is_empty()
            || self.preferred_protocols.values().any(|w| *w <= 0.0)
        {
            return Err(DomainError::InvalidPersona(
                "invalid value, budget, or protocol weights".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Agent {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub public_key: String,
    pub signer_ref: String,
    pub persona: PersonaKind,
    pub lifecycle: LifecycleState,
    pub created_at: DateTime<Utc>,
    pub account_age_days: u32,
    pub daily_budget_lamports: u64,
    pub weekly_budget_lamports: u64,
    pub fee_reserve_lamports: u64,
    pub actions_per_hour: u32,
    pub actions_per_day: u32,
    pub next_action_at: Option<DateTime<Utc>>,
    pub failure_count: u32,
    pub health: String,
    pub deterministic_seed_tag: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum ActionKind {
    NativeTransfer,
    Memo,
    SplTokenTransfer,
    StakeCreate,
    StakeDeactivate,
    StakeWithdraw,
    Browse,
    Consolidate,
}

impl ActionKind {
    pub fn adapter_id(self) -> &'static str {
        match self {
            Self::NativeTransfer | Self::Consolidate => "native-sol",
            Self::Memo | Self::Browse => "memo",
            Self::SplTokenTransfer => "spl-token",
            Self::StakeCreate | Self::StakeDeactivate | Self::StakeWithdraw => "native-stake",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlannedAction {
    pub id: Uuid,
    pub fleet_id: Uuid,
    pub agent_id: Uuid,
    pub scheduled_at: DateTime<Utc>,
    pub kind: ActionKind,
    pub adapter_id: String,
    pub amount_lamports: u64,
    pub counterparty: Option<Uuid>,
    pub asset: String,
    pub state: ExecutionState,
    pub idempotency_key: String,
    pub model: PlannerModel,
    pub seed_tag: String,
    pub session_id: Option<Uuid>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Ord, PartialOrd)]
#[serde(rename_all = "kebab-case")]
pub enum PlannerModel {
    NaiveUniform,
    IndependentWeighted,
    PersonaSession,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlanManifest {
    pub schema_version: u32,
    pub fleet_id: Uuid,
    pub model: PlannerModel,
    pub seed: u64,
    pub seed_tag: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub agent_count: usize,
    pub action_count: usize,
    pub trace_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lifecycle_rejects_impossible_transition() {
        assert!(
            LifecycleState::Retired
                .transition(LifecycleState::Waking)
                .is_err()
        );
        assert!(
            LifecycleState::Sleeping
                .transition(LifecycleState::Waking)
                .is_ok()
        );
    }

    #[test]
    fn execution_states_are_conservative() {
        assert!(!ExecutionState::Submitted.can_transition_to(ExecutionState::Planned));
        assert!(
            ExecutionState::Submitted.can_transition_to(ExecutionState::ReconciliationRequired)
        );
    }
}
