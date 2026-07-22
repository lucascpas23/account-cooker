CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS fleets (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    created_at TEXT NOT NULL,
    seed_tag TEXT NOT NULL,
    agent_count INTEGER NOT NULL CHECK (agent_count >= 0)
);

CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    fleet_id TEXT NOT NULL REFERENCES fleets(id) ON DELETE RESTRICT,
    public_key TEXT NOT NULL,
    signer_ref TEXT NOT NULL,
    persona TEXT NOT NULL,
    lifecycle TEXT NOT NULL,
    created_at TEXT NOT NULL,
    account_age_days INTEGER NOT NULL,
    daily_budget_lamports INTEGER NOT NULL,
    weekly_budget_lamports INTEGER NOT NULL,
    fee_reserve_lamports INTEGER NOT NULL,
    actions_per_hour INTEGER NOT NULL,
    actions_per_day INTEGER NOT NULL,
    next_action_at TEXT,
    failure_count INTEGER NOT NULL DEFAULT 0,
    health TEXT NOT NULL,
    deterministic_seed_tag TEXT
);
CREATE INDEX IF NOT EXISTS agents_fleet_lifecycle ON agents(fleet_id, lifecycle);

CREATE TABLE IF NOT EXISTS relationships (
    fleet_id TEXT NOT NULL REFERENCES fleets(id) ON DELETE RESTRICT,
    a TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    b TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    strength REAL NOT NULL CHECK (strength >= 0 AND strength <= 1),
    household INTEGER NOT NULL,
    protocol_affinity TEXT,
    PRIMARY KEY (fleet_id, a, b),
    CHECK (a <> b)
);

CREATE TABLE IF NOT EXISTS planned_actions (
    id TEXT PRIMARY KEY,
    fleet_id TEXT NOT NULL REFERENCES fleets(id) ON DELETE RESTRICT,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    scheduled_at TEXT NOT NULL,
    kind TEXT NOT NULL,
    adapter_id TEXT NOT NULL,
    amount_lamports INTEGER NOT NULL CHECK (amount_lamports >= 0),
    counterparty TEXT,
    asset TEXT NOT NULL,
    state TEXT NOT NULL,
    idempotency_key TEXT NOT NULL UNIQUE,
    planner_model TEXT NOT NULL,
    seed_tag TEXT NOT NULL,
    session_id TEXT,
    lease_owner TEXT,
    lease_expires_at TEXT,
    retry_count INTEGER NOT NULL DEFAULT 0,
    signature TEXT UNIQUE,
    last_error TEXT,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS planned_actions_due
    ON planned_actions(state, scheduled_at, lease_expires_at);
CREATE INDEX IF NOT EXISTS planned_actions_agent
    ON planned_actions(agent_id, state, scheduled_at);

CREATE TABLE IF NOT EXISTS transaction_attempts (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id TEXT NOT NULL REFERENCES planned_actions(id) ON DELETE RESTRICT,
    attempt INTEGER NOT NULL,
    phase TEXT NOT NULL,
    signature TEXT,
    result_category TEXT NOT NULL,
    created_at TEXT NOT NULL,
    UNIQUE(action_id, attempt, phase)
);

CREATE TABLE IF NOT EXISTS budgets (
    fleet_id TEXT NOT NULL REFERENCES fleets(id) ON DELETE RESTRICT,
    agent_id TEXT,
    utc_day TEXT NOT NULL,
    spent_lamports INTEGER NOT NULL DEFAULT 0,
    actions INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY(fleet_id, agent_id, utc_day)
);

CREATE TABLE IF NOT EXISTS worker_leases (
    worker_id TEXT PRIMARY KEY,
    heartbeat_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS reconciliation_records (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    action_id TEXT NOT NULL REFERENCES planned_actions(id) ON DELETE RESTRICT,
    previous_state TEXT NOT NULL,
    resolved_state TEXT NOT NULL,
    signature TEXT,
    evidence TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS configuration_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    fleet_id TEXT,
    sha256 TEXT NOT NULL,
    redacted_toml TEXT NOT NULL,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS immutable_events (
    sequence INTEGER PRIMARY KEY AUTOINCREMENT,
    fleet_id TEXT,
    agent_id TEXT,
    action_id TEXT,
    event_type TEXT NOT NULL,
    redacted_payload TEXT NOT NULL,
    created_at TEXT NOT NULL
);
