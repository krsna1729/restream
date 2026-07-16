# Dashboard v2 live MSR operator review — 2026-07-16

## Contents

- [Scope](#scope)
- [Live setup evidence](#live-setup-evidence)
- [Browser/CDP crawl](#browsercdp-crawl)
- [Operator findings](#operator-findings)
- [Done-state interpretation](#done-state-interpretation)

## Scope

This pass used the Mahashivratri/MSR harness as a populated live-pressure setup,
excluding any in-browser agent workflow. The run exercised a single live SRT
ingest with 30 audio tracks and 120 active egress outputs.

## Live setup evidence

Command shape:

```sh
MSR_OUTPUT_COUNTS=120 MSR_SAMPLE_SECS=8 MSR_SAMPLE_INTERVAL_MS=4000 \
  MSR_SINK_SAMPLE_SECS=2 MSR_NO_CLEANUP=1 BENCH_BUILD=if-needed \
  ./scripts/harness/run.sh msr -- --no-netns
```

Result: `PASS`.

Artifacts:

- `.local/artifacts/latest/msr.json`
- `.local/artifacts/msr/msr-results.json`
- `.local/artifacts/ui-v2-live-msr/crawl-results.json`
- `.local/artifacts/ui-v2-live-msr/*.png`

Key live numbers:

| Signal | Value |
|---|---:|
| Outputs executed | 120 |
| Output mix | 114 RTMP / 6 SRT |
| Restream output progress | 120/120 |
| MediaMTX ready paths | 120/120 |
| Restream avg CPU | 15.03% |
| Restream RSS peak | 116,624 KiB |

## Browser/CDP crawl

The crawl logged into the live dashboard, forced `ui=v2`, and visited:

- Overview
- Pipeline / Operate
- Pipeline / Inspect
- Pipeline / Monitor
- Media
- Settings
- Status
- Incidents
- Telemetry

Summary:

| Screen | Ownership today | CDP nodes | Observation |
|---|---|---:|---|
| Overview | v2 | 3,829 | Calm fleet summary; 1 live input / 120 running outputs visible. |
| Pipeline / Operate | v2 | 6,260 | Bounded output list works under 120 outputs with search/filter and first-8 default. |
| Pipeline / Inspect | legacy | 13,863 | Usable, but still visually/DOM-heavy compared with v2. |
| Pipeline / Monitor | legacy | 17,052 | Usable, but a separate redesign pass is still needed. |
| Media | legacy | 18,990 | Not redesigned in v2. |
| Settings | legacy | 22,123 | Not redesigned in v2. |
| Status | legacy | 25,700 | Not redesigned in v2. |
| Incidents | legacy | 28,494 | Not redesigned in v2. |
| Telemetry | legacy | 32,479 | Not redesigned in v2. |

## Operator findings

What is now strong enough for v2:

- Overview and Pipeline / Operate carry the v2 seam cleanly.
- Large output sets are bounded by default instead of dumping every output card.
- Output search/filter is necessary and useful under MSR-scale output counts.
- Chaos-derived states are now covered by seeded fixtures:
  - reconnecting publisher grace;
  - HLS retry timeout;
  - recovered-but-flapping sink;
  - stalled sink among healthy siblings;
  - retry-budget-exhausted terminal failure.

What changed from the live operator pass:

- A 30-audio-track MSR input made the input card too tall. v2 now shows the
  first six audio tracks by default and exposes an explicit `Show all 30`
  progressive-disclosure affordance.
- The first focused pipeline view could briefly look incomplete while the
  selected-pipeline runtime refresh converged. v2 now renders one lightweight
  details placeholder in the header slot so the operator sees that the selected
  pipeline is catching up instead of seeing a silent partial shell.

Still not a full v2 redesign:

- Inspect, Monitor, Media, Settings, Status, Incidents, and Telemetry are still
  intentionally legacy-owned.
- Legacy-owned routes keep substantial hidden DOM mounted across navigation.
  This is visible in CDP node growth and should be treated as a future
  performance/accessibility cleanup, not as solved by the v2 seam.

## Done-state interpretation

For an rc3 merge candidate, v2 is ready as a bounded, opt-in replacement for
Overview and Pipeline / Operate. It is not yet a complete dashboard-wide v2.
The remaining evolution should be sequenced as separate ownership passes:

1. Pipeline / Inspect v2.
2. Pipeline / Monitor v2.
3. Incidents and Telemetry v2.
4. Media, Settings, and Status polish.
5. Hidden DOM / route unmount cleanup across legacy panels.
