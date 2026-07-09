# Architecture Guardrails

This repository keeps architectural drift visible through CI and local scripts.
The guardrails are intentionally small and mechanical: they do not prove the
whole design is ideal, but they catch the highest-risk regressions early.

## Source Audit

Run:

```sh
./scripts/source-audit.sh
```

The audit enforces:

- `src/media/` must not import API modules.
- Large files may not grow beyond their current no-growth baselines:
  - `src/media/engine.rs`: 6587 lines
  - `src/bin/test_harness.rs`: 10282 lines
- Production code must not add raw `std::env::var` reads outside the approved
  config/startup/test-harness boundaries.

The script also writes `target/source-audit.json` with line counts, route-module
count, repository-module count, feature-cfg count, and forbidden-import counts.

## Related Gates

- API boundary changes: `./scripts/check-api-contract.sh`
- Concurrency and lifecycle changes: `./scripts/check-concurrency-proof-fast.sh`
  and `./scripts/check-concurrency-contract.sh`
- Fixture discipline: `./scripts/check-fixture-discipline.sh`
- Test hygiene: `./scripts/check-test-hygiene.sh`

When a phase intentionally changes ownership boundaries, update this file and
the relevant script in the same commit.
