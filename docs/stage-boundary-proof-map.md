# Stage Boundary Proof Map

This map tracks the proof wall around stage boundaries. The goal is not line
coverage; it is to prove that packets, lifecycle state, capacity waits,
cancellation, and diagnostics cross each boundary without losing causality.

## Boundary Matrix

| Boundary | Contract to prove | Current proof | Next confidence target |
|---|---|---|---|
| Planner -> stage runtime | Planned `StageKey` and backend policy select the runtime that is registered, rendered in graph/status, and used by outputs. | Graph planner unit tests, backend-policy unit tests, engine terminal-stage tests, HLS/recording planned-key tests. | Property-test output encodings into stage plans for unique terminal keys and no stale unqualified HEVC keys. |
| Runtime admission -> registry | `ensure_stage` creates exactly one live runtime, reuses live runtimes, replaces cancelled runtimes, and snapshots lifecycle/metrics. | Stage runtime unit tests plus transcoder/TS muxer loom models for replacement races. | Add a direct loom model for generic registry admission once the model can share the production locking shape. |
| Source ring -> stage input pump | Stage input starts at the correct keyframe/preroll point, emits TS bytes only for selected media, records first input once, refreshes parameter sets, and exits on EOS/cancel. | Stage input codec-hint unit test, finite source-stage tests, source-stage chunking proptest, ring migration proptests/loom. | Unit-test first-input suppression for filtered audio-only packets and EOS completion after filtered packets. |
| Input pump -> backend | External and internal FFmpeg receive the same compiled operation and startup policy; capacity waits are lifecycle-visible and cancellation-aware. | Shared operation/compiler tests, startup-policy tests, external capacity unit/harness evidence. | Table-test each `StageKind` into `FfmpegStagePlan` plus backend operation equivalence for internal/external paths. |
| Backend -> output normalizer | Every backend emits through the normalizer; output timestamps are stage-local, non-negative, per-stream monotone, parameter sets are cached, first output is recorded once, and metrics match emitted packets. | Stage timeline unit tests, normalizer unit tests for first output, keyframe inference, split HEVC parameter sets, and a proptest over arbitrary interleaved audio/video packets asserting ring-visible timestamp/metric invariants. | Extend the property to generated split parameter-set/keyframe combinations if a future bug appears there. |
| Audio router boundary | Selected tracks, remap/downmix operations, prebuffer replay, EOS, and lifecycle cleanup preserve packet order and selected-track intent. | Audio-router unit tests for selected tracks, prebuffer replay, multi-track routing, and stage sharing. | Property-test generated selected-track operations over interleaved audio/video packets. |
| HLS segmenter boundary | Segmenter uses the planned protocol stage key, does not publish segments before init, exposes keyframe/no-segment states, and cleans runtime ownership. | HLS planned-key tests, fMP4 proptests, HLS publish loom, uploader terminal-stage tests. | Unit-test lifecycle/alert mapping for keyframe wait and no-segment states from the same snapshot. |
| Recording writer boundary | Recording metadata identity is persisted before failures, lifecycle is stage-owned, writer cleanup is visible, and media-library reads never rely on filename tokens. | Recording metadata tests, mixed harness recording identity proof, recording stage runtime ownership tests. | Add a pure service-level failure-before-output test that proves metadata identity survives writer failure. |
| Runtime snapshot -> status/graph/alerts | Non-producing stage phases surface `blockedBy`, backend/capacity details, graph lifecycle details, diagnostics context, and alerts consistently. | Engine status tests, graph/status API tests, Phase 12 alert unit tests. | Table-test every non-producing `StagePhase` against status, graph details, and alert classification where applicable. |
| Cancel/teardown -> observable cleanup | Cancellation wakes waiters, stops stages, removes runtime registry entries, and leaves operator-visible status causal rather than unknown. | AVIO/TS ring/ring migration loom, lifecycle guard tests, fault harness evidence. | Add targeted loom only where production wake/cancel ownership has no direct model yet; avoid duplicating covered ring primitives. |

## Priority Order

1. **Input pump filtered-packet/EOS proof**: cheap async unit tests for
   first-input and EOS behavior at the source-stage boundary.
2. **Status/graph/alert phase table**: unit tests ensuring every causal
   non-producing phase has the same operator meaning across read models.
3. **Planner terminal-key property proof**: generated encodings prove stage
   identity remains qualified and stable for shared-stage topologies.
4. **Audio-router selected-track property proof**: generated track layouts
   prove packet selection and prebuffer replay do not regress.
5. **Only then add loom** for any uncovered create/reuse/cancel interleaving
   that is not already modeled by the ring, AVIO, TS chunk-ring, transcoder
   stage, or TS muxer stage loom suites.
