use std::collections::BTreeMap;

use chrono::{DateTime, Datelike, Duration, Timelike, Utc};
use rand::{Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;
use rand_distr::{Distribution, LogNormal, Normal, Poisson};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    ActionKind, Agent, DomainError, ExecutionState, LifecycleState, PersonaKind, PersonaProfile,
    PlanManifest, PlannedAction, PlannerModel, RelationshipGraph,
};

pub fn default_personas() -> BTreeMap<PersonaKind, PersonaProfile> {
    use PersonaKind::*;
    [
        profile(
            CasualHolder,
            19.0,
            3.2,
            0.55,
            18.0,
            155.0,
            50_000,
            4_000_000,
        ),
        profile(
            ActiveTrader,
            14.0,
            4.0,
            2.7,
            42.0,
            48.0,
            100_000,
            18_000_000,
        ),
        profile(
            StakingOriented,
            16.0,
            3.8,
            0.28,
            12.0,
            310.0,
            500_000,
            30_000_000,
        ),
        profile(
            TokenExplorer,
            21.0,
            4.5,
            1.35,
            26.0,
            83.0,
            20_000,
            7_000_000,
        ),
        profile(
            LowFrequencyLongTerm,
            18.0,
            2.8,
            0.09,
            9.0,
            620.0,
            100_000,
            2_000_000,
        ),
    ]
    .into_iter()
    .map(|p| (p.kind, p))
    .collect()
}

#[allow(clippy::too_many_arguments)]
fn profile(
    kind: PersonaKind,
    active_hour: f64,
    hour_stddev: f64,
    sessions: f64,
    session_minutes: f64,
    delay_seconds: f64,
    min_value: u64,
    max_value: u64,
) -> PersonaProfile {
    let protocols = match kind {
        PersonaKind::CasualHolder => [("native-sol", 0.55), ("memo", 0.25), ("spl-token", 0.20)],
        PersonaKind::ActiveTrader => [("spl-token", 0.62), ("native-sol", 0.28), ("memo", 0.10)],
        PersonaKind::StakingOriented => {
            [("native-stake", 0.62), ("native-sol", 0.30), ("memo", 0.08)]
        }
        PersonaKind::TokenExplorer => [("spl-token", 0.50), ("memo", 0.28), ("native-sol", 0.22)],
        PersonaKind::LowFrequencyLongTerm => {
            [("native-sol", 0.46), ("native-stake", 0.44), ("memo", 0.10)]
        }
    };
    PersonaProfile {
        kind,
        utc_active_hour_mean: active_hour,
        utc_active_hour_stddev: hour_stddev,
        weekday_sessions_mean: sessions,
        weekend_multiplier: if matches!(kind, PersonaKind::ActiveTrader) {
            0.72
        } else {
            1.18
        },
        session_minutes_mean: session_minutes,
        inter_action_seconds_median: delay_seconds,
        preferred_protocols: protocols.into_iter().map(|(k, v)| (k.into(), v)).collect(),
        preferred_assets: vec!["SOL".into(), "USDC".into()],
        min_value_lamports: min_value,
        max_value_lamports: max_value,
        rare_event_probability: 0.015,
        peer_interaction_probability: 0.58,
        consolidation_probability: if matches!(kind, PersonaKind::ActiveTrader) {
            0.025
        } else {
            0.008
        },
        max_daily_spend_lamports: max_value.saturating_mul(5),
        max_weekly_spend_lamports: max_value.saturating_mul(20),
        account_age_days: match kind {
            PersonaKind::TokenExplorer => 35,
            PersonaKind::LowFrequencyLongTerm => 1_400,
            _ => 420,
        },
        activity_intensity: (sessions / 3.0).clamp(0.05, 1.0),
        risk_tolerance: if matches!(kind, PersonaKind::ActiveTrader | PersonaKind::TokenExplorer) {
            0.8
        } else {
            0.35
        },
    }
}

#[derive(Debug, Clone)]
pub struct Planner {
    personas: BTreeMap<PersonaKind, PersonaProfile>,
}

impl Planner {
    pub fn new(personas: BTreeMap<PersonaKind, PersonaProfile>) -> Result<Self, DomainError> {
        for persona in personas.values() {
            persona.validate()?;
        }
        Ok(Self { personas })
    }

