# UI Migration Map

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
