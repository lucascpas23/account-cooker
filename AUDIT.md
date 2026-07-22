# Engineering audit

This report records commands and results observed in the development environment. It is not an independent security audit, a mainnet-readiness claim, or a guarantee of privacy.

## 1. Reviewed scope

The Rust workspace, migrations, example configurations and personas, CLI, behavioral planners, evaluator, durable scheduler and recovery behavior, protocol instruction builders, encrypted keystore primitives, documentation, CI, embedded Surfpool acceptance, and generated-evidence workflow.

## 2. Bounty requirement mapping

| Requirement | Implementation |
|---|---|
| Rust end to end | Eight Rust product crates plus Rust `xtask`; no Python or JavaScript runtime |
| Thousands for weeks | Virtual time, persisted indexed queue, batch claims, bounded workers, no per-agent sleeper |
| Manageable fleet | Init, doctor, create/inspect, plan/run, pause/resume/drain, reconcile, report/evidence |
| Extensible protocols | `ProtocolAdapter` contract and registry independent of scheduler |
| Realistic behavior | Five personas, hierarchical sessions, heavy-tail timing/value draws, protocol affinity, relationship graph, and hard daily/weekly/session caps |
| Measurable privacy | Three baselines, explicit public-feature observer, five-fold agent holdouts, ARI/NMI/AUC/precision/recall/F1, ablation, and longitudinal analysis |
| Durability | WAL, foreign keys, schema migrations, unique idempotency, leases, conservative checkpoints, immutable events, and atomic budget reservations |
| Safe local execution | Loopback-only transport, exact signed-byte simulation/submission, signature-first reconciliation, and embedded offline Surfpool acceptance |
| Open and documented | Preserved MIT license, README, architecture, operations, threat, protocol, dependency, and responsible-use docs |

## 3. Architecture and safety summary

Planning and evaluation require no Solana RPC. Execution claims due rows, applies the central policy, atomically reserves spend and rate capacity using actual execution time, calls an allowlisted adapter, signs locally, simulates and caches the exact serialized transaction, durably records submission intent, and sends only those bytes. Confirmation commits the reservation; a finalized failure releases it; ambiguity retains it for reconciliation.

Expired pre-submit work can return to `planned`. Any checkpoint that could have reached a validator moves to `reconciliation_required` and is queried by its locally known signature. It is never blindly resubmitted. The normal CLI has no public-cluster broadcast path.

## 4. Toolchain observed

- Rust: 1.97.1
- Cargo: 1.97.1
- Solana CLI: 4.1.1 (Agave)
- Surfpool SDK: exactly 1.5.0, embedded by `xtask`
- External Surfpool binary: not installed and not required
- SQLite: bundled through `rusqlite`

## 5. Commands executed

