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
| Media | v2 checkpoint + legacy library | 18,990 | First-glance recording/source and search state is v2; dense media actions remain legacy. |
| Settings | v2 checkpoint + legacy form | 22,123 | First-glance config/auth security state is v2; dense admin controls remain legacy. |
| Status | v2 checkpoint + legacy details | 25,700 | First-glance process/build state is v2; dense status sections remain legacy. |
| Incidents | v2 checkpoint + legacy feed | 28,494 | First-glance incident state is v2; dense alert/event feed remains legacy. |
| Telemetry | v2 checkpoint + legacy counters | 32,479 | First-glance telemetry state is v2; dense counter grids remain legacy. |

The seeded browser proof now also exercises the owned v2 path by keyboard:
Overview tabs to `Add Pipeline`, tabs to an attention pipeline `Operate`
action, presses Enter into Operate, then keyboard-selects another pipeline from
the v2 selector. CDP assertions keep Overview and Operate node budgets bounded
and verify stable accessible button names for output operations.

The shared workspace summary now also announces the current ownership state:
`UI v2 owned` for Overview and Pipeline Operate, `UI v2 checkpoint` for
Pipeline Inspect, Pipeline Monitor, Media, Settings, Incidents, Telemetry, and
Status. Seeded Playwright/CDP
coverage proves that cue is visible and exposed as status text while moving
across the route journey.

Incidents and Telemetry are now discoverable from the primary workspace tab
strip instead of being route-only surfaces. They remain legacy-owned
checkpoints, but operators can reach alert triage and engineering counters from
the same shell navigation as Overview, Pipeline, Media, Settings, and Status.

The workspace and Pipeline sub-tab bars now support Arrow, Home, and End
keyboard navigation with activation. Seeded Playwright/CDP coverage walks
Overview → Pipeline → Incidents → Telemetry → Status and Pipeline Operate →
Inspect → Monitor without requiring repeated Tab presses.

Overview and the v2 pipeline selector now expose Clear search as soon as an
operator narrows a long pipeline list, not only after a no-hit. Seeded
Playwright/CDP coverage proves hit-state and no-hit recovery on both search
surfaces while keeping the result-count status text intact.

Overview Restream Activity now follows the same noise-management contract for
chaos/MSR bursts: once there are enough grouped events to scan, the panel gets a
local search with announced result counts, hit-state recovery, and no-hit Clear
activity search recovery. This keeps the activity list useful without turning
the top-level Overview into another dense log wall.

Operate output search/filter recovery is now available before the operator
hits an empty list: when a query or state filter is active, a local Clear output
filters action restores the full destination list. Seeded Playwright/CDP
coverage proves both hit-state recovery and no-hit recovery announce the right
status text.

The same seeded proof now starts at a visible-on-focus skip link. Pressing Enter
lands focus on the active main tabpanel before the dense dashboard chrome, so
keyboard operators can bypass the navbar, workspace tabs, and secondary
pipeline navigation when they are already trying to work the current screen.

Overview-to-pipeline navigation is now atomic in v2: the attention-card Operate
and Inspect actions each push one canonical `mode=pipeline&view=...&p=...` URL,
render the selected pipeline destination immediately, and let a single browser
Back return to clean v2 Overview.

Pipeline workspace navigation is also context-stable: seeded Playwright/CDP
coverage proves an operator can move Operate → Inspect → Monitor while retaining
the same selected pipeline, and that clicking the already-active workspace tab
does not add a duplicate browser-history entry.

Pipeline Inspect now announces the current inspection scope before its dense
graph and resource sections: selected pipeline, input state, output count, and
attention count. It also promotes the diagnostics focus into one live status
line: probe readiness, fault-candidate count, and suggested next step. Seeded
Playwright/CDP coverage proves the Overview → Inspect flow lands on both
summaries and exposes them as status text.
Inspect now has its first v2-owned checkpoint as well: a React scan strip that
summarizes selected pipeline health, graph readiness, output attention, and the
next diagnostic step before the legacy graph/resource panels. The existing graph
explorer and output-preview search remain legacy-owned underneath that strip.
Inspect resource details now also follow the v2 scan-first rule: process and
pipeline-attribution summary cards stay visible, while raw FFmpeg worker tables,
resource truncation notes, and attribution-accuracy details mount only after the
operator opens `Show resource details`. This keeps Inspect useful for first-pass
triage without starting every graph visit with dense worker tables.

Pipeline Inspect now also gives the output preview a local search/count/clear
loop once the selected pipeline has enough outputs to become scan-heavy. The
stalled-sink chaos fixture proves an operator can isolate one healthy sibling
or a no-hit without leaving Inspect for Operate, while CDP status text keeps
the result count announced.

