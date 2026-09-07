# Decision 0001: Freeze behavior before selecting a framework

## Contents

- [Context](#context)
- [Decision](#decision)
- [Consequences](#consequences)

- Status: accepted
- Date: 2026-07-14

## Context

The dashboard already has typed routes, a centralized API client, lifecycle-SSE
coordination, visibility-sensitive polling, mutation-convergence behavior, and
substantial source/fake-DOM/browser coverage. Its largest remaining ownership
problems are oversized feature modules, imperative DOM ownership, and legacy
global handlers.

Selecting a component framework before cataloging those contracts would make
the first implementation branch responsible for both discovering behavior and
changing it.

## Decision

Land a framework-neutral baseline first. Use the first read-only Overview slice
to compare the existing implementation with a bounded island experiment.

React, Lit, TanStack Query, Figma, and any replacement design system remain
options rather than baseline dependencies.

## Consequences

- Baseline work can merge without production-runtime risk.
- Existing proofs are indexed instead of duplicated.
- Missing visual, responsive, and accessibility proofs become visible.
- A later framework choice must demonstrate improved ownership and preserved
  operational behavior.
