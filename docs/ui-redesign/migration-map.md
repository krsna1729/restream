# UI Migration Map

## Contents

- [Ownership that remains stable](#ownership-that-remains-stable)
- [Stop rules](#stop-rules)
- [Build artifact contract](#build-artifact-contract)

## Ownership that remains stable

- `web/ts/app/` owns bootstrap and cross-feature composition.
- `web/ts/core/api.ts` remains the sole dashboard API transport owner.
- `web/ts/core/pipeline-workspace.ts` remains the location contract.
- `web/ts/core/` owns shared state, transport, and pure transforms.
- Bounded feature modules own local rendering and interaction behavior.
- The HLS player remains behind an imperative lifecycle adapter.
- The v2 dashboard boots unconditionally; obsolete `ui` query parameters are
  ignored.

Historical slice-order and framework-decision narrative:
[`docs/archive/ui-redesign/`](../archive/ui-redesign/build-seam.md).

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

## Build artifact contract

Release preparation and artifact-smoke scripts still require:

- `public/js/app/dashboard-entry.js`;
- `public/output.css`;
- `public/js/lib/hls.min.js`;
- `public/base-path.js` and the existing login assets.

Hashed filenames or a manifest need their own release-contract change.