    pub fn create_fleet(
        &self,
        fleet_id: Uuid,
        count: usize,
        seed: u64,
        mix: &BTreeMap<PersonaKind, f64>,
        created_at: DateTime<Utc>,
    ) -> Vec<Agent> {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        (0..count)
            .map(|index| {
                let persona = choose_persona(mix, rng.random());
                let p = &self.personas[&persona];
                let id = deterministic_uuid(seed, index as u64, b"agent");
                let key_bytes = Sha256::digest(
                    [seed.to_le_bytes().as_slice(), &(index as u64).to_le_bytes()].concat(),
                );
                Agent {
                    id,
                    fleet_id,
                    public_key: bs58::encode(&key_bytes[..32]).into_string(),
                    signer_ref: format!("external:agent/{id}"),
                    persona,
                    lifecycle: LifecycleState::Dormant,
                    created_at,
                    account_age_days: p.account_age_days.saturating_add(rng.random_range(0..180)),
                    daily_budget_lamports: p.max_daily_spend_lamports,
                    weekly_budget_lamports: p.max_weekly_spend_lamports,
                    fee_reserve_lamports: 500_000,
                    actions_per_hour: 12,
                    actions_per_day: 80,
                    next_action_at: None,
                    failure_count: 0,
                    health: "healthy".into(),
                    deterministic_seed_tag: Some(seed_tag(seed)),
                }
            })
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn plan(
        &self,
        fleet_id: Uuid,
        agents: &[Agent],
        graph: &RelationshipGraph,
        starts_at: DateTime<Utc>,
        days: u32,
        seed: u64,
        model: PlannerModel,
    ) -> (Vec<PlannedAction>, PlanManifest) {
        let mut rng = ChaCha20Rng::seed_from_u64(seed);
        let mut actions = Vec::new();
        let end = starts_at + Duration::days(i64::from(days));
        for agent in agents {
            let profile = &self.personas[&agent.persona];
            match model {
                PlannerModel::NaiveUniform => plan_naive(
                    &mut rng,
                    fleet_id,
                    agent,
                    profile,
                    graph,
                    starts_at,
                    end,
                    seed,
                    &mut actions,
                ),
                PlannerModel::IndependentWeighted => plan_independent(
                    &mut rng,
                    fleet_id,
                    agent,
                    profile,
                    graph,
                    starts_at,
                    days,
                    seed,
                    &mut actions,
                ),
                PlannerModel::PersonaSession => plan_sessions(
                    &mut rng,
                    fleet_id,
                    agent,
                    profile,
                    graph,
                    starts_at,
                    days,
                    seed,
                    &mut actions,
                ),
            }
        }
        actions.sort_by_key(|a| (a.scheduled_at, a.agent_id, a.id));
        let trace_hash = trace_hash(&actions);
        let manifest = PlanManifest {
            schema_version: 1,
            fleet_id,
            model,
            seed,
            seed_tag: seed_tag(seed),
            starts_at,
            ends_at: end,
            agent_count: agents.len(),
            action_count: actions.len(),
            trace_hash,
        };
        (actions, manifest)
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_naive(
    rng: &mut ChaCha20Rng,
    fleet_id: Uuid,
    agent: &Agent,
    p: &PersonaProfile,
    graph: &RelationshipGraph,
    start: DateTime<Utc>,
    end: DateTime<Utc>,
    seed: u64,
    out: &mut Vec<PlannedAction>,
) {
    let count =
        ((end - start).num_days().max(1) as f64 * p.weekday_sessions_mean * 2.0).round() as usize;
    for ordinal in 0..count {
        let seconds = rng.random_range(0..(end - start).num_seconds().max(1));
        push_action(
            rng,
            fleet_id,
            agent,
            p,
            graph,
            start + Duration::seconds(seconds),
            seed,
            ordinal,
            PlannerModel::NaiveUniform,
            None,
            None,
            out,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_independent(
    rng: &mut ChaCha20Rng,
    fleet_id: Uuid,
    agent: &Agent,
    p: &PersonaProfile,
    graph: &RelationshipGraph,
    start: DateTime<Utc>,
    days: u32,
    seed: u64,
    out: &mut Vec<PlannedAction>,
) {
    let normal =
        Normal::new(p.utc_active_hour_mean, p.utc_active_hour_stddev).expect("valid persona");
    let mut ordinal = 0;
    for day in 0..days {
        let date = start + Duration::days(i64::from(day));
        let lambda = p.weekday_sessions_mean
            * if date.weekday().number_from_monday() >= 6 {
                p.weekend_multiplier
            } else {
                1.0
            };
        let count = Poisson::new(lambda.max(0.01))
            .expect("positive lambda")
            .sample(rng) as usize;
        for _ in 0..count.saturating_mul(2) {
            let hour = normal.sample(rng).rem_euclid(24.0);
            let at = day_start(date) + Duration::seconds((hour * 3600.0) as i64);
            push_action(
                rng,
                fleet_id,
                agent,
                p,
                graph,
                at,
                seed,
                ordinal,
                PlannerModel::IndependentWeighted,
                None,
                None,
                out,
            );
            ordinal += 1;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_sessions(
    rng: &mut ChaCha20Rng,
    fleet_id: Uuid,
    agent: &Agent,
    p: &PersonaProfile,
    graph: &RelationshipGraph,
    start: DateTime<Utc>,
    days: u32,
    seed: u64,
    out: &mut Vec<PlannedAction>,
) {
    let active_hour =
        Normal::new(p.utc_active_hour_mean, p.utc_active_hour_stddev).expect("valid persona");
    let session_length = LogNormal::new(p.session_minutes_mean.ln(), 0.42).expect("valid persona");
    let inter_delay =
        LogNormal::new(p.inter_action_seconds_median.ln(), 0.75).expect("valid persona");
    let mut ordinal = 0;
    let mut weekly_spend = 0_u64;
    for day in 0..days {
        if day % 7 == 0 {
            weekly_spend = 0;
        }
        let date = start + Duration::days(i64::from(day));
        let mut daily_spend = 0_u64;
        let mut daily_actions = 0_u32;
        let lambda = p.weekday_sessions_mean
            * if date.weekday().number_from_monday() >= 6 {
                p.weekend_multiplier
            } else {
                1.0
            };
        let session_count = Poisson::new(lambda.max(0.005))
            .expect("positive lambda")
            .sample(rng) as usize;
        for session_index in 0..session_count {
            if daily_actions >= agent.actions_per_day {
                break;
            }
            let session_id = deterministic_uuid(
                seed ^ u64::from(day),
                ordinal as u64 ^ session_index as u64,
                agent.id.as_bytes(),
            );
            let hour = active_hour.sample(rng).rem_euclid(24.0);
            let session_start = day_start(date) + Duration::seconds((hour * 3600.0) as i64);
            let end = session_start + Duration::seconds((session_length.sample(rng) * 60.0) as i64);
            let mut at = session_start;
            let primary_protocol = weighted_protocol(&p.preferred_protocols, rng.random());
            let max_session_actions = (3.0 + p.activity_intensity * 15.0).round() as u32;
            let mut session_actions = 0_u32;
            while at < end
                && session_actions < max_session_actions.max(2)
                && daily_actions < agent.actions_per_day
                && ordinal < (days as usize).saturating_mul(200)
            {
                let before = out.len();
                let preferred_protocol = rng.random_bool(0.68).then_some(primary_protocol.as_str());
                push_action(
                    rng,
                    fleet_id,
                    agent,
                    p,
                    graph,
                    at,
                    seed,
                    ordinal,
                    PlannerModel::PersonaSession,
                    Some(session_id),
                    preferred_protocol,
                    out,
                );
                ordinal += 1;
                if let Some(action) = out.get(before) {
                    let next_daily = daily_spend.saturating_add(action.amount_lamports);
                    let next_weekly = weekly_spend.saturating_add(action.amount_lamports);
                    if next_daily > p.max_daily_spend_lamports
                        || next_weekly > p.max_weekly_spend_lamports
                    {
                        out.pop();
                        break;
                    }
                    daily_spend = next_daily;
                    weekly_spend = next_weekly;
                    daily_actions += 1;
                    session_actions += 1;
                }
                at += Duration::seconds(inter_delay.sample(rng).clamp(8.0, 7200.0) as i64);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_action(
    rng: &mut ChaCha20Rng,
    fleet_id: Uuid,
    agent: &Agent,
    p: &PersonaProfile,
    graph: &RelationshipGraph,
    at: DateTime<Utc>,
    seed: u64,
    ordinal: usize,
    model: PlannerModel,
    session_id: Option<Uuid>,
    preferred_adapter: Option<&str>,
    out: &mut Vec<PlannedAction>,
) {
    let adapter = preferred_adapter
        .map(str::to_owned)
        .unwrap_or_else(|| weighted_protocol(&p.preferred_protocols, rng.random()));
    let mut kind = match adapter.as_str() {
        "native-sol" => ActionKind::NativeTransfer,
        "spl-token" => ActionKind::SplTokenTransfer,
        "native-stake" => ActionKind::StakeCreate,
        _ => ActionKind::Memo,
    };
    if rng.random_bool(p.consolidation_probability) {
        kind = ActionKind::Consolidate;
    }
    if rng.random_bool(p.rare_event_probability) && matches!(kind, ActionKind::StakeCreate) {
        kind = ActionKind::StakeDeactivate;
    }
    let amount = if matches!(kind, ActionKind::Memo | ActionKind::Browse) {
        0
    } else {
        let low = p.min_value_lamports as f64;
        let high = p.max_value_lamports as f64;
        let sampled = LogNormal::new(((low + high) / 4.0).ln(), 0.8)
            .expect("valid values")
            .sample(rng)
            .clamp(low, high);
        sampled as u64
    };
    let peers = graph.neighbors(agent.id);
    let mut counterparty = if !peers.is_empty() && rng.random_bool(p.peer_interaction_probability) {
        Some(peers[rng.random_range(0..peers.len())])
    } else {
        None
    };
    if counterparty.is_none()
        && !peers.is_empty()
        && matches!(
            kind,
            ActionKind::NativeTransfer | ActionKind::SplTokenTransfer | ActionKind::Consolidate
        )
    {
        counterparty = Some(peers[rng.random_range(0..peers.len())]);
    }
    let id = deterministic_uuid(seed ^ 0xa11ce, ordinal as u64, agent.id.as_bytes());
    let idempotency_key = hash_parts(&[
        fleet_id.as_bytes(),
        agent.id.as_bytes(),
        id.as_bytes(),
        &at.timestamp_millis().to_le_bytes(),
    ]);
    out.push(PlannedAction {
        id,
        fleet_id,
        agent_id: agent.id,
        scheduled_at: at,
        kind,
        adapter_id: kind.adapter_id().into(),
        amount_lamports: amount,
        counterparty,
        asset: p
            .preferred_assets
            .get(rng.random_range(0..p.preferred_assets.len()))
            .cloned()
            .unwrap_or_else(|| "SOL".into()),
        state: ExecutionState::Planned,
        idempotency_key,
        model,
        seed_tag: seed_tag(seed),
        session_id,
    });
}

fn choose_persona(mix: &BTreeMap<PersonaKind, f64>, roll: f64) -> PersonaKind {
    let total = mix.values().sum::<f64>().max(f64::EPSILON);
    let mut cursor = roll * total;
    for (kind, weight) in mix {
        cursor -= weight.max(0.0);
        if cursor <= 0.0 {
            return *kind;
        }
    }
    mix.keys()
        .next_back()
        .copied()
        .unwrap_or(PersonaKind::CasualHolder)
}

fn weighted_protocol(weights: &BTreeMap<String, f64>, roll: f64) -> String {
    let total = weights.values().sum::<f64>().max(f64::EPSILON);
    let mut cursor = roll * total;
    for (protocol, weight) in weights {
        cursor -= weight;
        if cursor <= 0.0 {
            return protocol.clone();
        }
    }
    weights
        .keys()
        .next_back()
        .cloned()
        .unwrap_or_else(|| "memo".into())
}

fn day_start(value: DateTime<Utc>) -> DateTime<Utc> {
    value
        - Duration::hours(i64::from(value.hour()))
        - Duration::minutes(i64::from(value.minute()))
        - Duration::seconds(i64::from(value.second()))
        - Duration::nanoseconds(i64::from(value.nanosecond()))
}

pub fn deterministic_uuid(seed: u64, ordinal: u64, domain: &[u8]) -> Uuid {
    let digest = Sha256::digest(
        [
            seed.to_le_bytes().as_slice(),
            &ordinal.to_le_bytes(),
            domain,
        ]
        .concat(),
    );
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

pub fn seed_tag(seed: u64) -> String {
    hash_parts(&[&seed.to_le_bytes()])[..16].into()
}

pub fn trace_hash(actions: &[PlannedAction]) -> String {
    let mut hasher = Sha256::new();
    for action in actions {
        hasher.update(action.id.as_bytes());
        hasher.update(action.agent_id.as_bytes());
        hasher.update(action.scheduled_at.timestamp_millis().to_le_bytes());
        hasher.update(action.adapter_id.as_bytes());
        hasher.update(action.amount_lamports.to_le_bytes());
        hasher.update(action.idempotency_key.as_bytes());
    }
    hex_string(&hasher.finalize())
}

fn hash_parts(parts: &[&[u8]]) -> String {
    let mut h = Sha256::new();
    for p in parts {
        h.update(p);
    }
    hex_string(&h.finalize())
}

fn hex_string(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphConfig, RelationshipGraph};

    fn fixture(seed: u64) -> (Planner, Uuid, Vec<Agent>, RelationshipGraph) {
        let planner = Planner::new(default_personas()).unwrap();
        let fleet = deterministic_uuid(seed, 0, b"fleet");
        let mix = PersonaKind::ALL.into_iter().map(|k| (k, 1.0)).collect();
        let agents = planner.create_fleet(fleet, 40, seed, &mix, Utc::now());
        let graph = RelationshipGraph::generate(&agents, seed, &GraphConfig::default()).unwrap();
        (planner, fleet, agents, graph)
    }

    #[test]
    fn same_seed_has_same_trace() {
        let start = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        let (planner, fleet, agents, graph) = fixture(42);
        let (_, a) = planner.plan(
            fleet,
            &agents,
            &graph,
            start,
            5,
            42,
            PlannerModel::PersonaSession,
        );
        let (_, b) = planner.plan(
            fleet,
            &agents,
            &graph,
            start,
            5,
            42,
            PlannerModel::PersonaSession,
        );
        assert_eq!(a.trace_hash, b.trace_hash);
    }

    #[test]
    fn different_seeds_diverge() {
        let start = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        let (planner, fleet, agents, graph) = fixture(42);
        let (_, a) = planner.plan(
            fleet,
            &agents,
            &graph,
            start,
            3,
            42,
            PlannerModel::PersonaSession,
        );
        let (_, b) = planner.plan(
            fleet,
            &agents,
            &graph,
            start,
            3,
            43,
            PlannerModel::PersonaSession,
        );
        assert_ne!(a.trace_hash, b.trace_hash);
    }

    #[test]
    fn shipped_personas_parse_and_validate() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        for name in [
            "casual-holder",
            "active-trader",
            "staking-oriented",
            "token-explorer",
            "low-frequency-long-term",
        ] {
            let text = std::fs::read_to_string(root.join(format!("examples/personas/{name}.toml")))
                .unwrap();
            let persona: PersonaProfile = toml::from_str(&text).unwrap();
            persona.validate().unwrap();
        }
    }

    #[test]
    fn session_plans_respect_per_agent_daily_caps() {
        let start = DateTime::from_timestamp(1_767_225_600, 0).unwrap();
        let (planner, fleet, agents, graph) = fixture(77);
        let (actions, _) = planner.plan(
            fleet,
            &agents,
            &graph,
            start,
            14,
            77,
            PlannerModel::PersonaSession,
        );
        let by_id: BTreeMap<_, _> = agents.iter().map(|agent| (agent.id, agent)).collect();
        let mut daily: BTreeMap<_, (u64, u32)> = BTreeMap::new();
        for action in &actions {
            let totals = daily
                .entry((action.agent_id, action.scheduled_at.date_naive()))
                .or_default();
            totals.0 = totals.0.saturating_add(action.amount_lamports);
            totals.1 += 1;
        }
        for ((agent_id, _), (spend, count)) in daily {
            let agent = by_id[&agent_id];
            assert!(spend <= agent.daily_budget_lamports);
            assert!(count <= agent.actions_per_day);
        }
    }
}