Monitor now announces its pipeline scope before the monitoring wall: selected
pipeline, output count, configured monitor count, and missing monitoring URL
count. Search feedback still separates filtered matches from configuration
gaps: seeded Playwright/CDP coverage proves a search miss is announced as
`0/N monitored match` while preserving the true missing-monitoring-URL count,
then proves the local Clear search action restores the monitoring wall without
resetting the whole room. That prevents the control room from implying a
configured monitor disappeared just because the operator narrowed the list.
Generic web monitor embeds are now lazy by default: the card exposes an
explicit `Load preview` action before mounting the iframe, while direct
Open/Copy/Edit URL actions remain available behind an explicit per-card
`Show monitor actions` disclosure. That keeps the initial Monitor checkpoint
lighter, makes cross-origin preview loading intentional, and lets the wall read
as live status first rather than URL maintenance first.
Monitor now also has its first v2-owned checkpoint strip above the legacy wall.
It turns that same control-room state into one scan layer: monitor coverage,
missing URLs, active search narrowing, lazy web-preview count, and next operator
step. Playback, edit/save, player lifecycle, and external preview loading remain
legacy-owned underneath it.

Media now has the same lightweight operator checkpoint above the legacy library.
The v2 strip answers the first operator question before the dense rows: how many
recordings, how many source files, whether search is narrowing the library, and
whether storage telemetry is visible. Upload, rename, delete, playback, and
download remain legacy-owned below it.

Media search now follows the same feedback contract: the checkpoint and legacy
Media route expose one live result-count summary that also splits matches by
Recordings and Source Files. Seeded Playwright/CDP coverage proves search hits
and misses are announced without relying on visual scanning of both sections,
then proves the local Clear search action restores the full library.
Dense media libraries now also use bounded Recordings and Source Files lists
with explicit `Show all` affordances. Search still matches the full fetched
library, so the first view stays a scan layer while full file audit remains one
click away per section.
Media rows now separate primary and secondary actions in v2: Play stays visible
as the routine inspection task, while Download, Rename, and Delete mount only
after the operator opens `More actions` on that row. This keeps the library
scan calmer and keeps archive/destructive actions intentional instead of
visually competing with playback.

Status now has the same lightweight operator checkpoint: the v2 strip announces
loaded/warning/error state, build identity, process-log count, notable-activity
count, active log-search result, SBOM component count, and uptime before the
dense status sections. Seeded Playwright/CDP coverage proves the checkpoint
updates for hit/no-hit search states while the legacy route summary remains
visible and exposed in the accessibility tree.

Status now also has one local search surface across Recent Activity and Process
Log. The route summary remains the unfiltered process truth, while the search
result summary is announced as status text for hit and no-hit states. Seeded
Playwright/CDP coverage proves operators can narrow by target/message, recover
with a local Clear search action, and keep the authoritative route counts.

Incidents now gets the same cognitive-load reduction before its alert and
event feeds: the v2 checkpoint summarizes critical/warning state, recent-event
volume, active scope, and the current incident search result before the legacy
feed. The legacy route still announces critical, warning, recent-event, and
active scope counts as a live status line. Seeded Playwright/CDP coverage proves
both fleet-wide and pipeline-scoped summaries update visibly and in the
accessibility tree.

Incidents also now has one local search surface across active alerts and recent
lifecycle events. The search result summary is announced as status text, so an
operator can narrow a noisy incident feed by destination, pipeline, cause, or
event wording without visually scanning both columns. Seeded Playwright/CDP
coverage proves hit and no-hit summaries are visible and announced, and that a
local Clear search action restores both columns without changing incident scope.
Dense incident feeds now also use bounded alert and event lists with explicit
`Show all` affordances. Search still matches the full fetched incident set, so
the first view is triage-first while full audit mode stays one click away.
Incident alert cards now keep the triage surface compact in v2: severity,
cause, last-seen time, and `Open pipeline` stay visible, while evidence and
recommended-action detail blocks mount only after the operator opens `Show alert
details` on that alert. This makes the feed easier to scan under pressure
without removing the action guidance.

Telemetry now has an equivalent engineer-facing checkpoint: before the dense
counter grids, the v2 strip summarizes loaded/stale state, scoped pipeline,
stage counter volume, egress count, active telemetry search result, reader
count, and transcoder-buffer count. Seeded Playwright/CDP coverage proves that
summary updates on pipeline switch and hit/no-hit search states, while the
legacy route status remains exposed as status text.

