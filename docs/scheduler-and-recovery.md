# Scheduler and recovery

The scheduler claims an indexed, persisted due-action batch under an immediate SQLite transaction. A windowed query selects at most one due action per agent and excludes agents with an unexpired lease, including across processes. A semaphore bounds active workers; it never creates a sleeping task for every agent.

Normal states are `planned -> leased -> simulated -> submitted -> signature_recorded -> confirmed`. Exceptional states are `rejected`, `failed_before_submission`, `unknown_outcome`, `reconciliation_required`, `exhausted`, and `cancelled`.

Safety ordering:

1. Persist the plan and idempotency key.
2. Claim a lease transactionally.
3. Validate pre-simulation policy.
4. Estimate costs and build allowlisted instructions.
5. Sign and simulate the exact transaction.
6. Persist `simulated`.
7. Revalidate policy with simulation evidence.
8. Atomically reserve agent/fleet spend and agent/fleet/protocol rate capacity in SQLite.
9. Persist a submission intent and commit the reservation before the network call.
10. Submit the exact cached signed bytes, persist the locally known signature even if the RPC response is lost, and confirm.
11. Classify response loss as unknown and reconcile by signature without calling submit.

Expired `leased` and `simulated` checkpoints are safe to return to `planned`; both are provably pre-submit. Expired `submitted`, `signature_recorded`, and `unknown_outcome` checkpoints move only to `reconciliation_required`. Confirmed reconciliation commits the budget reservation; a finalized transaction error releases it. An outcome with no signature remains manual and is never resubmitted. Solana/RPC ambiguity cannot be eliminated, but the transaction signature is known from the signed bytes before the RPC call, which closes the ordinary lost-response gap.
