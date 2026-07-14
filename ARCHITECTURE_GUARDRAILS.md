# Architecture guardrails

This page explains where architecture drift is enforced. The scripts own the
exact checks and generated fields; architecture documents own the design
rationale.

## Contents

- [Source audit](#source-audit)
- [Regression evidence](#regression-evidence)
- [Gate selection](#gate-selection)

## Source audit

Run the canonical source audit:

```sh
scripts/check/source-audit.sh
```

[scripts/check/source-audit.sh](scripts/check/source-audit.sh) is authoritative
for the boundaries it rejects and the schema of `target/source-audit.json`.
Do not copy its current import patterns, file limits, approved environment-read
locations, schema fields, or generated inventories into this page. A change to
one of those rules belongs in the script and its tests or CI wiring.

The audit is deliberately mechanical. Passing it proves that the encoded
high-risk regressions are absent; it does not prove that every module boundary
is ideal. Current ownership guidance lives in
[Architecture](docs/architecture.md) and the
[layering audit skill](docs/agent-guidance/skills/layering-audit/SKILL.md).

## Regression evidence

[Regression artifacts](docs/regression-artifacts.md) maps historical failure
classes to durable fixtures, focused tests, harness workflows, or generated
artifact locations. Add evidence there when a new architecture guardrail is
introduced instead of embedding a changing replay inventory here.

## Gate selection

[AGENTS.md](AGENTS.md) owns the file-to-first-gate routing used by agents, and
[Testing](docs/testing.md) owns contributor-facing proof selection. This page
does not maintain a second gate table.

When an architecture boundary changes intentionally, update the executable
audit, its proof, and the relevant architecture contract in the same change.
