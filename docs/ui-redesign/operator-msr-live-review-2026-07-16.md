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

The seeded browser proof now also exercises the owned v2 path by keyboard:
Overview tabs to `Add Pipeline`, tabs to an attention pipeline `Operate`
action, presses Enter into Operate, then keyboard-selects another pipeline from
the v2 selector. CDP assertions keep Overview and Operate node budgets bounded
and verify stable accessible button names for output operations.

The same seeded proof now starts at a visible-on-focus skip link. Pressing Enter
lands focus on the active main tabpanel before the dense dashboard chrome, so
keyboard operators can bypass the navbar, workspace tabs, and secondary
pipeline navigation when they are already trying to work the current screen.

Overview-to-pipeline navigation is now atomic in v2: the attention-card Operate
and Inspect actions each push one canonical `mode=pipeline&view=...&p=...` URL,
render the selected pipeline destination immediately, and let a single browser
Back return to clean v2 Overview.

The seeded v2 Operate path also has a CDP layout proof across the configured
desktop, tablet, and mobile browser projects. The stress fixture uses the
recovered sink-flap pipeline with 30 audio tracks and verifies that both DOM
scroll width and CDP layout content width stay within the viewport.

The same v2 Operate stress path now runs axe across desktop, tablet, and mobile
with WCAG 2.0/2.1 A/AA tags. It has no serious or critical findings, and CDP
accessibility-tree assertions verify that the primary headings and actions stay
semantically named.

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
- Failed recording mutations now keep contextual status in the v2 pipeline
  header after the existing API alert fires, so the operator can see which
  lifecycle action failed and retry without scanning away from the control.
- File-ingest failures use the same local header pattern, preserving the
  existing API alert while keeping the failed action and retry affordance
  adjacent to `Start File` / `Stop File`.
- Failed output start/stop mutations now stay visible on the affected v2 output
  card, so the operator can retry the exact destination without losing context
  after the global error alert fades.
- Long pipeline lists now get the same search/status feedback pattern as large
  output sets, so the operator can jump to a named or degraded pipeline without
  turning the left rail into a scroll hunt.
- v2 input ownership now empties the hidden legacy audio table, matching the
  already-empty legacy preview and output-card cleanup pattern so CDP/node
  growth reflects only the active operator surface.
- v2 selector ownership now empties the hidden legacy pipeline rows as well,
  so the active rail is the only pipeline navigation subtree under Operate.
- v2 Overview ownership now empties the hidden legacy Overview container, so
  the fleet-summary route does not carry duplicate inactive summary markup.
- v2 Overview now adds pipeline-name search only once the fleet is large enough
  to become a scan burden, with CDP-visible status text for result counts and
  no-result recovery.
- Shared auth expiry now preserves the full operator return path, including
  `ui=v2`, so a re-login can return to the interrupted v2 workflow instead of
  dumping the operator at the default Overview.

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
