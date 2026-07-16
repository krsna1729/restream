# Operator Baseline Test Plan

## Contents

- [Scenario 1: Empty fleet](#scenario-1-empty-fleet)
- [Scenario 2: Locate a degraded path](#scenario-2-locate-a-degraded-path)
- [Scenario 3: Responsive review](#scenario-3-responsive-review)
- [Scenario 4: Accessibility review](#scenario-4-accessibility-review)

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

Status: automated at 1440 x 900, 1024 x 768, and 390 x 844 with committed
Chromium screenshot baselines, a page-overflow assertion, and browser-visible
Overview navigation and a rendered Add Pipeline action.

## Scenario 4: Accessibility review

Complete the empty-fleet and degraded-path tasks using only the keyboard. Review
accessible names and structural snapshots, then run an axe scan.

Expected: no serious or critical violations, visible focus, and no task that
requires pointer-only interaction.

Status: automated for the mixed-health Overview ARIA structure and a zero
serious/critical WCAG 2.0/2.1 A/AA axe threshold in both fixtures. The Add
Pipeline keyboard path retains focus across a periodic refresh and supports
Enter/Escape operation. Manual keyboard, focus, contrast, zoom, and
assistive-technology review remains required before a redesigned surface ships.
