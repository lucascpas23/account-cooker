CREATE TABLE IF NOT EXISTS budget_reservations (
    action_id TEXT PRIMARY KEY REFERENCES planned_actions(id) ON DELETE RESTRICT,
    fleet_id TEXT NOT NULL REFERENCES fleets(id) ON DELETE RESTRICT,
    agent_id TEXT NOT NULL REFERENCES agents(id) ON DELETE RESTRICT,
    utc_day TEXT NOT NULL,
    utc_hour TEXT NOT NULL,
    lamports INTEGER NOT NULL CHECK (lamports >= 0),
    status TEXT NOT NULL CHECK (status IN ('reserved', 'committed', 'released')),
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS budget_reservations_agent_day
    ON budget_reservations(agent_id, utc_day, status);
CREATE INDEX IF NOT EXISTS budget_reservations_fleet_day
    ON budget_reservations(fleet_id, utc_day, status);
CREATE INDEX IF NOT EXISTS budget_reservations_agent_hour
    ON budget_reservations(agent_id, utc_hour, status);
