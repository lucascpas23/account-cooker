# Evidence

Committed evidence is documentation only. Reproducible generated evidence belongs in ignored `demo-output/` or `evidence/generated/` and contains aggregate JSON/CSV/Markdown plus SHA-256 checksums—never the database, signer material, passphrases, raw logs, environment variables, or home-directory paths.

Generate and verify:

```bash
cargo xtask full-demo
cargo xtask verify-evidence
```

The `run.json` classification distinguishes planning, instruction simulation, local submission, local confirmation, ambiguous submission, and post-restart reconciliation. The demo embeds pinned Surfpool 1.5.0 in offline mode, submits one exact previously simulated signed transaction, verifies its signature and balance delta, then runs an injected lost-response scenario against a durable SQLite database to prove reconciliation without a second submission.
