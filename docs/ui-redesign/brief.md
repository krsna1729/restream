# UI Redesign Baseline

## Contents

- [Purpose](#purpose)
- [Active contracts](#active-contracts)
- [Acceptance boundary](#acceptance-boundary)

## Purpose

Preserve operator workflows and runtime semantics while the dashboard evolves.
Framework and visual choices may change; these contracts must not silently
drift.

## Active contracts

- [`operator-task-model.md`](operator-task-model.md) — workflows an operator
  must finish;
- [`state-matrix.yaml`](state-matrix.yaml) — important states and proof status;
- [`route-contract.md`](route-contract.md) — URL ownership and compatibility;
- [`migration-map.md`](migration-map.md) — ownership stop rules and build
  artifact paths that remain in force;
- [`visual-accessibility-baseline.md`](visual-accessibility-baseline.md) —
  viewport, screenshot, keyboard, ARIA, and axe policy;
- `test/frontend/redesign/` — deterministic Playwright seeds and executable
  specs.

Dated experiments (Overview slice, build seam, live MSR operator review) and
the pre-cutover framework ADR live under
[`docs/archive/ui-redesign/`](../archive/ui-redesign/overview-slice.md).

## Acceptance boundary

Changes must not silently alter:

- query-string routes or deep links;
- API transport ownership in `web/ts/core/api.ts`;
- lifecycle-SSE filtering, replay, or fallback polling;
- visibility-sensitive refresh behavior;
- mutation intent and runtime-convergence feedback;
- authentication, base-path, embedded-asset, or HLS behavior;
- operator-visible distinctions between pending, retrying, degraded, failed,
  and intentionally stopped states.