Passed on the final tree:

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets --all-features -- -D warnings`
- `cargo test --workspace --all-features`
- `cargo test --doc --workspace`
- `cargo build --workspace --all-features --release`
- `cargo xtask demo`
- `/usr/bin/time -l cargo xtask full-demo`
- `cargo xtask verify-evidence`
- `cargo audit` with the explicit accepted-risk list described below
- `cargo deny check` (`advisories ok, bans ok, licenses ok, sources ok`)

Preflight and final hygiene include repository/remote/branch inspection, tool version checks, `git diff --check`, generated-file checks, private-key/secret-pattern checks, home-directory-path checks, and verification that the original license was unchanged.

## 6. Test results

`cargo test --workspace --all-features` passed 34 tests; all workspace doctests passed. Coverage includes strict configuration and shipped examples, lifecycle/execution transitions, persona-group lifecycle controls, deterministic replay and seed divergence, persona validation, property-generated graphs, planner daily limits, evaluator determinism and classifier sanity bounds, encrypted-keystore confidentiality/wrong-passphrase/tamper handling, typed adapter construction, insufficient-balance rejection, public-RPC rejection, exact-simulation-cache enforcement, bounded scheduling, unique signatures, lost-response reconciliation without resubmission, migration idempotency, idempotency uniqueness, per-agent serialization, expired-lease recovery, concurrent no-overspend reservations, durable per-protocol rate limits, and pre-/post-submit crash checkpoints.

## 7. Canonical scale result

Seed `181141552` planned 1,000 agents for 30 virtual days. `persona-session` emitted 159,721 actions with trace `dba0325230d56d0879144b7b621c4983417bfa172623680f656871c902e38461`. A fresh same-seed replay produced the identical count and hash. A second seed, `6840227784729600302`, produced 158,469 actions and the different trace `ac6c919ec629bc910e6c24f86db496db2ac634090104a47e38210a61de7a3148`.

Primary planning took 24,219 ms and the SQLite database was 114,610,176 bytes. The complete workflow took 200.50 seconds wall time, with a 272,302,080-byte maximum resident set and 242,927,680-byte macOS peak footprint. This measurement includes all baseline datasets, evaluation, replay, second seed, persistence, crash scenarios, Surfpool startup/acceptance, and evidence generation; it is not a planner-only benchmark.

## 8. Scheduler, budgets, and crash recovery

The canonical run injected seven expired leases and recovered all seven, then instruction-simulated a bounded batch of 64. Durable SQLite reservations recheck per-action spend, agent/fleet daily spend, agent daily/hourly counts, fleet hourly counts, and per-protocol hourly counts under `BEGIN IMMEDIATE`. The periods come from actual execution time, so old virtual schedules cannot bypass current limits.

A separate injected response-loss scenario persisted an ambiguous result, destroyed the scheduler, opened the same database with a new scheduler, reconciled the known signature, and observed one submission and one reconciliation with zero duplicate signatures.

## 9. Embedded Surfpool acceptance

`xtask` started the official pinned Surfpool SDK as an offline Surfnet on a dynamic loopback port. It created an ephemeral in-memory signer funded with 1,000,000,000 lamports, signed a memo transaction locally, simulated the serialized bytes, submitted those exact cached bytes once, confirmed the locally known signature, and observed a final balance of 999,995,000 lamports. The 5,000-lamport delta is the transaction fee. Final evidence records one local submission and one local confirmation.

## 10. Policy and adapters

Fail-closed defaults, public-send rejection, required simulation, strict loopback detection, program/action allowlists, atomic spend/rate reservation, pause state, and insufficient-balance rejection are exercised. Native SOL, memo, checked SPL token with idempotent associated-token-account creation, native stake lifecycle, and a read-only example adapter use typed instruction builders. The scheduler depends only on `ProtocolAdapter`.

## 11. Adversarial privacy evaluation

The observer uses only declared public-chain features; sessions are inferred from timestamp gaps rather than planner-private session IDs. Classification uses deterministic five-fold agent-level out-of-sample evaluation.

| Planner | Actions | Active agents | ARI | NMI | ROC AUC | Precision | Recall | F1 |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| naive-uniform | 59,526 | 1,000 | 0.715 | 0.800 | 0.500 | 0.500 | 1.000 | 0.667 |
| independent-weighted | 59,304 | 980 | 0.910 | 0.907 | 0.954 | 0.959 | 0.803 | 0.874 |
| persona-session | 159,721 | 989 | 0.709 | 0.829 | 0.995 | 1.000 | 0.891 | 0.942 |

For `persona-session`, removing sequence features reduced AUC by 0.114 and removing timing reduced it by 0.012. Removing funding-graph, amount, or consolidation features did not materially improve this observer. Persona-session AUC was already 0.994 at one observed day and approximately 0.995 at 30 days.

These are unfavorable results: the generated persona traffic is highly distinguishable from the declared naive baseline, and persona clusters remain recoverable. The metrics are retained rather than optimized away. Synthetic scores neither establish anonymity nor represent every real analyst or organic Solana traffic.

## 12. Supply-chain findings

`cargo audit` exits successfully only because six known vulnerabilities in transitive dependencies of the pinned Surfpool 1.5.0 offline acceptance harness are explicitly accepted in `.cargo/audit.toml`:

- `RUSTSEC-2022-0093` (`ed25519-dalek` 1.0.1)
- `RUSTSEC-2024-0344` (`curve25519-dalek` 3.2.0)
- `RUSTSEC-2024-0421` (`idna` 0.1.5)
- `RUSTSEC-2026-0098`, `RUSTSEC-2026-0099`, and `RUSTSEC-2026-0104` (`rustls-webpki` 0.101.7)

The product crates do not depend on Surfpool. The acceptance harness runs offline on loopback, which narrows but does not erase the risk. `cargo audit` also reports 14 allowed unmaintained/unsound warnings, including `bincode`, `proc-macro-error2`, and legacy dependencies within the Surfpool/Agave graph. `deny.toml` enumerates acknowledged advisories; any new advisory remains a failure. `cargo deny check` passed, with one upstream metadata warning because `solana-config-interface 2.0.0` has no license field.

## 13. Evidence and secret hygiene

`cargo xtask verify-evidence` verified four SHA-256 entries: evaluation JSON, CSV, Markdown, and run JSON. Generated evidence contains aggregates and hashes, not the SQLite database, configuration contents, keys, passphrases, environment variables, raw logs, or machine-specific paths. The generated directory is intentionally ignored because timing and resource measurements are environment-specific.

## 14. Remaining risks and untested paths

- The privacy model is synthetic and currently easy for its own observer to distinguish.
- The encrypted keystore primitives are not yet wired to CLI signer selection.
- Real SPL-token and stake state fixtures are not exercised against Surfpool; their typed instruction construction is unit tested, while the bounded live acceptance uses memo.
- Submitted work lacking a persisted signature cannot be safely automated and remains a manual reconciliation case.
- Running multiple scheduler processes assumes reasonably bounded database-host clock drift.
- The pinned offline Surfpool dependency graph has the acknowledged advisories above.
- Public-cluster execution is intentionally absent.
- No independent security review or organic-data privacy evaluation has occurred.

## 15. Reproduction

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace
cargo build --workspace --all-features --release
cargo audit
cargo deny check
cargo xtask full-demo
cargo xtask verify-evidence
```

No external Surfpool process is needed. The first `xtask` build is large because it compiles the complete pinned Surfpool/Agave runtime.

## 16. Final checklist

- [x] Working, tested Rust core
- [x] Safe defaults and central policy cage
- [x] Atomic durable budgets and rates
- [x] Bounded scheduler, checkpoint-aware recovery, and no-blind-retry reconciliation
- [x] Trait-based adapters and typed instruction construction
- [x] Public-feature adversarial evaluator and multi-seed evidence
- [x] Encrypted-keystore primitives and tamper tests
- [x] Embedded Surfpool exact-transaction confirmation and account delta
- [x] Reproducible checksums and explicit dependency risk acceptance
- [ ] Independent security review
- [ ] Real-organic-data privacy evaluation
