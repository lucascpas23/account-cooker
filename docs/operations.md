# Operations

Run one supervised scheduler process per database. Keep configuration and database directories owner-readable, use external or age-encrypted signers, and never place passphrases in configuration. Use structured logs with redacted identifiers in shared environments.

Start with `init`, then `doctor`. `doctor` reports schema, RPC reachability, loopback status, installed tools, signer posture, and whether sending is actually allowed. Create and inspect a fleet before planning. Plan and evaluate offline. Run bounded dry cycles before any local transaction acceptance.

Operational controls:

- `pause` freezes one agent, one persona group, or the latest fleet without deleting work.
- `resume` returns the selected paused agents to dormant.
- `drain --confirm` prevents future lifecycle activity but never transfers assets implicitly.
- `reconcile` validates the loopback RPC identity, queries each persisted signature, records evidence, and refuses blind resubmission. Missing signatures remain manual.
- `report --json` exposes durable queue and outcome counts.
- `evidence` exports hashes and aggregates, excluding the database, keys, config contents, and raw logs.

Use SIGINT/SIGTERM supervision around bounded cycles. Keep RPC concurrency below validator capacity. Monitor scheduler lag, unknown outcomes, retries, reserved/committed budget use, fleet/protocol rate ceilings, database growth, and worker saturation. Back up the database consistently with SQLite’s backup API or while stopped; copying only the main file during WAL writes is unsafe.
