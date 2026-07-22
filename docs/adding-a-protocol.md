# Adding a protocol

Implement `account_cooker_core::ProtocolAdapter` in the protocols crate or a downstream crate.

The adapter must provide a stable identifier, configuration validation, supported action types, every required program ID, a conservative lamport estimate, typed instruction construction, expected state changes, and a safety classification. It receives a bounded `PlannedAction` and explicit account context—not arbitrary serialized instructions.

Register the boxed adapter in `AdapterRegistry`. No scheduler code changes are needed. Then:

1. Add the adapter ID, actions, and program IDs to an explicit configuration allowlist.
2. Test invalid configuration, insufficient balance, program declarations, transaction construction, estimates, simulation failure, and expected changes.
3. Add a pinned loopback fixture if program state is required.
4. Extend the evaluator’s protocol categorization if the action introduces a new observable feature.
5. Document costs, safety constraints, reconciliation semantics, and fixture provenance.

`ExampleReadOnlyAdapter` demonstrates the minimum shape. It reuses the safe memo path and does not grant arbitrary transaction execution.
