ALTER TABLE budget_reservations ADD COLUMN adapter_id TEXT NOT NULL DEFAULT '';

CREATE INDEX IF NOT EXISTS budget_reservations_protocol_hour
    ON budget_reservations(fleet_id, adapter_id, utc_hour, status);
