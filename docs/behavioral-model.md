# Behavioral model

The `persona-session` planner is hierarchical rather than uniformly random:

- Daily session counts use a Poisson distribution because sparse event counts vary without a fixed cadence.
- Active hours use a wrapped normal distribution around a persona-specific UTC preference.
- Session duration and inter-action gaps use log-normal distributions because human activity is positive, skewed, and occasionally much longer than its median.
- Amounts use bounded log-normal draws to avoid identical values while honoring hard budgets.
- Protocols use persona-specific categorical weights with bounded within-session affinity, so a session has continuity without collapsing into a single protocol.
- Weekends apply persona-specific multipliers.
- Rare events and consolidation are separately bounded Bernoulli events.
- Peer selection comes from recurring strong/weak graph relationships rather than a complete graph or round robin.
- Per-session action caps, per-agent daily action caps, and daily plus rolling-week spend caps bound heavy-tail draws.

The five examples are `casual-holder`, `active-trader`, `staking-oriented`, `token-explorer`, and `low-frequency-long-term`. They differ in session frequency, hour spread, value range, account age, protocol mix, and risk—not merely keypairs.

`naive-uniform` samples timestamps uniformly across the whole window. `independent-weighted` uses time and protocol weights without explicit sessions. Both are retained as adversarial baselines.

The distributions are synthetic assumptions, not measurements of real people. Funding graphs remain public. Repeated fee payers, operator infrastructure, protocol availability, long observation windows, and a mismatch between synthetic personas and real ecosystem traffic remain detectable. Explicit seeds provide reproducible research. When the CLI seed is omitted it draws a seed from Rust's operating-system-seeded CSPRNG, prints it, and persists enough provenance to replay the plan. Ephemeral transaction signing also uses operating-system entropy.