Telemetry stage cards now behave as a scan layer instead of a raw counter dump:
each card shows stage state plus counter count, then sends the operator to the
Stage detail panel for raw values. Seeded Playwright/CDP coverage proves the
raw `packetsOut` counter is absent from the initial stage grid and appears only
after the operator activates the stage Details control. v2 also lets the
operator hide the selected stage detail again, returning the page to the
counter-scan state after inspection.

Telemetry also now has a local filter across readers, processing stages, and
egresses. The filter summary is announced as status text, so operators can
narrow dense MSR telemetry by reader, stage, output, or counter name, recover
with Clear search, and keep the route-level telemetry scope unchanged.
Dense telemetry stage and egress lists are also bounded by default with explicit
`Show all` affordances. This keeps the checkpoint closer to the v2 Operate
model: scan a few processing branches/destinations first, search when isolating
one stage or destination, and only expand the full fan-out when comparison is
intentional.
Telemetry host settings now follow the same progressive-disclosure rule in v2:
the first view shows health plus the host-setting count, while the raw
kernel/runtime table is mounted only after the operator asks for it. That keeps
capacity prerequisites available for deep diagnosis without making every
Telemetry visit start with a low-frequency configuration table.

Settings now has the same lightweight operator checkpoint above the legacy admin
form. The v2 strip answers the first operator question before the dense
controls: which config surface is loaded, how many profiles exist, how many
auth attempts are tracked, whether auth search is filtering, and whether any
attempts are currently banned. Save/logout/password/profile mutations remain
legacy-owned below it.

Settings now also has a local authentication-attempt search surface. The route
summary and v2 checkpoint remain the unfiltered settings truth, while the
auth-attempt search summary announces hit and no-hit counts as status text.
Seeded Playwright/CDP coverage proves operators can narrow by scope/IP/status,
recover with a local Clear search action, and keep the authoritative settings
counts.
Dense authentication-attempt rows are now bounded by default with an explicit
`Show all` affordance. Search still matches the full fetched security state,
so an operator can scan the first few attempts before choosing full audit mode.
Authentication reset actions now follow the same v2 maintenance-action rule:
`Refresh` remains visible, while `Reset All` and per-row `Reset` buttons mount
only after `Show reset actions`. That keeps the default security surface focused
on blocked/tracked attempts and makes destructive recovery actions deliberate.
Account/session actions follow that same rule as well: `Logout` remains
available under `Show account actions`, but no longer sits in the first settings
scan layer beside configuration and security state.
Under `ui=v2`, Settings also now treats low-frequency advanced configuration
groups as disclosure sections. Recording retention, global SRT ingest policy,
backend policy, and transcode profiles show compact summaries first; the actual
forms stay available on demand. The security/auth-attempt surface stays open
because it answers the common operator question: "is anything blocked right
now?"
Within transcode profiles, each row now keeps the scan layer to name, preset,
and tune first; numeric encoder tuning is lazy-mounted only after `Show tuning`.
That preserves save semantics from row-backed values while removing a grid of
low-frequency boxes from both the default Settings read and the collapsed DOM.
Under `ui=v2`, leaving dense checkpoint routes now also unmounts their legacy
detail DOM so later checkpoint routes do not inherit hidden route weight. Media
resets its list render cache on that unmount path, so returning to the route
repaints the library instead of preserving stale placeholder state.

Status process logs are now bounded by default with an explicit `Show all`
affordance. The route still fetches the same recent history and search still
matches the full fetched set, but the first view is a scan layer rather than an
80-card wall of logs.
Status advanced diagnostic sections now follow the same rule under `ui=v2`:
Toolchain, Native Libraries, and SBOM render compact summaries first and mount
their row tables only when the operator asks for details. Build, System,
Activity, and Logs remain immediate because they answer the common "is the
service healthy right now?" question.
Status export/SBOM actions now also follow that audit-only pattern under
`ui=v2`: the route shows a compact Export actions row first, then mounts
Download/Copy Status and SBOM actions only after `Show export actions`. That
keeps the default Status readout focused on build, system, activity, and log
state while preserving the release/audit evidence workflow.

The seeded browser proof also includes a route-checkpoint matrix across the
checkpoint screens: Inspect, Monitor, Media, Settings, Status, Incidents, and
Telemetry. That matrix walks the screens as an operator journey and proves each
one exposes a visible checkpoint, matching CDP accessibility `status` text, and
a route-specific node budget. Inspect and Monitor now use v2-owned checkpoint
strips while the other dense surfaces remain legacy-owned. In the seeded route
journey, those budgets are
currently 6k / 8.5k / 10.5k / 13.5k / 16k / 18k / 21k nodes respectively. This
does not make those screens full v2 yet, but it gives the redesign loop a
stable regression tripwire before deeper ownership passes.
That same route journey now proves that non-pipeline checkpoint routes do not
carry hidden Pipeline Inspect or Monitor detail shells. The v2 checkpoint root
anchors stay mounted for presentation, but the heavy detail DOM is removed until
the operator returns to Inspect or Monitor, where the shell is restored before
rendering.

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
- The shell now names whether the current route is v2-owned or a legacy
  checkpoint, reducing the “which UI am I in?” ambiguity during the staged
  rollout.
