---
name: frontend
description: Use before changing the restream dashboard (web/ts/, web/styles/), its build, its API calls, or the frontend/backend contract.
---

# Frontend

Read [frontend](../../../frontend.md) for the design system and the
dashboard route contract.

## Sources and build

- Edit `web/ts/` and `web/styles/input.css`. `public/js/` is generated
  output; never hand-edit it.
- One Vite build (`vite.config.ts`) emits every `web/ts` module
  (`preserveModules`) and puts dependencies under `public/js/vendor/`. `tsc`
  only type-checks.
- The backend embeds frontend assets with `rust-embed`, with a disk-first
  fallback during development.

## Contract

- Route every dashboard API call through `web/ts/core/api.ts`; route
  literals elsewhere fail `node scripts/check/api-drift.mjs`.
- Routing and per-view polling use TanStack Router and Query
  (`web/ts/control-room/router.tsx`, `polled.tsx`); the pushed runtime state
  stays in `store.ts`.
- When a route or payload changes, update the API contract tests
  (`cargo xtask api-contract`).
- The runtime-state TypeScript contract is generated from Rust:
  `cargo test --lib runtime_state::tests::write_generated -- --ignored`.

## Verification

- Default: `npm run test:frontend` (run only the new or changed test
  locally; CI runs the rest).
- Browser-only behavior: `npx playwright test`.
- Coverage: `npm run test:frontend:coverage`.
- Layering or module-boundary refactors: see
  [layering-audit](../layering-audit/SKILL.md).
