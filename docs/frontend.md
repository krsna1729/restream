# Frontend

The dashboard's design system, route contract and visual baseline. Source
lives in `web/ts/` and `web/styles/input.css`; `public/js/` is generated.
The target operator experience is the
[control-room mockup](design/control-room-mockup.html) (illustrative data).

## Contents

- [1. Atmosphere & Identity](#1-atmosphere--identity)
- [2. Color](#2-color)
- [3. Typography](#3-typography)
- [4. Spacing & Layout](#4-spacing--layout)
- [5. Components](#5-components)
- [6. Motion & Interaction](#6-motion--interaction)
- [7. Depth & Surface](#7-depth--surface)
- [8. Accessibility Constraints & Accepted Debt](#8-accessibility-constraints--accepted-debt)
- [Control Room](#control-room)

## 1. Atmosphere & Identity

Restream is a compact broadcast operations console. It should feel quiet,
precise, and immediately scannable under time pressure. Its signature is
status-led density: restrained surfaces, clear state color, and media that
appears only when an operator asks for it.

## 2. Color

The console uses DaisyUI's `dim` theme. Components use semantic Tailwind and
DaisyUI tokens rather than fixed palette values.

| Role | Token | Usage |
|---|---|---|
| Page surface | `base-300` | Application background |
| Section surface | `base-200` | Primary panels |
| Item surface | `base-100` | Rows, cards, menus |
| Primary text | `base-content` | Labels and values |
| Secondary text | `base-content/70` | Supporting information |
| Border | `base-content/10` | Panel and row separation |
| Primary action | `primary` | Create and submit actions |
| Operational action | `accent` | Start, select, preview, and promote |
| Healthy | `success` | Connected and forwarding |
| Attention | `warning` | Recovering and awaiting media |
| Failure | `error` | Failed and destructive actions |
| Information | `info` | Neutral runtime notices |

Status color always carries meaning. Decorative color and gradients are not
part of the console language.

## 3. Typography

The primary stack is Aptos, Segoe UI Variable, Segoe UI, IBM Plex Sans,
Helvetica Neue, Arial, and sans-serif. The monospace stack is Cascadia Mono,
Cascadia Code, JetBrains Mono, SFMono-Regular, Consolas, Liberation Mono, and
monospace.

| Level | Size | Weight | Usage |
|---|---|---|---|
| Page title | 18px | 600 | Workspace title |
| Section title | 16px | 600 | Panel heading |
| Item title | 14px | 600 | Input, output, or monitor name |
| Body | 15px | 400 | Default console text |
| Supporting | 14px | 400 | Descriptions |
| Metadata | 12px | 400-600 | Runtime and protocol details |
| Compact label | 11px | 600 | Uppercase field labels |

Letter spacing is zero except the existing compact uppercase labels, which use
the theme's `tracking-wide` utility.

## 4. Spacing & Layout

Spacing follows Tailwind's 4px base unit. Standard panel padding is 16px,
compact row padding is 8-12px, and section gaps are 12-16px. Content is limited
to `max-w-7xl`.

The workspace owns document scroll. Monitor cards use a responsive intrinsic
grid and fixed 16:9 media frames. At 375px, controls and rows reflow to one
column without horizontal page scrolling. Stream keys and URLs may scroll or
wrap within their own bounded field.

## 5. Components

### Dashboard Section
- **Structure**: semantic section, optional header, full-width content.
- **Variants**: standard, table, compact.
- **States**: loading, empty, populated, error.
- **Accessibility**: labelled by its heading.
- **Layout**: stack; document is the scroll owner.

### Operational Row
- **Structure**: title and state, metadata, command cluster.
- **Variants**: selected, standby, disabled, disconnected.
- **States**: default, hover, focus, busy, error.
- **Accessibility**: full command labels include the affected input or output.
- **Layout**: wrapping cluster that becomes a vertical stack on narrow widths.

### Status Badge
- **Structure**: short state label and optional detail.
- **Variants**: healthy, attention, failure, neutral.
- **States**: static; color is never the only state indicator.
- **Accessibility**: readable text at WCAG AA contrast.

### Monitor Card
- **Structure**: input/output title, state, actions, 16:9 media frame.
- **Variants**: input, pipeline program, output monitor, empty.
- **States**: idle, loading, playing, offline, error.
- **Accessibility**: media controls are keyboard reachable; buttons identify
  their monitor.
- **Motion**: state feedback only.
- **Layout**: frame inside an intrinsic grid; no nested card.

### Input Manager
- **Structure**: compact input rows plus an add command.
- **Variants**: selected primary, selected backup, standby, disabled.
- **States**: loading, empty, busy, error, maximum reached.
- **Accessibility**: labels, stream keys, URLs, and actions remain available to
  keyboard and screen-reader users.
- **Layout**: stack of rows with wrapping command clusters.

## 6. Motion & Interaction

Interactions use existing DaisyUI hover, active, and focus behavior. State
transitions use 100-200ms transform or opacity changes only. Loading media may
use the existing spinner. `prefers-reduced-motion` is respected by the
framework and no decorative motion is added.

## 7. Depth & Surface

The strategy is borders plus tonal shifts. Sections use `base-200`, rows use
`base-100`, and `base-content/10` borders provide separation. Shadows are
reserved for popovers and menus. Cards use at most an 8px radius.

## 8. Accessibility Constraints & Accepted Debt

The target is WCAG 2.2 AA: 4.5:1 body text contrast, 3:1 large text contrast,
visible focus, full keyboard reachability, semantic buttons and labels, and
no color-only status. Touch targets remain at least 36px in compact operator
surfaces.

None open.

## Control Room

`/` is the dashboard: one React root (`web/ts/control-room/main.tsx`) in
`web/pages/index.html`, mounting a TanStack Router (`router.tsx`). The engine
serves the same page for unknown top-level paths, so the place is read from
the query whatever path the page was served from (`/index.html?w=operate`
included: the router matches its root route there) and a link from there
canonicalises to `/`. A nested unknown path (`/a/b?w=inspect`) is redirected
to the shell's directory by a relative `../` per level with the query kept,
since the page's asset URLs are relative (so it can sit under a proxy prefix)
and would not load from a nested path.

- Data: one store (`web/ts/control-room/store.ts`) for what is pushed or
  sampled for the whole page, plus TanStack Query for the per-view polls
  (`polled.tsx`). The store follows the
  revisioned runtime state stream (`/api/v1/state/stream`) and samples engine
  telemetry and host metrics every 2 s while a view is subscribed. Views
  read it through `useLive`; they hold no copy. Rates come from two
  successive telemetry samples of one ingest session or output (`rates.ts`);
  a counter that went down, or a failover to another input, has no rate.
  The header shows the stream's connection (`live · rev N` or
  `reconnecting`; a steady system sends no deltas, so the time since the
  last change says nothing) and the age of the oldest sampled source.
- Location: `?w=<workspace>` and, for a pipeline view, `&p=<pipeline id>`
  (`web/ts/control-room/route.ts` holds the pure rules; `router.tsx` wires
  them to TanStack Router, which owns history and link clicks); an absent or
  unknown workspace opens Fleet, and the pipeline is kept across workspaces.
  Links (`PlaceLink`) are real `href`s, and a plain click navigates in place.
  Other query parameters are kept, after `w` and `p`; a repeated parameter
  keeps its first value.
- Errors: the page renders the `#error-alert`/`#error-msg` and
  `#saving-badge` elements that `core/api.ts` reports into, so a failed
  mutation is shown, not only logged.
- Workspaces ship on live data only. Fleet: delivery counts, then degraded
  outputs grouped by cause (an upstream cause, with its stage or shard,
  covers every output it stalls; destination causes stay per output) and
  ranked by outputs affected, upstream first on a tie; wanted outputs that
  neither deliver nor carry a diagnosis count as starting, never as
  healthy. Headroom from `/metrics/system`: measured host use; egress flow
  only when shards measured it (`flowMeasured`); and, labeled as such, the
  capacity model's projection onto service centers from calibration priors
  (observe-only until Q-031 measures the coefficients). Fleet also shows the
  shared work (`uniqueStages` serving `activeLeaves`, from the capacity
  snapshot), the open-alert count with its age (the same `aggregate-alerts`
  query Incidents lists), a history of headroom (100 less the projected
  utilization: the calibration priors, raised to the deepest egress shard's
  measured queue when shards run), delivery and degraded outputs by
  layer (`LiveSnapshot.history`: one `fleet-history.ts` sample per store
  sample, kept for 15 min of this page session; Incidents reads the
  engine's own history instead; a source with nothing to say is a gap, not a
  zero), and a
  worst-first table of at most 10 pipelines that shares its rows with
  Pipelines (`pipeline-table.tsx`) and links to it for the rest. Pipelines: the
  inventory (attention first, then size), searchable by name or id and
  filterable by the same health rule as Fleet's count, with outputs by
  protocol; above it, all from the runtime state (`pipelines-model.ts`), the
  fleet-wide protocol mix, the shared stages (running outputs grouped by the
  `stage` they read, largest group first), and the operational cards (needs
  attention, warm standby, bonded SRT, recording) that narrow the table.
  Operate (one pipeline): the source deck from the runtime state's
  `inputs` (RTT from the telemetry sample's `inputs`) with Promote and each
  input's preview; the media path (`operate-graph.tsx`) in four views: Graph
  (the state's `graphs` entry with the telemetry's counters attached,
  `state-views.ts`, laid out by longest path, each shared stage drawn
  once with its consumer count and destinations folded per upstream stage
  and protocol; egress nodes join outputs on their `outputId`), Destinations
  (the same groups as a table), Timeline (the pipeline's kept lifecycle
  events, newest first) and Preview (the program). Selecting a group opens
  its inspector (`operate-group-model.ts`): delivering count, worst delivery
  ratio, worst feed lag and resyncs from the engine telemetry; the fabric
  shards its running outputs are placed on (`fabricFeed`, `shardId`), with
  state, command queue and progress age from the live store's 2 s
  `/metrics/system` sample (`egressShards`); and each diagnosed
  output's layer, cause and ruled-out layers. Then the
  destination matrix from the state stream (feed lag from the running egress,
  rate from the telemetry rates) with Start/Stop, degraded first and capped
  at 200 rendered rows, with the hidden count (and how many of them need
  attention) and a "show all" button for the rest. Inputs and graph come
  from the state stream, so an input change shows without a poll; their
  counters come from the store's 2 s telemetry sample, whose age the panels
  show (amber once older than 2.5 samples). Inspect (one pipeline): the
  resource map, polled every 2 s (process measurements, not state)
  (`/engine/resource-map?pipeline_id=…&view=detail&top_n=200`), nodes under
  pressure first, then by memory held, each with its confidence (measured,
  derived, estimated, unmeasured), a detail panel and the count of nodes the
  server left out. The detail panel shows the node's metric cards (graph
  consumers on the pipeline map, measured CPU with the FFmpeg child PID,
  input packet rate and work per packet since the node started; a number the
  node lacks is left out, not shown as zero) and the engine's bottleneck
  judgement. "Global resource map" switches to the runtime scope (no
  `pipeline_id`); there the graph is not the map's, so no consumer card. Then
  the source ring (fill and retained payload) from the
  graph and the telemetry's `sourceRings`; the egress shards this pipeline's outputs run on (the server scopes
  them by feed and shard); and the map's attribution legend. Monitor (one
  pipeline): ingest and egress rate, the share of wanted outputs delivering,
  the worst feed lag, ring fill and the slowest of its shards' progress age,
  sampled every 2 s on one clock and kept in the page for the last 15
  minutes (the engine keeps no history). A stale or missing source, a state
  stream that is not live, or a truncated map is a gap in its line, never a
  zero or a frozen value. The degraded-output count is a signal too: each
  rise from 0 the page saw is an incident onset, marked as a cursor on every
  line and listed by time (a pipeline already degraded at page load has no
  onset in view). Pause shows the window as it was while sampling goes on;
  Compare, while paused, overlays the live window dashed on the same scale.
  Below (`monitor-path.ts`): the selected input's transport (what its
  protocol reports, from the telemetry sample's `inputs`; an unreported metric is
  left out, not 0), the shared media path from the processing graph (source
  ring fill, worst reader lag, each stage with its packet rate and the
  wanted outputs reading it, reader overflows), and the destination outlier:
  the wanted output with the lowest delivery ratio under the engine's 0.95
  floor, against the median of its peers on the same stage and protocol. The
  mockup's blocked AVIO writes are not shown: that queue was deleted in #327.
  Operate, Inspect and Monitor share one pipeline
  header and picker (`pipeline-scope.tsx`). Incidents (fleet): every open
  alert (`/alerts`), critical first, then the widest scope, then the oldest;
  the selected one's lifecycle events (`/events`) from a minute before it
  opened; and its evidence packet: what failed (the diagnosis layer and
  cause), what did not (the layers ruled out), each evidence line marked
  `measured` or `derived` by the engine, and the recommended action. The
  resolved list is the engine's (`/alerts?since=`, a 15m/1h/6h/24h window),
  with a note when the window reaches past the engine's kept history.
  A correlation chart draws the engine's fleet history
  (`/fleet/history?since=&until=`, sampled every 5 s and kept 24 h, read
  from a minute before the incident opened to a minute after it closed,
  then only new samples, no longer than the engine keeps them; a resolved
  incident's span stops loading once complete) as delivering,
  headroom and degraded lines with cursors at the incident's open and
  close, with a note when the kept history starts after that minute; "Export
  evidence" downloads the alert, packet, timeline and those samples as one
  JSON file, built in the browser. Capacity (fleet): the
  `/metrics/system` capacity snapshot (load per service center, labelled
  calibration priors, observe-only), the Flow Doctor (measured from the
  busiest shard, or projected when none runs) and a scenario form whose
  baseline and projection both come from `/capacity/scenario`, priced on the
  priors from one workload sample: the page computes no capacity number. The
  scenario picks the added outputs' pipeline (whose ingest they carry; the
  highest-bitrate live one by default), protocol and encoding (new renditions
  are shared stages); Changes prices its added outputs by protocol, on its
  own pipeline's ingest when that is live, and the calibration table shows each limit and whether it
  is configured or a prior; "draft it in Changes" leads to a planned change's
  topology diff. A
  timeline says when the engine's event log no longer reaches back to a
  minute before the incident opened.

Media (fleet): the media directory (`/media`) split into recordings and
source files, newest first, with search over every stored name; the selected
file's stored forms, its probe (`/media/{file}/analysis`, run once on select)
and whether it can feed a live pipeline as it is (the engine's sparse-GOP
judgement against its live GOP target; with fewer than two keyframes the
engine measures no spacing, so none, or one in a file longer than the
target, needs Live Optimized); upload, rename and a confirmed
delete (refused while a pipeline ingests the file); and the media disk from
`/metrics/system`. A file a recording session wrote shows its timeline from
the persisted session the media list already joins (`recordings` table)
and the remux conversion state: the start; at stop only the `converting`
state is written, so its time is the stop; the session's `endedAt` is when
the browser MP4 was written, not the stop; a failed remux is one failure
with the `.ts` kept; a step with no recorded time shows none. A row is
selected by session id, so it stays selected when its `.ts` becomes the
`.mp4`; a rename breaks the session join (by stored path), so a renamed
recording keeps its file but loses its timeline. "Use as source" opens
the new-pipeline form with the file as a looping file source, Live
Optimized when the probe says the GOP needs it, and goes to the new
pipeline's Operate view. System (fleet, `/engine/health` every 10 s): the host
checks (a warning first), kernel io_uring and TLS capability, the execution
classes (running media workers of those configured, RTMP and SRT ingress
owners, egress fabric shards and FFmpeg children as the engine reports them;
Tokio workers as configured), the SRT ingress owners' counters with every
non-zero one that means lost or late work since the engine started, and the
build, native libraries and SBOM from `/engine`. Settings (fleet): reads
`/settings` once (it also lists every pipeline and output, so it is never
polled); each card saves only its own part (general names, SRT ingest with
the 10–79 byte passphrase rule, transcode profiles with the built-ins kept,
failed-attempt limits, the dashboard password) and says when the engine
stored it. The failed-attempts list is polled; forgetting one client sends
its own scope key, and forgetting all sends no scope (the engine rejects any
other scope with 400). Log out is on the password card. Changes (fleet,
the [agent plane](api-reference.md#agent-plane)): one pipeline, one or more
destination changes (add, change, start, stop, remove) and a reason. Preview
asks the engine to judge them (`/agent/plans`, which mutates nothing) and is
shown only while the form still matches what was judged; a valid preview
creates an operation from that same request, which the operator approves
(as this dashboard session, with an audited reason: confirmation, not a
second person's sign-off), applies and verifies. A valid preview also shows
the impact: the plan's topology diff (outputs wanted running by stage and
protocol, before and after, marking a stage the change starts and an add
that reuses a stage) and, when the change adds outputs or stages, the
capacity scenario (`/capacity/scenario`) for those additions, before and
after on the calibration priors (removals are counted, not credited). The
engine's four fixed gates decide "verified" (desired state, the outputs it
started are running, no new critical alert on the changed pipelines since the
apply, and the change did not push the host past its safe utilization); the per-change checks are
listed under them, and the alert delta and active graph nodes are shown as
observations. The operation card's convergence timeline merges the audit
events and each output change the apply made, in time order from creation,
and ends with where the changed outputs stand in the live state. Verification checks one moment, so a failed one
can run again (a just-started destination needs seconds to connect). A
refused apply answers 400 with the failed record; the page reads the record
back and shows the reason, and warns that earlier changes may already be
applied. The tab keeps the operation id in `sessionStorage` and reads the
operation back after a reload; operations live in engine memory only, so
after a restart (404 `operationNotFound`) the page starts over.

Managing pipelines (`operate-manage.tsx`, `pipeline-form.tsx`,
`output-form.tsx`, rules in `manage-model.ts`): Pipelines has New pipeline,
and the new pipeline's Operate opens once the engine stored it. Operate
reads `/settings` once and again after each edit, and adds the pipeline
controls: Record (armed until a source arrives; the summary's `enabled` and
`active`), play or stop a configured file source (whether it runs is polled
from `/pipelines/{id}/file-ingest`: a file can end or fail), a confirmed
delete, and the settings form (name, a free stream key on create, "Issue a
new stream key" on edit, locked while any destination runs as in the classic
editor, live or file source with its ingest options, and the per-pipeline SRT
ingest override). An edit re-sends the source only when it changed, because
re-sending a file source restarts its ingest. The source deck adds backup
inputs with their publish URLs, and enables, disables or deletes them; the
primary and the selected input offer neither, as the engine refuses both. An
input's source is either a stream key (live push over RTMP, SRT or HLS) or a
remote HLS URL the engine pulls, with a rendition rule (highest or lowest
bandwidth) and one credential choice: none, a bearer token, a pasted cookie
string, a username and password, or custom headers. A signed URL needs no
credential (its token is in the URL) and origin cookies are carried
automatically, so `none` is the usual choice; a stored secret is written once
and shown only as state, never as a value, and a save that does not touch it
leaves it in place.
The destination matrix adds, edits and deletes destinations: a preset server
plus key or a full URL, SRT as fields, the RTMP flavour from the preset (or
chosen for a custom server), video passthrough or a profile (stored custom
video settings are kept unless replaced), and audio routing with notes from
the destination's caps: what is dropped or downmixed, and which tracks or
channels exceed its limits. The caps are advice (the engine does not
enforce them), so they never stop a save. The source's tracks come from
`/pipelines/{id}/probe` when someone publishes; the pickers always include a
stored track or channel.

Logs (`logs.tsx`, rules in `diagnose-model.ts`): the newest 200 persisted
rows (`/logs`) for everything, engine-wide rows only, one pipeline (with its
destinations' rows) or one destination, from a lowest level, optionally
lifecycle events only; then every matching row live from `/logs/stream`,
which resumes after the newest row read, so no row is lost or shown twice.
History and stream use one filter set. The page keeps 500 rows, newest
first; Pause live closes the stream and keeps the rows shown, and Resume
reopens it after the newest row shown, so it delivers what the pause missed.
A failed history read says so and offers Read again. Inspect adds Diagnostics: the engine's
checks on the pipeline's live input (`POST /pipelines/{id}/diagnostics/run`)
on request, each with its result, issues, help and output, and the run
downloads as a `restream.diagnostics.v1` report.

Previews (`preview.tsx`): the media path's Preview view plays the program
(`/hls/{pipeline}/master.m3u8`) and the source deck each connected input (its `previewUrl`)
muted through hls.js, only when asked, because a preview makes the engine
segment that stream. The engine answers 404 until the first segment exists,
so the player retries 404s for up to 15 s (Safari's own player does not
retry, so the page polls the playlist first). A stream whose buffered media
has not grown for 20 s is stalled: a stopped source raises no player error,
its playlist only stops growing. The preview then stops and offers Retry; a
wait for data reads "buffering", never "live". A 401 goes to the login page.
More than one audio track adds a track picker. Inspect's detail for a
selected stage adds its pipe counters (`/stages/{key}/telemetry`), which the
resource map leaves out, and says so when the engine no longer has the stage.

Proof: `test/frontend/control-room-fleet.test.mjs`,
`test/frontend/control-room-fleet-history.test.mjs`,
`test/frontend/control-room-monitor.test.mjs`,
`test/frontend/control-room-media.test.mjs`,
`test/frontend/control-room-operate.test.mjs`,
`test/frontend/control-room-inspect.test.mjs`,
`test/frontend/control-room-incidents.test.mjs`,
`test/frontend/control-room-system.test.mjs`,
`test/frontend/control-room-settings.test.mjs`,
`test/frontend/control-room-changes.test.mjs`,
`test/frontend/control-room-manage.test.mjs`,
`test/frontend/control-room-diagnose.test.mjs`,
`test/frontend/frontend-runtime-state-stream.test.mjs` (stream liveness) and
`test/frontend/control-room.spec.ts` (against a running engine).
