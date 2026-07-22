# Threat model

## Observers

- A passive chain analyst sees the complete public ledger and can cluster funding sources, timings, amounts, protocol sequences, counterparties, fee payers, consolidation, transaction shapes, account ages, and long-term changes.
- An RPC observer additionally sees source network metadata, request order, simulations, failed intents, and operator cadence.
- An active attacker may influence prices, program availability, responses, or counterparties; transaction simulation is not protection against every state change between simulation and landing.
- Operator or local-key compromise reveals control directly. Behavioral noise cannot repair key theft.

## Intended improvement

The project makes simplistic deterministic cadence, identical protocol ordering, uniform values, complete graphs, one-shot funding, and synchronized fleet behavior less reliable signals. It creates varied persistent personas and evaluates whether a declared synthetic observer can still classify or cluster them.

## Remaining leakage

Shared funding sources and fee payers can be decisive. Funding trees are permanently public. Amount fingerprints, rare protocol sequences, one controller, IP/RPC metadata, consolidation destinations, and weeks of observation can overpower timing variation. Dust activity can itself become a fingerprint. Local database, configuration, signer, or host compromise defeats the model.

This is not cryptographic anonymity, zero knowledge, unlinkability, mixing, or protection against every analytics platform. It does not hide fund flows and should not be described as doing so.
