# Architecture

## Design goals

The design optimizes for weeks of persisted work across thousands of agents, deterministic research, fail-closed transaction policy, and adapters that cannot bypass the scheduler. It intentionally does not map one agent to one operating-system task.

## Components

| Crate | Responsibility |
|---|---|
| `account-cooker-core` | Domain states, personas, three planners, graph generation, adapter contract, policy |
| `account-cooker-config` | Strict schema v1 configuration and safe defaults |
| `account-cooker-store` | SQLite WAL migrations, fleets, actions, leases, atomic budget/rate reservations, transitions, evidence events |
| `account-cooker-protocols` | SOL, memo, SPL token, stake, and example adapter implementations |
| `account-cooker-scheduler` | Bounded claims/workers, pipeline states, simulation and loopback transport |
| `account-cooker-evaluator` | Synthetic observer, clustering/classification metrics, ablation, longitudinal analysis |
| `account-cooker-keystore` | Age-encrypted local bytes and signer abstraction |
| `account-cooker-cli` | Operator commands and sanitized evidence |
| `xtask` | Reproducible setup, demo, full demo, cleanup, and verification |

The recommended tree was consolidated from ten product crates to eight because core policy, planning, and graph types share invariants, while the Solana instruction builders belong together. No empty crate exists only to match a diagram.

## Data and execution flow

1. Strict TOML validation constructs explicit limits and allowlists.
2. A seeded ChaCha20 planner creates agents, a relationship graph, sessions, and versioned actions.
3. Actions are inserted transactionally with unique idempotency keys.
4. A worker process claims an ordered batch under an expiring lease.
5. The central policy validates host, action, program, reserve, and pause state.
6. SQLite atomically reserves action, agent/fleet spend, agent/fleet rate, and per-protocol rate capacity using the actual execution hour/day.
7. The selected adapter builds typed instructions and expected changes.
8. The transaction is signed locally; its signature is known before RPC, and its exact serialized bytes are cached after successful simulation.
9. A durable submission intent is written before those exact bytes are sent.
10. Confirmation commits the reservation. A finalized failure releases it. Lost or ambiguous responses retain the reservation and move to reconciliation.
11. On restart, pre-submit work may return to planned; anything that could have reached the validator is queried by signature and is never automatically resubmitted.

Virtual time bypasses RPC entirely and produces deterministic trace hashes. Local execution is bounded and requires loopback. The CLI has no public-cluster execution path. Acceptance embeds the official pinned Surfpool SDK in offline mode, so it does not depend on a separately installed binary.

## Scale

Planner memory is proportional to emitted actions, not virtual duration ticks. Scheduling reads an indexed due batch and creates at most `worker_concurrency` active tasks. SQLite connections use WAL, foreign keys, a busy timeout, immediate write transactions, and an index on `(state, scheduled_at, lease_expires_at)`.
