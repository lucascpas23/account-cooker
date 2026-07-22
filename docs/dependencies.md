# Dependency compatibility

The committed lockfile is authoritative. The workspace uses Rust 1.97.1 and the split official Anza/Solana crates matching Surfpool 1.5.0’s generation: `solana-instruction` 3.x, `solana-keypair` 3.x, `solana-message` 4.x, `solana-transaction` 4.x, and current SPL interface crates. `solana-transaction` enables its official serde and signing support for canonical wire serialization.

Surfpool SDK 1.5.0 is pinned as an `xtask`-only dependency. Its published dependency metadata uses Agave/Solana 4.x runtime and client components with the same split SDK generation. The local Solana CLI detected during development was 4.1.1; no external Surfpool binary was installed or required. `xtask` starts the official SDK offline on dynamic loopback ports.

The embedded Surfpool 1.5.0 graph currently carries six RustSec vulnerabilities and several unmaintained/unsound warnings in pinned transitive dependencies. They are explicitly enumerated in `.cargo/audit.toml` and `deny.toml`, apply to the offline acceptance harness, and are not silently described as clean. Both tools still fail on any newly introduced advisory. See `AUDIT.md` for IDs and scope.

`rusqlite` uses bundled SQLite for reproducibility. `age` provides scrypt/passphrase authenticated encryption; no custom cryptography is implemented. ChaCha20 is deterministic only for tests and virtual research. Production signer generation uses Solana’s keypair implementation and operating-system randomness.

Dependency versions are resolved and pinned by `Cargo.lock`. CI runs `cargo deny` and `cargo audit` when their tools are available.
