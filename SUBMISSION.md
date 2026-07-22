# Privacy-Through-Noise submission guide

`account-cooker` is a production-oriented Rust fleet manager and adversarial research harness for long-lived Solana activity agents. It combines believable hierarchical behavior, durable policy-caged scheduling, exact local transaction acceptance, and privacy evaluation that reports unfavorable results instead of claiming anonymity.

## What to review first

1. Read the architecture and safety summary in `README.md`.
2. Inspect the command-backed results and remaining risks in `AUDIT.md`.
3. Run `cargo xtask demo` for the bounded path or `cargo xtask full-demo` for the canonical 1,000-agent, 30-day acceptance.
4. Verify generated evidence with `cargo xtask verify-evidence`.

## Differentiators

- The scheduler does not create a sleeping task per wallet. SQLite stores indexed due work; bounded workers claim one action per agent under expiring leases.
- Budgets are not in-memory counters. SQLite atomically reserves agent/fleet spend and agent/fleet/protocol rate capacity using real execution periods, including under concurrent workers.
- The transaction signature is known before RPC. Successful simulation caches serialized signed bytes, and submission can send only that exact cached transaction.
- Ambiguous RPC outcomes are not retried. The acceptance harness injects response loss, destroys and recreates the scheduler over the same database, then reconciles the signature with exactly one submission.
- Surfpool 1.5.0 is pinned and embedded. The demo starts offline on dynamic loopback, confirms a real transaction and account delta, and requires no separately installed binary.
- The evaluator uses public features only, infers sessions from timestamps, uses five-fold agent-level holdouts, and compares three required planners across clustering, classification, ablation, and longitudinal views.
- Explicit seeds replay exactly; omitted CLI seeds come from an operating-system-seeded CSPRNG and are printed for provenance.

## Canonical evidence

The measured development-environment run planned 1,000 agents over 30 virtual days:

| Evidence | Result |
|---|---:|
| Persona-session actions | 159,721 |
| Primary deterministic trace | `dba0325230d56d0879144b7b621c4983417bfa172623680f656871c902e38461` |
| Same-seed replay | identical |
| Second-seed actions | 158,469 |
| Second deterministic trace | `ac6c919ec629bc910e6c24f86db496db2ac634090104a47e38210a61de7a3148` |
| Full workflow wall time | 200.50 s |
| Maximum resident set | 272,302,080 bytes |
| SQLite database | 114,610,176 bytes |
| Expired leases recovered | 7 / 7 |
| Local Surfpool submit / confirm | 1 / 1 |
| Ambiguous submit / post-restart reconciliation | 1 / 1 |
| Duplicate signatures | 0 |

Generated JSON, CSV, Markdown, and run metadata are SHA-256 verified. The generated directory is ignored because timings are machine-specific and because evidence should be regenerated rather than trusted as a static claim.

## Honest privacy result

The persona-session model is highly distinguishable from the naive-uniform baseline in the bundled observer (ROC AUC 0.995), and its persona clustering remains material (ARI 0.709). Sequence is the strongest ablation signal. This does not demonstrate anonymity; it demonstrates both a richer behavioral model and a remaining fingerprint that future work must reduce. The raw result, observer assumptions, and limitations are preserved in every output.

## Validation status

- Formatting and strict Clippy pass.
- 34 unit/integration tests and all doctests pass.
- Full release build passes.
- Embedded Surfpool acceptance and evidence verification pass.
- `cargo audit` and `cargo deny check` pass under explicit accepted-risk files.
- Six known vulnerabilities and additional maintenance warnings in Surfpool's pinned offline-only transitive graph are disclosed in `AUDIT.md`; this is not described as a clean dependency tree.

## Deliberate boundaries

The normal CLI cannot broadcast to a public cluster. There is no arbitrary-instruction adapter, no funded-wallet artifact, no secret material in evidence, and no claim that noise defeats chain analysis. SPL-token and stake adapters have typed construction tests, while the bounded live acceptance uses memo. Independent review and evaluation against organic Solana data remain future work.
