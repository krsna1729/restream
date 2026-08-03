# Frontend Boundary Proof Map

This map is the frontend counterpart to
[`stage-boundary-proof-map.md`](stage-boundary-proof-map.md). The goal is not
line coverage; it is to prove that operator-visible UI contracts — stream
reconnects, mutation convergence, session/auth redirects, route ownership,
and accessible structure — hold across the boundaries where the dashboard
talks to the backend, to real browser APIs, and to the operator.

## Contents

- [Boundary Matrix](#boundary-matrix)
- [Current Mandatory Surfaces](#current-mandatory-surfaces)
- [Priority Order](#priority-order)

## Boundary Matrix

| Boundary | Contract to prove | Current proof | Next confidence target |
|---|---|---|---|
| API client -> backend routes | Every dashboard read/mutation hits a canonical `/api/v1` route and method; multipart upload shape stays stable. | `test/frontend/frontend-api-contract.test.mjs`, plus the cross-stack `scripts/check/api-drift.mjs` gate run by `scripts/check/api-contract.sh`. | None open; this is the most mature boundary — the only one with a hard-failing CI gate rather than an advisory test run. |
| SSE/log-stream reconnect | One connection per filter/scope, paused while the tab is hidden, resumed from the last event id on visibility, replaced (not duplicated) when scope changes, and a superseded source's events never reach the caller. | `frontend-log-stream.test.mjs`, `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`, `frontend-overview-activity-stream.test.mjs` for scripted scenarios; `frontend-log-stream-interleaving.property.test.mjs` model-checks the staleness guard (`source !== openedSource`) against randomized `sync()`/`emit()` interleavings, the frontend analog of a loom test. | The interleaving proof covers `core/log-stream.ts` only; `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`, and `frontend-overview-activity-stream.test.mjs` still rely on scripted scenarios for their own reconnect guards. |
| Dashboard runtime polling and mutation convergence | Output/pipeline start, stop, edit, and delete mutations optimistically update the UI and converge with the shared runtime poller; nothing starts a second poller of its own. | `test/frontend/dashboard-contract/output-mutations.test.mjs`, `pipeline-mutations.test.mjs`, `runtime-modes.test.mjs`, `runtime-polling.test.mjs`, `frontend-publisher-health-contract.test.mjs`. | None open. |
| Auth/session boundary | Unauthenticated requests redirect to `/login`; a successful login reaches the dashboard and preserves the intended destination. | `test/frontend/frontend-browser-dom.spec.ts` (login flow, audio-track picker, HLS retry, mobile overflow). | None open. |
| HLS playback and fatal-error retry | The managed HLS controller waits for manifest readiness, destroys and recreates the player on a fatal error, supports alternate-audio-track switching, and clears state on stage teardown. | `test/frontend/hls-player.spec.ts` (real browser playback), `npm run test:frontend:browser-dom` (audio-track picker), backend `cargo test hls_fmp4` and `cargo bench --bench hls_fmp4_cost` for the segment/publication side. | None open; see `docs/testing.md`'s fMP4 preview section for the full ladder. |
| Dashboard route ownership and navigation history | Each mode route (overview, pipeline, media, settings, status, incidents, telemetry) is rendered by the v2 owner with no leftover v1 fallback; browser back/forward is one predictable history step per navigation; primary tab focus survives background refresh. | `test/frontend/redesign/seed-navigation.spec.ts`, `seed-media-route.spec.ts`, `seed-settings-route.spec.ts`, `seed-status-route.spec.ts`, `seed-surfaces.spec.ts`, `frontend-ops-navigation.test.mjs`, `frontend-build-smoke.test.mjs`. | None open. |
| Accessible structure | Heading outline stays in true reading order and operator-clean across default routes, interactive controls expose accessible names, no serious/critical axe violations, and keyboard focus reaches primary actions. | `test/frontend/redesign/visual-accessibility.spec.ts`. | The strict route-heading-order assertion only covers Overview, Operate, Inspect, and Monitor; extend to Media/Settings/Status/Incidents/Telemetry if a heading-order regression ever surfaces there. |
| Scale and large-fleet rendering | The dashboard stays responsive and collapses repeated entries (egress leaves, non-egress branch stages) once pipeline/output counts get large. | `test/frontend/redesign/seed-scale.spec.ts`, `frontend-pipeline-workspace.test.mjs` (processing-graph collapse cases). | None open. |

## Current Mandatory Surfaces

These frontend tests must never regress silently. Touching the boundary they
cover requires an equal or stronger replacement proof in the same change,
mirroring [`concurrency-proofing.md`](concurrency-proofing.md)'s backend
list:

- `test/frontend/frontend-api-contract.test.mjs`
- `test/frontend/dashboard-contract/output-mutations.test.mjs`,
  `pipeline-mutations.test.mjs`, `runtime-modes.test.mjs`,
  `runtime-polling.test.mjs`
- `test/frontend/frontend-publisher-health-contract.test.mjs`
- `test/frontend/frontend-log-stream.test.mjs`,
  `frontend-log-stream-interleaving.property.test.mjs`,
  `frontend-status-stream.test.mjs`, `frontend-history-stream.test.mjs`
- `test/frontend/hls-player.spec.ts`
- `test/frontend/frontend-browser-dom.spec.ts`
- `test/frontend/redesign/seed-navigation.spec.ts`,
  `seed-media-route.spec.ts`, `seed-settings-route.spec.ts`,
  `seed-status-route.spec.ts`
- `test/frontend/redesign/visual-accessibility.spec.ts`
- `test/frontend/frontend-build-smoke.test.mjs`

## Priority Order

All current priority targets are complete. New proof work should start by
adding a row to this map for the new UI surface, stream contract, or route
boundary, then choose the lowest layer that can catch the bug (TypeScript
unit -> fake-DOM scenario matrix -> browser-native Playwright -> full
`test:e2e`), per `docs/testing.md`'s layered UI strategy.
