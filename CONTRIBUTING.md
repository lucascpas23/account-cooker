# Contributing

Contributions must remain MIT-compatible, Rust-first, safe by default, and factual about privacy. Do not include private keys, databases, generated wallets, raw operator logs, copied submissions, or unpinned third-party fixtures.

Before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
cargo test --doc --workspace
cargo xtask demo
git diff --check
```

Protocol changes must follow `docs/adding-a-protocol.md`. State-machine or migration changes need crash/recovery tests. Planner changes need same-seed reproducibility, different-seed divergence, scale evidence, and an evaluator comparison that retains unfavorable results.