- Incidents and Telemetry are promoted into the primary workspace navigation,
  so the operator can discover alert triage and engineering counters without
  knowing hidden route URLs.
- Workspace and Pipeline tabs now support expected Arrow/Home/End keyboard
  movement, which matters more now that the primary shell has more first-class
  destinations.
- Active Workspace and Pipeline tabs now scroll back into view on narrow rails
  during direct route loads and keyboard movement, keeping later checkpoints
  like Telemetry and Status discoverable without page-wide horizontal overflow.
- Seeded Playwright/CDP coverage now repeats the narrow-rail proof with
  operator text zoom enabled on Telemetry and Monitor, so larger text does not
  silently reintroduce page-wide horizontal overflow while the operator moves
  through dense checkpoints.
- Content-driven workspace jumps now move focus to the active destination panel:
  Overview `Operate` lands in Pipeline / Operate, and Pipeline `Graph` lands in
  Inspect, while tablist navigation still keeps focus on the selected tab.
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
  progressive-disclosure affordance. The audio section also now has a local
  track search for overflowed inputs, with status-text match counts and Clear
  search recovery, so an operator can jump to Track 30 without expanding every
  row first.
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
- Pipeline Inspect now adds local output-preview search for sibling-heavy
  pipelines, so a stalled output can be compared against a named healthy sibling
  without switching context to Operate.
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
- v2 Overview Restream Activity now adds burst search only once the feed is long
  enough to justify the extra control, with CDP-visible result counts and local
  Clear activity search recovery.
- Non-pipeline v2 checkpoint routes now drop hidden Pipeline Inspect and Monitor
  detail shells while preserving their v2 checkpoint anchors, so route-specific
  CDP snapshots no longer include stale graph/control-room DOM from the previous
  operator workspace.
- Settings now opens on the operational security surface and tucks advanced
  Recording, SRT, Backend, and Profile forms behind explicit disclosure rows,
  reducing the initial form wall without hiding the path to edits. Security
  reset actions and account/logout actions are also tucked behind explicit
  disclosures, leaving Refresh visible for routine inspection.
- Shared auth expiry now preserves the full operator return path, including
  `ui=v2`, so a re-login can return to the interrupted v2 workflow instead of
  dumping the operator at the default Overview.
- Monitor now avoids eager output-preview mounts. Generic web previews and
  output HLS previews both start as explicit `Load preview` affordances, while
  the local pipeline HLS preview remains immediately available. This keeps the
  wall scan-first and avoids spending browser/media work on outputs the operator
  is not inspecting yet.
- v2 Monitor cards now tuck per-card URL maintenance actions behind an explicit
  `Show monitor actions` disclosure, so the wall opens with preview/status as
  the dominant scan layer and exposes Edit/Copy/Open only when the operator asks
  for them.

Still not a full v2 redesign:

- Inspect graph/resource details, the Monitor wall, the Incidents feed, the
  Telemetry counter grids, some Status detail sections, Media library details, and some Settings form details are still
  intentionally legacy-owned, now with lightweight route checkpoints where useful
  rather than full v2 layouts. Inspect, Monitor, Media, Settings, Incidents,
  Telemetry, and Status each have only their first v2-owned decision checkpoint.
- Legacy-owned route details still have substantial active DOM when visible.
  Under v2, dense checkpoint detail DOM and inactive Pipeline workspace shells
  are now unmounted on route exit, but active detail-section redesign remains a
  future performance/accessibility cleanup. Seeded Playwright/CDP now also
  proves the Media route remounts with populated rows after that cleanup.

## Done-state interpretation

For an rc3 merge candidate, v2 is ready as a bounded, opt-in replacement for
Overview and Pipeline / Operate. It is not yet a complete dashboard-wide v2.
The remaining evolution should be sequenced as separate ownership passes:

1. Complete Pipeline / Inspect v2 beyond the checkpoint strip.
2. Complete Pipeline / Monitor v2 beyond the checkpoint strip.
3. Incidents feed v2 and Telemetry counter-grid v2.
4. Media library, Settings form, and Status detail-section polish.
5. Hidden DOM / route unmount cleanup across legacy panels.
