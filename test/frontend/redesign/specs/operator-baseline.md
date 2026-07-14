# Operator Baseline Test Plan

Seed: `test/frontend/redesign/seed.spec.ts`

The seed logs into the isolated local application, disables live event streams,
and intercepts dashboard reads with deterministic, redacted responses. It does
not mutate application state and is suitable for Playwright planner exploration
or fixed-viewport visual capture.

## Scenario 1: Empty fleet

1. Open `?mode=overview` with the `empty` fixture.
2. Confirm the URL is canonical.
3. Confirm the Overview identifies that no pipelines are configured.
4. Confirm the Add Pipeline action is visible and keyboard reachable.

Expected: no fabricated activity, pipeline, or failure state appears.

## Scenario 2: Locate a degraded path

1. Open `?mode=overview` with the `mixed-health` fixture.
2. Locate `Retrying Destination` without opening a dialog.
3. Identify that one output is retrying while its input remains live.
4. Confirm `Healthy Program` remains visually distinct from the affected path.

Expected: the upstream input and downstream retry are not collapsed into one
generic failure state.

## Scenario 3: Responsive review

Repeat both scenarios at pinned desktop, tablet, and mobile widths.

Expected: no horizontal page overflow; the primary navigation, pipeline state,
and Add Pipeline action remain reachable.

Status: planned. Fixed viewport projects and committed screenshot baselines are
intentionally a follow-up after the seed proves stable in CI.

## Scenario 4: Accessibility review

Complete the empty-fleet and degraded-path tasks using only the keyboard. Review
accessible names and structural snapshots, then run an axe scan.

Expected: no serious or critical violations, visible focus, and no task that
requires pointer-only interaction.

Status: planned. The baseline records this gap without adding a new dependency
before its CI/runtime policy is agreed.
