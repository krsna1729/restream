# Architecture Guardrails

This repository keeps architectural drift visible through CI and local scripts.
The guardrails are intentionally small and mechanical: they do not prove the
whole design is ideal, but they catch the highest-risk regressions early.

## Contents

- [Source Audit](#source-audit)
- [Regression Artifacts](#regression-artifacts)
- [Related Gates](#related-gates)

## Source Audit

Run:

```sh
./scripts/check/source-audit.sh
```

The audit enforces:

- `src/media/` must not import API modules.
- Audited Rust, authored TypeScript, and hand-written JavaScript test files may
  not exceed 2,000 lines.
- Production code must not add raw `std::env::var` reads outside the approved
  config/startup/test-harness boundaries.
- API modules must not start FFmpeg/transcoder stages directly.
- Harness code must not read the removed output-status `state` field.

The script also writes `target/source-audit.json` with line counts, public
function and route inventories, harness modes/suites, environment-variable
usage, feature gates, forbidden imports, and output-status schema drift.

## Regression Artifacts

Historical failure classes that drove the architecture phases are indexed in
[`docs/regression-artifacts.md`](docs/regression-artifacts.md). The index links
each failure class to a checked-in fixture, harness mode, proof gate, or
documented generated-artifact location.

## Related Gates

- API boundary changes: `./scripts/check/api-contract.sh`
- Concurrency and lifecycle changes: `./scripts/check/concurrency/fast.sh`
  and `./scripts/check/concurrency/contract.sh`
- Fixture discipline: `./scripts/check/fixture-discipline.sh`
- Test hygiene: `./scripts/check/test-hygiene.sh`

When a phase intentionally changes ownership boundaries, update this file and
the relevant script in the same commit.
