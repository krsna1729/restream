# Overview vertical slice

## Contents

- [Objective](#objective)
- [Implemented direction](#implemented-direction)
- [Proof boundary](#proof-boundary)
- [Architecture finding](#architecture-finding)

## Objective

Test a priority-first information hierarchy on the read-only Overview without
changing dashboard routes, API ownership, lifecycle streams, polling cadence,
or mutation behavior.

## Implemented direction

- The page starts with the current operator priority instead of six equal
  metric cards.
- Only pipelines with actionable runtime states appear in the priority panel;
  healthy pipelines remain available in the fleet table.
- Fleet throughput and engine load are grouped into a compact signal panel.
- The full pipeline comparison and restream activity stream retain their
  existing behavior and ownership.
- Empty state guidance remains visible before the signal panel on narrow
  screens.

## Proof boundary

The deterministic `empty` and `mixed-health` scenarios prove the hierarchy at
1440 x 900, 1024 x 768, and 390 x 844. The mixed-health fixture has explicit
probe readiness so its healthy input is not incorrectly promoted into the
priority panel.

The browser contract also proves:

- the retrying destination is visible in the first mobile viewport;
- priority content precedes fleet signals and the pipeline table;
- keyboard focus survives a periodic runtime refresh;
- the Overview ARIA structure is reviewed;
- both scenarios have no serious or critical axe findings.

## Architecture finding

The hierarchy can be tested safely in the existing feature renderer without a
framework or build-system change. That keeps this result attributable to the
information architecture rather than a simultaneous runtime migration.

This slice does not resolve the broader ownership cost in `features/modes.ts`.
The follow-up component build seam is recorded in `build-seam.md`; it proves
that an island can preserve the embedded-asset contract, but intentionally
defers moving Overview state until a feature experiment can demonstrate simpler
ownership and focused tests.
