# Security policy

Please report vulnerabilities privately to the repository maintainers through GitHub Security Advisories. Include affected version, impact, minimal reproduction, and any suggested mitigation. Do not include funded keys, live secrets, or third-party personal data.

Maintainers should acknowledge a complete report within seven days, coordinate a fix and disclosure window, and credit reporters who want attribution. Public issues are appropriate for non-sensitive hardening ideas only.

No security audit has been completed. The project is not represented as mainnet production-ready. Its normal CLI is planning-only and its transaction transport refuses non-loopback RPC hosts.

High-priority areas are execution-policy bypass, duplicate submission after recovery, signature reconciliation, encrypted-keystore authentication, log/evidence secret leakage, SQLite state corruption, and adapter program-ID mismatch.
