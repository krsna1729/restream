# Regression Artifact Index

This index preserves the historical failure evidence that drove the first
architecture phases. Generated run directories stay under `test/artifacts/` and
are not committed; the durable guardrail is the checked-in fixture, harness
mode, or proof gate listed here.

| Historical failure class | Preserved evidence / replay path | Guardrail |
|---|---|---|
| External H.265 capacity or zero-output stall | HEVC checked-in fixtures: `test/fixtures/correctness-h265.ts`, `test/fixtures/bench-h265-1_5m.ts`, `test/fixtures/bench-h265-1_5m-2a.ts`, plus mixed HEVC modes such as `mixed.live.srt.h265.a1.bf2` and `mixed.live.srt.h265.a2.bf2`. | Dependency-aware health and alert tests cover `waitingForCapacity`; `scripts/check-concurrency-proof-fast.sh` includes external stage liveness checks. |
| Low-CPU external-capacity collapse | Resource sweep artifacts are generated under `test/artifacts/resource-sweep/`; authoritative CSV baselines are documented in `docs/resource-sweep.md`. | `target/bench/test_harness resource-sweep` and `docs/matrix-resource-constraints.md` preserve the capacity/RSS contract. |
| Internal-transcoder timestamp discontinuity | `tests/transcoder.rs` and `tests/av_sync.rs` use checked-in MPEG-TS fixtures through `src/test_fixtures.rs`. | `scripts/check-concurrency-proof-fast.sh` runs chunked internal-transcoder timestamp tests and source-stage proptests. |
| Recording `.tmp.mp4` or wrong-case media selection | Recording metadata tests in `tests/api.rs` and mixed harness playback tests reject temporary outputs and metadata-less filename fallback. | `cargo test media_recording_identity --bin test_harness` and API media-library metadata tests preserve recording identity by `pipelineId`/`recordingId`. |

When adding a new historical failure artifact, prefer one of these durable
forms:

- a checked-in media fixture registered in `src/test_fixtures.rs`;
- a focused unit/integration test that recreates the failure from an existing
  fixture;
- a harness mode that writes `manifest.json` and `results.jsonl` under
  `test/artifacts/<run-id>/`;
- a documented benchmark or sweep baseline with its replay command.

Do not commit ad-hoc generated run directories. If a generated artifact is
needed for triage, store it under `test/artifacts/<run-id>/` and reference the
run id from the issue, PR, or quality journal entry.
