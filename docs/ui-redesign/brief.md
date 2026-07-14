# UI Redesign Baseline

## Purpose

Freeze the operator workflows and runtime semantics that a future visual
redesign must preserve. This baseline is deliberately framework-neutral: it
adds specifications and deterministic browser setup without changing the
production dashboard.

## Outcome

The redesign may change layout, styling, component boundaries, and internal
rendering technology. It must not silently change:

- query-string routes or deep links;
- the API transport owner in `web/ts/core/api.ts`;
- lifecycle-SSE filtering, replay, or fallback polling;
- visibility-sensitive refresh behavior;
- mutation intent and runtime-convergence feedback;
- authentication, base-path, embedded-asset, or HLS behavior;
- operator-visible distinctions between pending, retrying, degraded, failed,
  and intentionally stopped states.

## Baseline deliverables

- `operator-task-model.md`: the workflows an operator must be able to finish;
- `state-matrix.yaml`: important states and their proof status;
- `route-contract.md`: canonical URL ownership and compatibility behavior;
- `migration-map.md`: safe slice order and ownership boundaries;
- `decisions/0001-baseline-before-framework.md`: why the framework decision is
  deferred;
- `test/frontend/redesign/`: a deterministic, redacted Playwright seed and an
  executable-spec starting point.

## Acceptance boundary

This baseline is complete when:

1. every required state has an existing proof or an explicitly recorded gap;
2. the seeded Overview is deterministic and contains no production secrets;
3. existing frontend, fixture, API-contract, and test-hygiene gates pass;
4. no production frontend or backend file changes.

The baseline does not select React, Lit, TanStack Query, a design tool, or a
replacement design system. Those choices require evidence from a bounded
Overview experiment.
