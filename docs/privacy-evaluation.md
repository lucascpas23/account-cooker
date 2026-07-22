# Privacy evaluation

The evaluator defines a passive synthetic chain observer with timestamps, amounts, program categories, counterparties, sessions inferred from gaps over 30 minutes, funding relationships, consolidation, and account age. It never reads planner-private session IDs. It has no private keys, off-chain identity oracle, proprietary labeling, mempool feed, or RPC-provider metadata.

For every planner it emits dataset parameters, seed, model version, feature set, action and agent counts, trace hash, ARI, NMI, ROC AUC, precision, recall, and F1. Classification centroids are trained and scored with deterministic five-fold agent-level cross-validation. It removes timing, funding graph, amounts, sequence, and consolidation features one group at a time and records the AUC delta. It repeats clustering/classification over 1, 7, 14, and 30-day windows.

Interpretation is intentionally conservative:

- ROC AUC near 0.5 means this observer cannot distinguish a planner from the declared baseline; higher separability is worse.
- High ARI/NMI means persona clusters are recovered; lower scores are better only relative to this observer.
- Low scores never prove anonymity.
- Synthetic baselines are not organic Solana traffic.
- Unfavorable results are part of the evidence and must not be suppressed.

The CSV is convenient for comparison, while JSON is the canonical structured artifact and Markdown is a human summary.
