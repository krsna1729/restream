# Component build seam

## Contents

- [Objective](#objective)
- [Contract](#contract)
- [Measurement](#measurement)
- [Findings](#findings)
- [Typed Overview view-model follow-up](#typed-overview-view-model-follow-up)
- [Complete read-only Overview](#complete-read-only-overview)
- [Pipeline Operate selector](#pipeline-operate-selector)
- [Selected pipeline header](#selected-pipeline-header)
- [Selected input and preview status](#selected-input-and-preview-status)
- [Selected output overview](#selected-output-overview)
- [Output launch actions](#output-launch-actions)
- [Attention remediation action](#attention-remediation-action)
- [Attention launch actions](#attention-launch-actions)
- [React output cards](#react-output-cards)
- [Pipeline lifecycle controls](#pipeline-lifecycle-controls)
- [Selected input metrics](#selected-input-metrics)
- [Live-source configuration](#live-source-configuration)
- [File-source details](#file-source-details)
- [Audio track editor](#audio-track-editor)
- [HLS preview host](#hls-preview-host)
- [Overview large-fleet search](#overview-large-fleet-search)
- [Pipeline Inspect checkpoint](#pipeline-inspect-checkpoint)
- [Pipeline Monitor checkpoint](#pipeline-monitor-checkpoint)
- [Incidents checkpoint](#incidents-checkpoint)
- [Shared checkpoint card](#shared-checkpoint-card)
- [Checkpoint bundle split](#checkpoint-bundle-split)

## Objective

Prove that an opt-in React/Vite island can coexist with the embedded legacy
dashboard without changing default behavior, routes, API ownership, refresh
cadence, or stable release asset names.

## Contract

- The legacy dashboard remains the default and continues to boot from
  `public/js/app/dashboard-entry.js`.
- `?mode=overview&ui=v2` preserves the canonical Overview location and loads
  `public/js/app/dashboard-v2-entry.js` dynamically. Checkpoint-only pipeline
  routes load `public/js/app/dashboard-v2-checkpoints-entry.js`; both share the
  stable `public/js/app/dashboard-v2-jsx-runtime.js` chunk.
- The experimental React root receives a typed, read-only Overview model. It
  does not fetch data, read shared state, own URL state, subscribe to lifecycle
  events, or own mutations. Under the explicit flag it replaces the Overview
  presentation only; the legacy renderer remains the unflagged default.
- The existing frontend preparation script remains the sole owner of the
  generated `public/` tree. Vite does not copy or clear that directory.
- Both legacy TypeScript and the TSX island are type-checked during the normal
  frontend build.

## Measurement

Measurements were taken from the same worktree before and after the seam.

| Artifact or path | Before | After |
|---|---:|---:|
| Legacy entry, raw | 166 B | 367 B |
| Legacy entry, gzip | 130 B | 199 B |
| Flag loader, raw | 0 B | 370 B |
| Flag loader, gzip | 0 B | 240 B |
| Opt-in React bundle, raw | 0 B | 258,982 B |
| Opt-in React bundle, gzip | 0 B | 66,581 B |
| Shared CSS, gzip | 28,265 B | 28,298 B |

The default route does not request the React bundle. Its measured bootstrap
delta is 309 gzip bytes across the entry and loader, plus one small module
request. The opt-in bundle started with a 75,000-byte gzip guardrail; after the
large-fleet Overview search slice, the current smoke guard is 76,000 bytes with
the measured rationale recorded below.

The synthetic DOM-operation benchmark remained unchanged for 125 outputs over
100 refreshes:

- stable telemetry: zero optimized DOM writes;
- live telemetry: 1,761 optimized text writes and zero subtree rewrites;
- naive comparison: 101 list rewrites and 100 subtree rewrites.

## Findings

The build and release seam is viable, including base query preservation,
browser execution, generated-asset ownership, Docker inputs, and release asset
checks. Two integration hazards were caught during the experiment:

1. Vite's default public-directory copy recursively targeted the nested output
   directory; `publicDir: false` leaves ownership with the existing preparer.
2. Library mode left React's environment branch unresolved until the build
   explicitly defined production mode.

React is not justified as the default merely because the seam works. Its
66.6 KB gzip floor must be amortized by simpler ownership and tests. The first
typed feature input and its updated measurement are recorded in
the follow-up below; `core/api.ts`, the URL contract, polling, and lifecycle
coordination remain outside the component boundary.

## Typed Overview view-model follow-up

The first real component input preserves one-way ownership:

```text
existing dashboard refresh and lifecycle handling
  -> core state
  -> features/overview-view-model.ts
  -> app/dashboard-v2-loader.ts
  -> opt-in React renderer
```

The feature module now owns the pure fleet-count and attention derivation used
by both renderers. The app publishes a model only when `ui=v2` was present at
boot, and the loader retains the latest model across its asynchronous bundle
import. React does not import `core/api.ts`, read shared state, fetch, poll,
subscribe, mutate, or own URL state.

Source tests cover healthy, retrying, intentionally stopped, probing, and
flapping states. The seeded browser test proves the mixed-health model renders
two pipelines, two live inputs, one of two outputs running, and one pipeline
needing attention. Existing unflagged visual, keyboard, ARIA, and axe baselines
remain unchanged.

Compared with the inert seam:

| Artifact | Inert seam | Read-only model | Delta |
|---|---:|---:|---:|
| Legacy entry, gzip | 199 B | 199 B | 0 B |
| Flag loader, gzip | 240 B | 319 B | +79 B |
| Opt-in React bundle, gzip | 66,581 B | 67,132 B | +551 B |

The 125-output, 100-refresh DOM benchmark remained unchanged: zero optimized
writes for stable telemetry, 1,761 text writes and zero subtree rewrites for
live telemetry, versus 101 list rewrites and 100 subtree rewrites in the naive
comparison.

The typed model boundary is justified independently of React because it gives
both renderers one tested definition of fleet state. The component is viable,
but this small summary does not reduce enough ownership to make React the
default. Proceed with only the tokens and primitives needed for a complete
read-only Overview behind `ui=v2`; keep the existing runtime as producer and
stop if that slice duplicates polling, lifecycle, URL, or mutation
orchestration.

## Complete read-only Overview

The `ui=v2` slice now renders the complete Overview surface: current priority,
six fleet signals with existing metric history, the pipeline comparison table,
and grouped Restream Activity. A deliberately small primitive set (`Panel`,
`StatusBadge`, `MetricCard`) and semantic tone maps live with this single
component until another migrated surface proves that a shared design-system
module would remove real duplication.

Ownership remains one-way:

```text
modes.ts polling, SSE, activity grouping, and metric history
  -> typed Overview presentation input
  -> pure overview-view-model.ts projection
  -> React presentation
  -> app-owned callbacks for Add, Operate, Inspect, and Status
```

React owns no timers, requests, global state, URLs, or mutations. The existing
app translates action identifiers into its established navigation and editor
flows. `modes.ts` skips legacy Overview markup only for the explicit flag while
continuing to own refresh and lifecycle behavior. Under `ui=v2`, the hidden
legacy Overview container is emptied so the active React Overview is the only
fleet-summary subtree. This removes double rendering without creating a second
runtime.

The full slice measures 272,694 raw bytes and 70,080 gzip bytes in Vite's
production report, below the existing 75,000-byte gzip guardrail. The compiled
flag loader is 953 raw bytes (373 bytes with deterministic gzip); the default
entry remains 367 raw bytes (197 bytes with deterministic gzip). Seeded browser
coverage proves that the legacy Overview is hidden only under `ui=v2`, the
mixed-health states and activity render, hidden legacy Overview content is
empty, Add Pipeline opens the existing editor, and Operate uses the established
pipeline route.

The architecture decision is now positive but narrow: React is justified for
new or migrated presentation slices when it consumes a typed model and
delegates effects through the app boundary. It is not yet justified as a
wholesale rewrite or as a second data/runtime layer. The next useful slice is
one pipeline workflow selected by operator value, using the same ownership
contract and measured independently.

## Pipeline Operate selector

The first Pipeline Operate slice migrates the selection and entry workflow:
the sorted pipeline list, semantic health, input/output rates, output counts,
current selection, and Add action. Under `ui=v2`, React replaces only the
legacy selector inside the existing Operate grid. The selected pipeline detail,
input preview, recording and file-ingest intents, audio labels, output cards,
and all mutations remain with their established owners.

The boundary is deliberately narrow:

```text
render.ts selection reconciliation and dashboard refresh
  -> pure pipeline-operate-view-model.ts projection
  -> dashboard-v2 loader
  -> React selector
  -> app-owned selectPipeline and addPipeBtn callbacks
```

This removes the legacy selector rewrite only for the experiment without
creating a second URL owner or data subscription. Under `ui=v2`, the hidden
legacy selector row list is emptied so CDP/node growth reflects the active v2
rail rather than stale duplicate navigation. The default route keeps its legacy
selector and does not load the v2 bundle. Source tests cover ordering,
health/rates, valid selection, and stale selection removal. The seeded browser
flow proves accessible current selection, canonical `p=` navigation, unchanged
legacy detail rendering, empty hidden legacy selector rows, and delegation to
the existing Add Pipeline editor.

The opt-in bundle moved from 272,694 bytes raw and 70,080 bytes gzip in Vite's
report to 275,599 bytes raw and 70,560 bytes gzip. The 480-byte gzip increase
remains below the 75,000-byte guardrail. The 125-output, 100-refresh benchmark
is unchanged: zero optimized writes for stable telemetry and 1,761 text writes
with zero subtree rewrites for live telemetry.

The stop boundary is intentional. Migrating the detail or output columns next
requires a dedicated typed model and delegated intent contract for one owner
at a time; copying their async caches or pending-mutation maps into React would
create the second runtime this migration is designed to avoid.

## Selected pipeline header

The next `ui=v2` slice migrates the selected pipeline's read-only identity and
status header: name, semantic health, source, input/output rates, output count,
and recording state. React also presents Graph, Diagnose, and Edit, but those
buttons delegate to the existing app-owned graph, diagnostics, and editor
flows. The default route retains the complete legacy header.

The ownership boundary remains asymmetric by design:

```text
pipeline-view.ts refresh and selected-pipeline lookup
  -> pure pipeline-operate-view-model.ts projection
  -> dashboard-v2 loader
  -> React header
  -> app-owned Graph, Diagnose, and Edit callbacks
```

React does not gain an API client, timer, URL owner, or mutation state. Record,
File Ingest, History, Delete, preview/audio behavior, output cards, and their
pending intent maps remain in the legacy detail owner. This avoids splitting a
single mutation lifecycle between renderers.

Source tests cover live RTMP and offline file-source presentations, recording
state, action availability, and stale selection. Seeded browser coverage proves
that the v2 header replaces only legacy identity and its duplicate
Graph/Diagnose/Edit launchers, while Record remains available and Edit opens
the established pipeline editor.

The opt-in bundle moved from 275,599 bytes raw and 70,560 bytes gzip in Vite's
report to 278,088 bytes raw and 70,870 bytes gzip. The 310-byte gzip increase
remains below the 75,000-byte guardrail. The 125-output, 100-refresh benchmark
is unchanged: zero optimized writes for stable telemetry and 1,761 text writes
with zero subtree rewrites for live telemetry.

The next boundary should remain read-only. Input/preview status is a candidate
only if it can consume a typed presentation model while leaving preview setup,
file-ingest intent, audio-label editing, and every mutation lifecycle with the
existing owner.

## Selected input and preview status

The next `ui=v2` slice adds a read-only selected-input summary for operator
state, publisher link and quality, browser-preview readiness, video shape,
audio-track count, uptime, and unexpected readers. The projection is pure and
is published from the existing selected-pipeline refresh path.

```text
pipeline-view.ts selected-pipeline refresh
  -> pure pipeline-operate-view-model.ts input projection
  -> dashboard-v2 loader
  -> React input-status presentation
```

Only the legacy publisher badge row is replaced under the experiment flag.
The existing HLS player still owns preview setup, readiness retries, playback,
and audio selection. Detailed video/audio stats, audio-label editing, recording,
file-ingest intent, source details, ingest URLs, and all mutation lifecycles
remain in the legacy feature owner. No API, timer, URL, cache, or pending-intent
map moved into React.

Source tests cover live publisher/preview/media state and offline failure state.
The seeded browser proof verifies the React input summary while also asserting
that the established preview player and detailed stats remain visible. The
unflagged route keeps the legacy publisher presentation.

The opt-in bundle moved from 278,088 bytes raw and 70,870 bytes gzip in Vite's
report to 281,591 bytes raw and 71,290 bytes gzip. The 420-byte gzip increase
remains below the 75,000-byte guardrail. The 125-output, 100-refresh benchmark
is unchanged: zero optimized writes for stable telemetry and 1,761 text writes
with zero subtree rewrites for live telemetry.

The next useful boundary is the read-only output overview. Output start/stop,
edit/delete, monitor/history launch, retries, and every optimistic or pending
mutation state should remain app- or legacy-owned until each delegated action
contract is proven independently.

## Selected output overview

The `ui=v2` Operate slice now includes a read-only output rollup: active and
total counts, aggregate bitrate, semantic status buckets, and a prioritized
list of outputs needing attention. The pure model uses the same output-status
contracts as the legacy cards but does not read output-control intent state.

```text
pipeline-output-list.ts selected-pipeline refresh
  -> pure pipeline-operate-view-model.ts output projection
  -> dashboard-v2 loader
  -> React output overview
```

Under the experiment flag, this replaces only the legacy rollup and attention
summary. The keyed output cards and bottom Add Output action remain visible and
continue to own start/stop, retry convergence, optimistic and pending state,
monitor/history launch, edit/delete, list expansion, and delegated event
handling. The default route retains the full legacy output presentation.

Source tests cover running, retrying, and intentionally stopped outputs,
aggregate bitrate, status ordering, and attention priority. The seeded browser
flow proves a retrying rollup, then a healthy rollup after pipeline selection,
while asserting that the established output cards and Add Output action remain
available.

The opt-in bundle moved from 281,591 bytes raw and 71,290 bytes gzip in Vite's
report to 284,738 bytes raw and 71,670 bytes gzip. The 380-byte gzip increase
remains below the 75,000-byte guardrail. The 125-output, 100-refresh benchmark
is unchanged: zero optimized writes for stable telemetry and 1,761 text writes
with zero subtree rewrites for live telemetry.

The next boundary needs a deliberate action contract rather than more summary
markup. Launch-only output actions can be delegated first; start/stop and delete
should not move until their busy, convergence, and optimistic-state contracts
can remain single-owned.

## Output launch actions

The output overview now delegates two launch-only actions through the app
composition root. Add Output opens the established output editor for the
selected pipeline, and History on an attention item opens the established
output-history dialog with that output's identity. React owns only the buttons
and passes stable identifiers to app-owned callbacks.

```text
React output overview button
  -> typed dashboard-v2 loader action
  -> dashboard-app composition callback
  -> existing editor or history controller
```

Under `ui=v2`, the React Add Output button replaces the duplicate bottom Add
Output button. The keyed legacy output cards remain visible and retain all
start/stop, retry convergence, busy and optimistic state, monitor, history,
edit/delete, and list-expansion ownership. History in the React attention list
is an additional contextual launcher, not a second history implementation.
The default route is unchanged.

The seeded browser flow opens both established dialogs from React, verifies the
selected output identity in History, and confirms that the legacy cards remain
visible while only the duplicate Add launcher is hidden. The opt-in bundle
moved from 284,738 bytes raw and 71,670 bytes gzip in Vite's report to 285,233
bytes raw and 71,740 bytes gzip. The 70-byte gzip increase remains below the
75,000-byte guardrail. The 125-output, 100-refresh DOM benchmark remains the
same: zero optimized writes for stable telemetry and 1,761 text writes with
zero subtree rewrites for live telemetry.

The next safe mutation proof is a contextual action on the bounded attention
list. It must consume the established pending intent and keep API mutation,
optimistic state, retry convergence, and cleanup in their existing owner.

## Attention remediation action

The `ui=v2` attention list now exposes the existing Stop lifecycle as a
contextual remediation action. React sends only pipeline ID and output ID
through the typed loader contract. The app composition root invokes the
established editor controller and republishes the output overview
before and after the promise so React reflects the controller's authoritative
pending intent.

```text
React attention action
  -> app-owned stopOutBtn delegation
  -> existing pending-intent and duplicate-click guard
  -> existing API mutation and local desired-state patch
  -> existing SSE or fallback-poll convergence
  -> output overview projection of busy intent
```

React owns no mutation promise, retry timer, optimistic cache, API call, or
convergence predicate. The legacy output cards remain visible with their full
Start/Stop controls, so every output remains operable; this slice adds a direct
remediation path only for the bounded set already needing attention. A full
toggle migration intentionally stops here because rendering all outputs in
React on every refresh would require a separate 125-output measurement and a
replacement for the legacy card-limit/expansion contract.

Source tests prove normal and stopping intent projections. The seeded browser
flow delays the existing stop response, observes the disabled `Stopping...`
state, then supplies the converged runtime snapshot and verifies the output
becomes intentionally stopped. The opt-in bundle moved from 285,233 bytes raw
and 71,740 bytes gzip in Vite's report to 285,702 bytes raw and 71,800 bytes
gzip. The 60-byte gzip increase remains below the 75,000-byte guardrail. The
125-output, 100-refresh benchmark is unchanged: zero optimized writes for
stable telemetry and 1,761 text writes with zero subtree rewrites for live
telemetry.

The next output mutation step should wait for an explicit all-output React
refresh benchmark and a design that preserves the card limit without duplicate
controls. Launch-only Monitor and Edit delegation can advance independently
without widening mutation ownership.

## Attention launch actions

The attention list now delegates Monitor and Edit through the same typed app
boundary as History and Stop. The pure model exposes only whether monitoring is
available; it does not expose or interpret the monitoring URL. The app callback
resolves the current output by stable IDs and hands its URL to the established
monitoring normalizer and sized-popup flow. Edit delegates the same IDs to the
existing output editor.

```text
React attention item
  -> typed Monitor or Edit callback with pipeline/output IDs
  -> dashboard-app current-state lookup or editor delegation
  -> existing URL normalization/popup or output modal
```

No monitoring player, URL policy, editor form state, validation, or submission
logic moved into React. Monitor appears only when the current output model has a
monitoring URL; Edit launches the established modal but all eventual config
mutation behavior remains editor-owned. The complete legacy output cards remain
available for outputs outside the bounded attention list.

Source tests prove the monitoring affordance projection. The seeded browser
flow records the normalized popup URL and verifies the edit dialog title for
the selected output. The opt-in bundle moved from 285,702 bytes raw and 71,800
bytes gzip in Vite's report to 286,212 bytes raw and 71,850 bytes gzip. The
50-byte gzip increase remains below the 75,000-byte guardrail. The 125-output,
100-refresh benchmark remains unchanged: zero optimized writes for stable
telemetry and 1,761 text writes with zero subtree rewrites for live telemetry.

With every safe contextual action delegated, the next meaningful decision is
whether to build and benchmark a bounded React output-card replacement. Delete
and full Start/Stop ownership should not move independently of that card-level
design.

## React output cards

The opt-in pipeline workspace now replaces the legacy output-card list with
React cards. The pure operate model projects at most eight cards by default and
retains the existing Show all/Show less contract. Expansion state remains in
the output-list feature, so changing presentation technology did not create a
second source of list state.

```text
output-list feature state and control snapshots
  -> pure bounded card model
  -> React card presentation
  -> typed action callbacks with pipeline/output IDs
  -> existing editor, confirmation, monitoring, and history owners
```

Start, Stop, and Delete still use the established editor controller. React does
not own pending-intent maps, duplicate-click guards, API calls, optimistic
config patches, confirmation state, lifecycle SSE convergence, or fallback
polling. The app composition root republishes the card model around lifecycle
mutations. Monitor, History, and Edit continue through the same launch-only
delegations. The legacy list and its toolbar are hidden and emptied under
`ui=v2`, eliminating duplicate controls; the default UI remains unchanged.

The redacted URL and duration helpers moved into a small pure core display
module and remain re-exported by the legacy utility module. This keeps the
React graph independent of window-bound compatibility handlers while
preserving existing imports.

Source tests prove the eight-card boundary, expansion state, redacted card
projection, control labels, and legacy-list replacement. The seeded browser
flow proves Stop, Start, Monitor, History, Edit, and Delete confirmation against
the established controllers. A browser-native 125-output, 100-refresh guard
measured 1,666.4 ms for stable refreshes with zero DOM mutations and 1,666.5 ms
for live refreshes with 12,500 text updates, zero attribute changes, and zero
subtree/list rewrites. The opt-in bundle moved from 286,212 bytes raw and 71,850
bytes gzip in Vite's report to 288,731 bytes raw and 72,180 bytes gzip. It
remains below the 75,000-byte gzip guardrail.

The next milestone should move the remaining pipeline-level legacy controls
only where a complete React replacement can preserve their controller owners.
Recording and file-ingest controls are the next coherent card-level boundary;
isolated button moves would recreate duplicate ownership.

## Pipeline lifecycle controls

The opt-in pipeline header now replaces the legacy recording and file-ingest
buttons with React controls. Their lifecycle ownership remains in
`pipeline-view.ts`: React delegates only the pipeline ID, while the existing
controller resolves current state, guards duplicate clicks, invokes the API,
patches dashboard state, waits for runtime convergence where required, and
clears pending intent in `finally`.

```text
pipeline-view controller state and pending intents
  -> pure header lifecycle-control projection
  -> React button presentation
  -> typed callback with pipeline ID
  -> existing recording or file-ingest controller
```

File ingest is projected only for a configured file-source pipeline. Recording
continues to enforce the established input-availability rule, and Edit remains
disabled while recording is active. Under `ui=v2`, the two legacy controls are
hidden to avoid duplicate actions; the default dashboard is unchanged and its
buttons now call the same exported controller functions.

Source tests prove normal and pending control projections plus the established
recording/file-ingest mutation contracts. The seeded browser flow delays each
mutation, observes disabled Starting/Stopping labels, verifies the converged
state, and confirms the legacy buttons stay hidden. The opt-in bundle moved
from 288,731 bytes raw and 72,180 bytes gzip in Vite's report to 289,550 bytes
raw and 72,300 bytes gzip. The 120-byte gzip increase remains below the
75,000-byte guardrail.

The next coherent boundary is the remaining legacy input-detail surface. It
should move only as a complete presentation replacement that leaves preview
setup, media metadata/analysis caches, audio-label editing, and lifecycle
refresh ownership in their current feature modules.

## Selected input metrics

The existing React input card now includes the selected pipeline's traffic and
video details: input/output bitrate, reader/output counts, codec, resolution,
frame rate, profile, level, PID, and multi-video selection when available. The
pure operate model derives these values from the dashboard snapshot already
published by `pipeline-view.ts`; React adds no polling, media inspection, or
runtime state.

Under `ui=v2`, the duplicate legacy traffic and video grids are hidden. Their
individual DOM nodes remain in the default dashboard, and the surrounding
legacy input owner continues to mount the HLS player and editable audio-track
table. This keeps playback retries, audio-label drafts and focus, and every
media lifecycle operation outside the React boundary.

Source tests cover the complete metric projection, including PID and selected
video track, and prove offline inputs omit live-only metrics. The seeded browser
flow verifies the React metrics, hidden duplicate grids, visible preview, and
visible audio editor. The opt-in bundle moved from 289,550 bytes raw and 72,300
bytes gzip in Vite's report to 290,379 bytes raw and 72,400 bytes gzip. The
100-byte gzip increase remains below the 75,000-byte guardrail.

The next input boundary is source configuration: masked stream credentials and
protocol-aware publish URLs for live sources, plus cached metadata and analysis
for file sources. That slice must retain protocol selection, clipboard effects,
and asynchronous media caches in their established feature owner.

## Live-source configuration

The React input card now replaces the legacy stream-key and publish-URL panels
for live sources. Its pure model contains only masked display values and the
available RTMP/SRT choices. Protocol selection and clipboard writes delegate
through typed app callbacks to `pipeline-view.ts`, which continues to resolve
the current pipeline, own selected-protocol state, copy the unmasked value, and
publish the next presentation model.

Under `ui=v2`, both legacy credential panels are hidden; the default dashboard
is unchanged. Source tests prove masking, protocol availability, and selection.
The seeded browser flow switches to SRT, observes the selected state and masked
URL, and verifies the delegated copy affordance. The opt-in bundle moved from
290,379 bytes raw and 72,400 bytes gzip in Vite's report to 292,314 bytes raw
and 72,680 bytes gzip, remaining below the 75,000-byte guardrail.

The remaining source slice is file metadata and analysis. It should consume the
existing cache snapshots without moving media-library requests, stale-response
guards, or file-ingest lifecycle state into React.

## File-source details

The React input card now replaces the legacy file-source panel with filename,
container, size, modification time, loop/start settings, live optimization,
codec, frame rate, duration, GOP analysis, and sparse-GOP guidance. The
presentation model is assembled only after `pipeline-view.ts` reads its existing
metadata and analysis caches; cache misses still schedule the established API
helpers, and their guarded completion rerenders the selected pipeline.

React owns no media request, cache, stale-response check, or file-ingest state.
The seeded browser proof observes the asynchronously loaded MP4 metadata and
sparse-GOP warning while confirming the legacy panel remains hidden under
`ui=v2`. The opt-in bundle moved from 292,314 bytes raw and 72,680 bytes gzip in
Vite's report to 293,532 bytes raw and 72,810 bytes gzip. The 130-byte gzip
increase remains below the 75,000-byte guardrail.

With source configuration migrated, the remaining legacy input owner is the
interactive preview and audio-label editor. Those should move only with an
explicit state-preservation design for playback retries, track selection,
draft text, focus, and keyboard behavior.

## Audio track editor

The React input card now replaces the legacy audio-track table under `ui=v2`.
It presents each track's friendly label, PID/language identity, codec, sample
rate, channel count, and profile, plus the established inline rename flow.
`pipeline-view.ts` remains the state owner: it resolves stable track keys,
loads and stores friendly labels, owns edit/draft state, and republishes the
presentation model after edit, save, or cancel actions.

React delegates only track-keyed edit events. Draft changes update the existing
controller map without forcing a render, so periodic dashboard publications do
not replace the focused input. Enter saves, Escape cancels, and both paths
return to the read-only track row. Under `ui=v2`, the legacy heading and table
are hidden; the default dashboard continues to render and operate the existing
table unchanged.

Source tests preserve the optional view-model contract and the existing legacy
label persistence coverage. The seeded browser flow proves the legacy input
stats wrapper is fully hidden, the hidden legacy audio table is emptied under
`ui=v2`, rename autofocus works, Enter persists the new label, and Escape
discards a later draft. The opt-in bundle moved from 293,532
bytes raw and 72,810 bytes gzip in Vite's report to 296,767 bytes raw and
73,320 bytes gzip. It remains below the 75,000-byte gzip guardrail.

The final input milestone is the HLS preview. It must preserve the existing
player mount, controls, retry, and teardown behavior while ensuring React owns
the host boundary rather than a second media lifecycle.

## HLS preview host

The complete browser-preview widget now mounts inside the React input card
under `ui=v2`. React owns whether and where the host exists; the established
`input-preview.ts` feature remains the bounded widget owner for video creation,
manifest readiness polling, HLS.js/native playback, retry state, alternate
audio selection, overlay controls, and teardown. The app composition root
exposes only typed mount and clear capabilities, so neither React nor its view
model depends on HLS.js internals.

Pipeline changes, offline transitions, and React unmounts call the existing
cleanup path. That path aborts pending listeners, marks the video disposed,
pauses playback, destroys the HLS instance, removes the media source, clears
the host, and removes any body-level audio menu. Repeated publications for the
same active pipeline remain idempotent and do not replace the player. The
legacy `#video-player` stays hidden and empty under `ui=v2`; the default
dashboard still mounts the same widget there unchanged.

The seeded browser flow proves the complete preview appears only in the React
card. The dedicated live HLS suite proves real manifest/media loading in that
host and verifies disposal when selection moves to an offline pipeline; its
existing coverage continues to prove retries, idempotence, cleanup, playback,
playlist advancement, and alternate-audio switching. The opt-in bundle moved
from 296,767 bytes raw and 73,320 bytes gzip in Vite's report to 297,399 bytes
raw and 73,470 bytes gzip, remaining below the 75,000-byte guardrail.

This completes the selected-pipeline Operate migration for the opt-in React
surface. The legacy dashboard remains the default, and the experiment still
shares the existing state, polling, controller, API, and media-lifecycle
owners rather than introducing a second application runtime.

## Overview large-fleet search

The v2 Overview table now shows a pipeline-name search when the fleet is large
enough to become a scan burden. This keeps small fleets quiet while giving MSR
or other multi-pipeline runs the same narrowing affordance already added to the
Operate rail and output destinations. The no-result state is explicit and
announced through status text, so keyboard and assistive-technology users do
not land on an empty table without explanation.

The implementation intentionally limits the Overview search to pipeline names.
The richer state/rate filters remain in Pipeline / Operate, where the operator
is already diagnosing a selected pipeline. This keeps the fleet summary as a
low-cognitive-load entry point instead of turning it into another dense
filtering console.

Clean `HEAD` before this slice measured 307,212 raw bytes and 74,994 bytes with
deterministic gzip for `dashboard-v2-entry.js`, leaving only six bytes below the
old 75,000-byte seam budget. The search slice plus shorter output empty-state
copy measures 309,349 raw bytes and 75,237 bytes with deterministic gzip, so the
explicit smoke guard is now 76,000 bytes. The guard remains narrow and should
force the next material UI slice either to pay down bundle weight or to make a
deliberate code-splitting decision.

## Pipeline Inspect checkpoint

Pipeline Inspect now has a first v2-owned checkpoint strip above the legacy
graph/resource panels. The checkpoint summarizes pipeline health, input/output
scope, graph readiness, attention count, and the suggested diagnostic next step,
while the existing graph explorer, resource attribution, diagnostics modal, and
output-preview search remain legacy-owned underneath it.

This is intentionally a partial ownership step rather than a full Inspect
rewrite. It moves the operator's first decision point into the React seam
without duplicating graph rendering or resource-map logic. The seeded
Playwright/CDP proof covers Overview → Inspect navigation, visible checkpoint
content, Operate/Diagnostics actions, route ownership text, and the existing
Inspect output-search path.

The opt-in bundle moved from the 76,000-byte smoke guard to 318,120 raw bytes
and 76,405 bytes with deterministic gzip after this checkpoint. The explicit
smoke guard is now 77,000 bytes. Further Inspect/Monitor ownership should either
pay down repeated checkpoint markup or introduce a deliberate split for
non-Operate v2 surfaces.

## Pipeline Monitor checkpoint

Pipeline Monitor now has the same first-decision v2 seam as Inspect: a React
checkpoint strip above the legacy monitoring wall. It summarizes selected
pipeline monitor coverage, missing monitoring URLs, active search narrowing,
lazy web-preview count, and the next operator step before the iframe/player grid
appears.

The full monitoring wall remains legacy-owned. Playback, mute/play-all controls,
monitoring URL edits, save validation, direct Open/Copy actions, YouTube status
checks, and lazy iframe mounting still live in `control-room.ts`. The v2 model is
read-only and exists to make the wall's state legible before the operator starts
loading external previews.

Seeded Playwright/CDP coverage proves the Monitor checkpoint appears under
`ui=v2`, the route ownership cue changes to `UI v2 checkpoint`, search updates
the checkpoint match count without relabeling filtered outputs as missing
configuration, and generic web monitors are counted as lazy previews before the
iframe is mounted.

## Incidents checkpoint

Incidents now follows the same checkpoint seam without taking over the dense
alert/event feed. The v2 strip summarizes current alert state, recent lifecycle
event volume, active scope, and the shared incident search state before the
legacy feed renders below it. The single v2 action jumps to Telemetry, matching
the normal incident-investigation flow without adding a second search/filter
surface.

Seeded Playwright/CDP coverage proves the Incidents checkpoint appears under
`ui=v2`, the route ownership cue changes to `UI v2 checkpoint`, the checkpoint
reacts to hit/no-hit incident search states, and scoped pipeline filtering keeps
the checkpoint and legacy route summary aligned.

The opt-in checkpoint bundle is deliberately separate from the full
Overview/Operate bundle. In the Incidents checkpoint build, Vite reports
`dashboard-v2-checkpoints-entry.js` at 6.40 kB raw / 1.78 kB gzip,
`dashboard-v2-entry.js` at 55.63 kB raw / 9.86 kB gzip, and the shared React
runtime chunk at 258.47 kB raw / 66.98 kB gzip. The smoke guard measures these
bundles independently so additional non-Operate checkpoints do not force the
main v2 UI path to load earlier than needed.

## Shared checkpoint card

The Inspect, Monitor, and Incidents checkpoint strips now render through one shared React
checkpoint component. This keeps the first-glance pattern consistent across
checkpoint routes: title, status badge, action cluster, four scan metrics,
optional compact metrics, and a focus/next-step block. Inspect and Monitor still
adapt their own pure view models at the boundary, and Incidents adapts the
existing legacy incident snapshot/search state. The shared component does not
fetch, subscribe, mutate, or own route state.

The refactor is behavior-preserving but removes the duplicated checkpoint
markup before adding more surfaces. The bundle now measures 319,332 raw bytes
and 76,735 bytes with deterministic gzip. Raw size drops by 2,481 bytes compared
with the Monitor checkpoint slice, while gzip remains under the 77,000-byte
smoke guard with 265 bytes of headroom. The next material v2 route should still
pay down more weight or split checkpoint surfaces before adding another large
component block.

## Checkpoint bundle split

The v2 build now emits stable route-oriented entrypoints instead of one growing
React island:

- `dashboard-v2-entry.js` owns Overview and Pipeline / Operate.
- `dashboard-v2-checkpoints-entry.js` owns Pipeline / Inspect and Pipeline /
  Monitor checkpoint strips.
- `dashboard-v2-jsx-runtime.js` is the shared React runtime chunk used by both
  entrypoints.

The dashboard loader imports the checkpoint bundle only when a checkpoint route
is active, so Overview and Operate no longer pay for Inspect/Monitor checkpoint
markup. The release build-tree and artifact smoke checks now require all three
stable files.

Measured deterministic gzip after the split:

| Route payload | Files | Gzip |
|---|---|---:|
| Overview / Operate | `dashboard-v2-entry.js` + `dashboard-v2-jsx-runtime.js` | 76,187 B |
| Inspect / Monitor checkpoint | `dashboard-v2-checkpoints-entry.js` + `dashboard-v2-jsx-runtime.js` | 67,978 B |

This restores meaningful headroom for checkpoint-route evolution without hiding
React's shared runtime cost. The smoke test now enforces per-route budgets
instead of a single monolithic bundle ceiling.
