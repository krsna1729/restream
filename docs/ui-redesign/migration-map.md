# UI Migration Map

## Contents

- [Ownership that remains stable](#ownership-that-remains-stable)
- [Slice order](#slice-order)
- [Stop rules](#stop-rules)
- [Build seam contract](#build-seam-contract)
- [Framework decision evidence](#framework-decision-evidence)

## Ownership that remains stable

- `web/ts/app/` owns bootstrap and cross-feature composition.
- `web/ts/core/api.ts` remains the sole dashboard API transport owner.
- `web/ts/core/pipeline-workspace.ts` remains the location contract.
- `web/ts/core/` owns shared state, transport, and pure transforms.
- bounded feature modules own local rendering and interaction behavior.
- the existing HLS player remains behind an imperative lifecycle adapter.

## Slice order

1. Baseline specifications and deterministic fixtures; no production changes.
2. Optional inert mount/build experiment behind a development-only switch.
3. Tokens and the minimum primitives required by the first real slice.
4. Overview, preserving existing polling and lifecycle-event behavior.
5. Pipeline Inspect.
6. Pipeline Monitor.
7. Pipeline Operate and destructive mutations.
8. Remaining incidents, telemetry, media, settings, and status surfaces.
9. Legacy removal only after all state-matrix rows have executable proofs.

The optional build experiment in step 2 is implemented and measured in
`build-seam.md`. It began as an opt-in `ui=v2` seam; the current
default-readiness pass selects v2 when no explicit UI override is stored or
provided, while preserving `ui=legacy` as the explicit fallback.

The follow-up typed snapshot in `build-seam.md` proved the first real data
boundary without moving runtime ownership. That same boundary still holds:
`core/api.ts`, polling, SSE, URL state, and mutation lifecycle remain owned by
the existing dashboard runtime, while the v2 shell now owns the mounted route
hosts for Overview, Pipeline Operate, Pipeline Inspect, Pipeline Monitor, Media,
Settings, Status, Incidents, and Telemetry. Dense controls can still be
rewritten slice by slice after the default switch because the route body
mount-point ownership is no longer split across hidden legacy panels.

Seeded browser fixtures now mirror the production default: a dashboard URL
without a `ui` parameter leaves local UI preference unset and therefore boots
v2. Fallback checks request `ui=v1` explicitly, so no-query coverage proves the
cutover path while the escape hatch remains tested.

## Stop rules

- Do not create the full target folder tree before a slice needs it.
- Do not replace `core/api.ts` with framework-specific fetch calls.
- Do not introduce a second URL router or app-wide client-state store.
- Do not convert SSE events into broad refetches without preserving event
  filtering, 200 ms coalescing, selected-pipeline scoping, and replay behavior.
- Do not replace the five-second foreground, thirty-second hidden-tab, or
  visibility-resume contracts without explicit operator evidence.
- Do not replace the 1.5-second mutation fallback with a generic loading state.
- Do not change generated assets in `public/js/` by hand.

## Build seam contract

The first build experiment must continue to produce and serve:

- `public/js/app/dashboard-entry.js`;
- `public/output.css`;
- `public/js/lib/hls.min.js`;
- `public/base-path.js` and the existing login assets.

The release preparation and artifact-smoke scripts are consumers of these
paths. Hashed filenames or a manifest are a later migration with their own
release-contract change.

## Framework decision evidence

An Overview experiment may recommend a framework only if it demonstrates:

- less cross-feature coordination than the existing composition root;
- unchanged request cadence and mutation/SSE contracts;
- no material regression in render churn or bundle/startup cost;
- simpler focused tests, not merely more wrappers;
- a clean path through embedded release-artifact verification.
