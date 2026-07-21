# Frontend Layering Audit — 2026-07-21

**Methodology**: Three-lens analysis — manual source reading + graphify AST dependency graph (1,592 nodes, 4,576→4,517 edges across 71 TS/TSX files) + CodeGraph v1.4.1 SQLite index (12,611 nodes, 44,701 edges across 648 files, 71 web/ts files covered).

> **Changelog — 2026-07-21 (Wave 1 implemented)**
> - **escapeHtml triplication fixed**: Removed duplicate `export function escapeHtml` from `features/diagnostics.ts` and internal copy from `features/settings.ts`. All 14 importers already pointed to canonical `core/utils.ts`. Graphify edges dropped from 4,576→4,517 (−59 from removed exports/copies). Build passes, 133/133 tests pass, Playwright confirms dashboard loads correctly.
> - **modes.ts moved to app/**: degree-126 composition hub relocated from `features/modes.ts` to `app/modes.ts`. Internal imports (`./foo` → `../features/foo`), test module paths (`features/modes.js` → `app/modes.js`), and `dashboard-app.ts` import all updated. Graphify confirms `src=app/modes.ts`. 133/133 tests pass, Playwright screenshot captured.
>
> **Changelog — 2026-07-21 (Wave 2: subdirectory restructuring)**
> - **Four oversized feature files split into ownership subdirectories**: `pipeline-view-*` → `features/pipeline-view/`, `control-room-*` → `features/control-room/`, `editor-*` → `features/editor/`, `pipeline-inspector-*` → `features/pipeline-inspector/`. Each subdirectory has a barrel `index.ts`. Internal imports updated across `app/`, `features/`, and `test/`. All `.mjs` test paths updated. TypeScript compilation clean. Frontend tests: 133 unit + 61 view-model pass.
> - **Graphify build pipeline re-run**: Backend extracted (`graphify extract src --code-only --no-cluster`), frontend updated, merged to `.local/graphify/root/restream-code-graph.json` — 10,123 nodes, 26,215 edges.
> - **Codegraph index synced**: 28 added, 10 modified, 14 removed — all subdirectory paths resolved correctly.
> - **Remaining oversized files assessed**: `pipeline-inspector/index.ts` (~1,272 lines) still above 1,000-line cap; `modes.ts` (moved to `app/`), `settings.ts`, `status.ts` each assessed as single coherent concern without meaningful split boundary.
> - **`core/state.ts` formalized** (item #6): Added typed read accessors `getPipelines()`, `getConfig()`, `getMetrics()`, `getHealth()` and `updateState()` for controlled state mutations.
> - **`pipeline-inspector/index.ts` further split** (item #5): Extracted graph rendering code (state vars + 7 functions) into `features/pipeline-inspector/graph.ts` using dependency-injection pattern to break circular imports. index.ts: 1,272→1,085 lines. graph.ts: 323 lines. `tsc --noEmit` clean, 61/61 frontend tests pass.
> - **`features/dashboard.ts` reviewed** (item #8): 715 lines, under 1,000 cap. Coherent single-concern orchestrator (refresh polling + config mutation helpers). The config mutation helpers (~150 lines) could be extracted, but low value. No action needed.

---

## Contents

- [Executive Summary](#executive-summary)
- [0. Lens Comparison — What Each Found That the Others Missed](#0-lens-comparison--what-each-found-that-the-others-missed)
- [1. CodeGraph Per-Symbol Analysis — Hot Imports](#1-codegraph-per-symbol-analysis--hot-imports)
- [2. escapeHtml Triplication (CodeGraph-Discovered Only)](#2-escapehtml-triplication-codegraph-discovered-only)
- [3. Size — Post Wave-2 Status](#3-size--post-wave-2-status)
- [4. Layer Violations](#4-layer-violations)
- [5. Layering Validation (CodeGraph Edge Analysis)](#5-layering-validation-codegraph-edge-analysis)
- [6. core/api.ts — Interface-Only Monolith (Manual + Graphify + CodeGraph)](#6-coreapits--interface-only-monolith-manual--graphify--codegraph)
- [7. core/utils.ts — Not a Catch-All (CodeGraph Correction)](#7-coreutilsts--not-a-catch-all-codegraph-correction)
- [8. View-Model Boilerplate (12 Thin Files)](#8-view-model-boilerplate-12-thin-files)
- [9. history/render.ts (1,663 lines)](#9-historyrenderts-1663-lines)
- [10. God Components in app/dashboard-v2-entry.tsx](#10-god-components-in-appdashboard-v2-entrytsx)
- [11. app/dashboard-v2-loader.ts (993 lines) — Boilerplate Action Pattern](#11-appdashboard-v2-loaderts-993-lines--boilerplate-action-pattern)
- [12. Updated Priority Order (All Three Lenses)](#12-updated-priority-order-all-three-lenses)
- [13. Methodological Takeaways](#13-methodological-takeaways)
- [14. References](#14-references)

---

## Executive Summary

The frontend layering is **mostly correct** — `app/` roots features, `features/` does not import from `app/`, and `history/` is well-bounded. But three systemic issues emerge across all lenses:

1. **File bloat** (manual): 10 files over 1,000 lines, 4 over 1,900 lines
2. **Architecture hub** (graphify): `modes.ts` is a degree-126 composition hub in the wrong layer, refiling alone won't shrink the file
3. **Per-symbol fragmentation** (codegraph): `escapeHtml` triplicated, `state` imported 17× as the hottest singleton, `core/api.ts` has 63 internal functions with 0 exports but 20 files depend on its types

---

## 0. Lens Comparison — What Each Found That the Others Missed

| Finding | Manual | Graphify (file-degree) | CodeGraph (symbol-level) |
|---|---|---|---|
| File sizes >1,000 lines | ✅ All 10 | — | — |
| `modes.ts` wrong layer | ✅ 8+ peer imports | ✅ Degree 126 (#4) | ✅ 27 non-import edges, 63 outgoing imports |
| `escapeHtml` triplicated | ❌ | ❌ | ✅ **3 copies** across `core/utils.ts`, `features/diagnostics.ts`, `features/settings.ts` |
| `state` is #1 most-imported symbol | ❌ | ❌ (file-degree misses singletons) | ✅ **17 importers** — hottest symbol in frontend |
| `apiRequest` is purely internal | ❌ | ❌ (file-degree lumps all symbols) | ✅ called 53×, all from within `core/api.ts` itself |
| `dashboard-app.ts` v1 hub | ❌ | ✅ Degree 118 (#6) | ✅ **114 outgoing imports** — more than any other file |
| `features/dashboard.ts` underweighted | ❌ | ✅ Degree 92 (#10) | ✅ 34 outgoing imports, 15 exported functions |
| `core/utils.ts` is NOT a catch-all | ❌ (assumed messy) | ⚠️ Degree 67 | ✅ Actually well-organized: 30+ functions under export block |
| `diagnostics.ts` is unofficial core utility | ❌ | ❌ (shows as degree-52 feature) | ✅ **14 importers** — mostly for `escapeHtml` which belongs in `core/` |
| `core/api.ts` degree 150 bottleneck | ✅ Central type depot | ✅ Degree 150 (#1) | ✅ **20 importing files** — 27 interfaces, 63 functions, 0 exported functions |
| `core/state.ts` `AppState` interface | ❌ | ❌ | ✅ Exported, imported via `state` constant 17× |
| `dashboard-v2-entry.tsx` internal bloat only | ✅ 10 components | ✅ Degree 55 (low cross-ref) | ✅ 53 nodes — all internal |

---

## 1. CodeGraph Per-Symbol Analysis — Hot Imports

### 1.1 Most Imported Symbols

| Symbol | File | Times Imported | Role |
|---|---|---|---|
| `state` | `core/state.ts` | **17×** | Global app state singleton |
| `PipelineView` | `types.ts` | **16×** | Core domain type |
| `escapeHtml` | `features/diagnostics.ts` | **14×** | HTML escaping (WRONG FILE — see §2) |
| `withBasePath` | `core/base-path.ts` | **7×** | URL base path helper |
| `OutputView` | `types.ts` | **7×** | Domain type |
| `showErrorAlert` | `core/utils.ts` | **6×** | Error alert |
| `getUrlParam` | `core/utils.ts` | **6×** | URL param reader |
| `AudioTrack` | `types.ts` | **5×** | Domain type |
| `RenderScopeToken` | `core/render-scope.ts` | **5×** | Scope management |
| `isOutputRunning/Retrying/IntentStopped` | `core/output-status.ts` | **5× each** | Output state queries |
| `copyText`, `showCopiedNotification` | `core/utils.ts` | **5× each** | Clipboard utilities |

### 1.2 Most Called Functions

| Function | Calls | Note |
|---|---|---|
| `escapeHtml` | **278×** | 278 internal calls across 15 files (39 in `incidents.ts` alone) |
| `apiRequest` | **53×** | All 53 calls from within `core/api.ts` — internal fetch pipeline |
| `showErrorAlert` | **27×** | Shared error display |
| `finiteNumber` | **19×** | Number formatting |
| `valueOrDash` | **19×** | Display fallback |
| `renderControlRoom` | **16×** | Feature render |
| `renderPipelineInfoColumn` | **15×** | Feature render |

### 1.3 Most Depended-Upon Core Modules (by inbound imports)

| Core File | Importing Feature Files | Key Symbols |
|---|---|---|
| `core/state.ts` | **17 files** | `state` (AppState singleton) |
| `core/api.ts` | **20 files, 50+ import edges** | 27 interfaces, 0 exported functions |
| `core/utils.ts` | **14+ files** | `escapeHtml`, `showErrorAlert`, `copyText`, `getUrlParam` etc. |
| `core/base-path.ts` | **5+ files** | `withBasePath` |
| `core/render-scope.ts` | **5+ files** | `RenderScopeToken` |
| `core/output-status.ts` | **5+ files** | `isOutputRunning`, `isOutputRetrying`, `isOutputIntentStopped` |
| `core/display.ts` | **3 files** | Display formatting |
| `core/output-config.ts` | **5+ files** | Output configuration types |
| `core/log-stream.ts` | **3 files** | Log stream helpers |
| `core/audio-caps.ts` | **3 files** | Audio capabilities |

### 1.4 Features Importing from Core — Full Map

```
Features → core/state.ts: 17 files (hottest singleton dependency)
Features → core/api.ts:   20 files via 50+ import edges (type depot + indirect API surface)
Features → core/utils.ts: 14 files (utility functions)
Features → core/base-path.ts: 5 files
Features → core/render-scope.ts: 5 files
Features → core/output-status.ts: 5 files
```

The layer boundary is respected: **0 feature files import from `app/`**.

---

## 2. `escapeHtml` Triplication (CodeGraph-Discovered Only)

**`escapeHtml` exists independently in THREE files:**

| File | Line | Signature | Exported | Imported By |
|---|---|---|---|---|
| `core/utils.ts` | 10 | `escapeHtml(str: unknown): string` | ✅ (via export block) | (should be canonical) |
| `features/diagnostics.ts` | 156 | `export function escapeHtml(str: string): string` | ✅ (inline export) | **14 files** ← actual importers |
| `features/settings.ts` | 85 | `function escapeHtml(value: string): string` | ❌ (internal) | (internal only) |

**Impact**: The canonical version lives in `core/utils.ts` (accepts `unknown`, more permissive), but 14 feature files import from `features/diagnostics.ts` instead. The `diagnostics.ts` version uses `str: string` (narrower). If the core version ever needs to change behavior (e.g., adding XSS protection), the 14 duplicate importers won't benefit.

**Fix**: Remove `export function escapeHtml` from `features/diagnostics.ts`, rewire its 14 importers to `core/utils.ts`, and remove the internal copy in `features/settings.ts`.

---

## 3. Size — Post Wave-2 Status

Using the backend size bands from `docs/layering-roadmap.md`. After Wave 2 (2026-07-21), four oversized feature files were split into ownership subdirectories. The original flat files no longer exist.

**FAIL (≥1,000 lines) — remaining:**

| File | Lines | Layer | Notes |
|---|---|---|---|
| `app/dashboard-v2-entry.tsx` | **1,949** | app | Internal bloat (10 components), no cross-module coupling |
| `history/render.ts` | **1,663** | history | Clean layer boundary, internal render code only |
| `features/settings.ts` | **1,576** | features | Single coherent concern, no meaningful split boundary |
| `features/pipeline-inspector/index.ts` | **1,085** | features | ~~1,272~~ (187 lines → `graph.ts`); still 85 over cap |
| `core/api.ts` | **1,250** | core | Type depot + 63 internal functions (all private) |
| `app/modes.ts` | **1,227** | app | Moved from `features/` in Wave 1; composition hub |
| `features/status.ts` | **1,058** | features | Single coherent concern |

**Wave-2 splits (previously ≥1,000, now within bounds):**

| Original File | Lines (before) | Subdirectory | Index.ts | Largest submodule |
|---|---|---|---|---|
| `features/editor.ts` | 1,997 | `features/editor/` | 849 | `pipeline.ts` (732) |
| `features/control-room.ts` | 1,978 | `features/control-room/` | 819 | `monitor.ts` (849) |
| `features/pipeline-view.ts` | 1,655 | `features/pipeline-view/` | 1,000 | `audio.ts` (444) |
| `features/pipeline-inspector.ts` | 1,272 | `features/pipeline-inspector/` | 1,085 | `resource-view.ts` (752), `shell.ts` (83), `graph.ts` (323) |

*Graph rendering (~230 lines of code + dependency injection wiring) extracted into `graph.ts` using DI pattern to avoid circular imports between index.ts and the new graph module. index.ts now at 1,085 lines (85 over cap, but the remaining code is tightly coupled orchestration with no clear split boundary).*

**WARN (800–999):** `app/dashboard-v2-loader.ts` (993, degree 108), `features/pipeline-output-list.ts` (914, degree 57), `features/incidents.ts` (859, degree 61), `features/pipeline-operate-view-model.ts` (802, degree 54)

---

## 4. Layer Violations

### 4.1 `modes.ts` — Composition Hub in Wrong Layer (Manual + Graphify)

- **Manual**: 8+ peer feature imports, owns mode-switching orchestration
- **Graphify**: Degree 126 (#4), connected to 11+ features
- **CodeGraph**: 63 outgoing imports, 27 non-import edges, imports from `control-room.ts`, `dashboard.ts`, `diagnostics.ts`, `engineer-telemetry.ts`, `incidents.ts`, `media-library.ts`, `overview-activity.ts`, `pipeline-inspector.ts`, `pipeline-workspace-shell.ts`, `render.ts`, `settings.ts`, `status.ts`
- **Verdict**: Move to `app/`. The ownership matrix says `app/` owns "page-level mode orchestration"; `features/` should own "bounded UI rendering."

### 4.2 `escapeHtml` in Wrong File (CodeGraph-Discovered)

As described in §2 — a generic HTML-escape utility used by 14 importing files lives in `features/diagnostics.ts` instead of `core/`. The canonical version in `core/utils.ts` exists but is ignored by consumers. **Fix is trivial**: remove the duplicate export from `diagnostics.ts` and point importers to `core/utils.ts`.

### 4.3 `diagnostics.ts` as Unofficial Core Module (CodeGraph)

`features/diagnostics.ts` is imported by **14 other feature files** — more than most core modules. Its only exported symbols are `escapeHtml` and `openDiagnosticsModal`. If `escapeHtml` is moved to `core/`, `diagnostics.ts` drops to 1 exported symbol and single-digit importers, restoring its feature identity.

---

## 5. Layering Validation (CodeGraph Edge Analysis)

### 5.1 Cross-Feature Import Map

**80 cross-feature import pairs** exist. Key clusters:

| Importer | Imports From (features only) | Count |
|---|---|---|
| `modes.ts` | control-room, dashboard, diagnostics, engineer-telemetry, incidents, media-library, overview-activity, overview-view-model, pipeline-inspector, pipeline-workspace-shell, render, settings, status | **13** |
| `pipeline-view.ts` | audio-track-labels, diagnostics, ingest-url-details, input-preview, metric-format, pipeline-dependencies, pipeline-operate-view-model, pipeline-output-list, publisher-quality | **9** |
| `control-room.ts` | control-room-checkpoint, control-room-inputs, control-room-shell, control-room-types, control-room-view-model, dashboard, diagnostics, hls-player, input-preview | **9** |
| `dashboard.ts` | render, restream-process-indicator | 2 |
| `status.ts` | dashboard, diagnostics, restream-process-indicator, status-view-model | 4 |
| `pipeline-inspector.ts` | diagnostics, graph, pipeline-inspect-view-model, pipeline-inspector-resource-view, pipeline-inspector-shell | 5 |
| `editor.ts` | dashboard, diagnostics, output-control-state | 3 |
| `pipeline-output-list.ts` | diagnostics, output-control-state, pipeline-dependencies, pipeline-operate-view-model | 4 |

### 5.2 Most-Imported Feature Files (by other features)

| File | Imported By | Why |
|---|---|---|
| `diagnostics.ts` | 11 features | `escapeHtml` + `openDiagnosticsModal` |
| `dashboard.ts` | control-room, editor, modes, publisher-health, status | Runtime state coordination |
| `publisher-quality.ts` | pipeline-operate-view-model, pipeline-view, publisher-health | Quality metrics |
| `output-control-state.ts` | editor, pipeline-output-list | Output mutation tracking |
| `pipeline-operate-view-model.ts` | pipeline-output-list, pipeline-view, render | View-model sharing |
| `overview-view-model.ts` | modes, pipeline-operate-view-model | View-model sharing |

### 5.3 Layer Boundary Compliance

| Boundary | Violations? | Verdict |
|---|---|---|
| `features/` → `app/` | **0 violations** | ✅ Clean |
| `features/` → `core/` | ✅ Allowed (58 edges, 25 features) | ✅ Expected |
| `history/` → `features/` | 1 (history/render.ts → diagnostics.ts for `escapeHtml`) | ⚠️ Goes away with §2 fix |
| `history/` → `core/` | 0 direct | ✅ Clean |
| `app/` → `features/` | ✅ Allowed (app composes features) | ✅ Expected |

---

## 6. `core/api.ts` — Interface-Only Monolith (Manual + Graphify + CodeGraph)

| Lens | Finding |
|---|---|
| **Manual** | 1,250 lines, 0 exported functions, ~50 interfaces/types |
| **Graphify** | Degree **150** (#1 most connected file) |
| **CodeGraph** | 27 interfaces, 4 type aliases, **63 functions** (all unexported), 20 importing files |

**CodeGraph reveals the real structure**: `core/api.ts` is an **API facade** — 63 private functions (each calls `apiRequest` once) that form the complete backend API surface, plus 27 exported interfaces. The high graphify degree comes from every feature importing types from it. The fix isn't splitting interfaces (they're naturally cohesive) — it's whether `getConfig()`, `createPipeline()`, `deleteOutput()` etc. should be extracted into domain-aligned modules.

**`apiRequest` itself**: degree 53 — called by every function in the file, never exported. It's the fetch pipeline with loading state, error handling, and redirect-on-401. Purely internal.

---

## 7. `core/utils.ts` — Not a Catch-All (CodeGraph Correction)

**Initial assumption** (manual): "Catch-all of 621 lines" — actually well-organized. Uses a consolidated `export { ... }` block pattern at line 584 that exports 30+ functions: `escapeHtml`, `showErrorAlert`, `copyText`, `getUrlParam`, etc. The codegraph `is_exported` field is misleading here — it tracks inline `export function` syntax but misses consolidated `export { ... }` blocks.

**Actual content breakdown**:
- HTML/DOM utilities: `setInnerText`, `escapeHtml`, `escapeRedactedHtml`, `showLoading`, `hideLoading`, `showErrorAlert`, `confirmInApp`, `promptInApp`
- URL: `getUrlParam`, `setUrlParam`, `safeParseUrl`, `safeDecodeUrlComponent`, `isAbsoluteUrl`
- Clipboard: `copyText`, `copyData`, `legacyCopy`, `showCopiedNotification`
- Format: `formatMaskedStreamKey`, `formatCodecName`, `formatChannelCount`, `getStatusColor`
- Output config: `isValidOutput`, `isValidMonitoringUrl`, `protocolUsesOutputServerPresets`, `resolvePresetOutputUrl`, `matchOutputServerPreset`, `detectOutputProtocol`
- Pipeline hints: `readSelectedPipelineHint`, `writeSelectedPipelineHint`
- Misc: `setServerConfig`, `extractCandidateStreamToken`, `getDefaultOutputToken`, `parseSrtFields`, `buildDefaultCustomOutputUrl`, `maskSecret`, `sanitizeLogMessage`, `msToHHMMSS`

**CodeGraph insight**: The file is already more modular than it looks — the function count and domain spread is visible through per-symbol queries. The real issue is `escapeHtml` being duplicated in `features/diagnostics.ts` (which 14 files import instead).

---

## 8. View-Model Boilerplate (12 Thin Files)

Confirmed thin type-export files. CodeGraph: each is degree 2-8, nearly leaf nodes. Low-impact boilerplate.

---

## 9. `history/render.ts` (1,663 lines)

**Graphify**: Degree 57. **CodeGraph**: imports `diagnostics.ts` (for escapeHtml). Well-bounded history module — the size is internal render code, not cross-module coupling.

---

## 10. God Components in `app/dashboard-v2-entry.tsx`

| Lens | Finding |
|---|---|
| **Manual** | 10 components, 1,949 lines |
| **Graphify** | Degree 55 — the problem is internal bloat, not cross-module coupling |
| **CodeGraph** | 53 AST nodes — high node-to-line ratio suggests dense template logic |

Components: `DashboardV2Overview` (~500L), `DashboardV2PipelineInputStatus` (~360L), `DashboardV2PipelineOutputOverview` (~425L), `DashboardV2PipelineSelector` (~190L), `DashboardV2PipelineHeader` (~115L), plus `Panel`, `StatusBadge`, `Sparkline`, `MetricCard`, `DashboardV2DetailsPlaceholderCard` (~360L total). Each can be extracted independently.

---

## 11. `app/dashboard-v2-loader.ts` (993 lines) — Boilerplate Action Pattern

| Lens | Finding |
|---|---|
| **Manual** | 13× action interfaces, 14× update functions, 14× hide helpers |
| **Graphify** | Degree 108 (#7 hub) |
| **CodeGraph** | 133 AST nodes — 2nd most nodes per file after `core/api.ts` (112) |

Correctly placed in `app/` as composition root. Boilerplate duplication is the issue, not the layer.

---

## 12. Updated Priority Order (All Three Lenses)

Changes since Wave 2 (2026-07-21):
- Items 1-2 completed in Wave 1 ✅
- Items 5, 7, 8 completed in Wave 2 ✅ (subdirectory splits)
- Item 5: further split — graph rendering extracted to `graph.ts` (index.ts: 1,272→1,085) ✅
- Item 6: `core/state.ts` formalized — typed accessors + updateState() ✅
- Item 8: `features/dashboard.ts` reviewed — 715 lines, coherent, no action needed ✅

**Remaining:**

| # | Finding | Lines | Graphify Degree | CodeGraph Insight | Effort | Value |
|---|---|---|---|---|---|---|
| 3 | **Split `dashboard-v2-entry.tsx`** | 1,949 | 55 | 53 AST nodes, internal bloat only | Medium | **High** |
| 4 | **Factor `core/api.ts`** — degree 150 bottleneck, extract domain API modules | 1,250 | **150** (#1) | 63 internal functions, 20 importers, 27 types | Medium | **High** |
| 5 | **Further split `pipeline-inspector/index.ts`** — graph extracted, remaining 1,085L is tightly coupled orchestration | 1,085 | **100** | graph.ts (323) + resource-view.ts (752) + shell.ts (83) now sibling modules | Low | Medium |
| 6 | ~~**Stabilize `core/state.ts`** — hottest singleton, formalize access pattern~~ | — | — | ✅ Done | — | — |
| 7 | **Deduplicate loader boilerplate** in `dashboard-v2-loader.ts` | ~400 | 108 | 133 AST nodes, repetitive patterns | Low | Medium |
| 8 | ~~**Review `features/dashboard.ts`**~~ | — | — | ✅ Done — 715 lines, coherent single-concern orchestrator. Config mutation helpers (~150L) could split but low value. No action needed. | — | — |
| 9 | **Split `history/render.ts`** extract modal/table/search | 1,663 | 57 | Clean layer boundary, internal size only | Medium | Low-Med |

**Items discovered only by graphify:**
- `app/dashboard-app.ts` (degree 118, 114 outgoing imports) — v1 hub still carrying load
- `features/dashboard.ts` (degree 92) — cross-feature hub manual audit underweighted
- `core_api_apirequest` (degree 60) — single most-called function; purely internal to api.ts

**Items discovered only by codegraph:**
- `escapeHtml` triplication across 3 files — cheap fix, high impact
- `state` imported 17× as hottest singleton — architectural attention point
- `diagnostics.ts` as unofficial core utility (14 importers)
- `apiRequest` purely internal (53 calls, all within api.ts)
- `core/utils.ts` export block pattern — already better organized than assumed

---

## 13. Methodological Takeaways

| Tool | Best For | Blind Spots |
|---|---|---|
| **Manual** | File size, code smells, logical cohesion | Misses 3+ copy duplication, can't surface import centrality |
| **Graphify** (AST graph) | File-level degree centrality, community detection | Misses per-symbol hotness (e.g., `state`), aggregates all edges at file level |
| **CodeGraph** (SQLite index) | Per-symbol import counts, cross-file call graphs, export accuracy | `is_exported` misses consolidated `export { ... }` blocks; no degree centrality aggregation |

**Takeaway**: Graphify's file-degree view and CodeGraph's per-symbol import view are complementary. Together they found everything. Neither alone would have caught the `escapeHtml` triplication (manual caught nothing, graphify aggregates at file level, codeGraph tracked per-symbol import chains).

---

## 14. References

- Backend layering ownership matrix: `docs/layering-roadmap.md`
- Layering ladder/stop rules: `docs/agent-guidance/skills/layering-audit/SKILL.md`
- Graphify output: `.local/graphify/web-ts/graphify-out/graph.json` (1,592 nodes, 4,576 edges)
- CodeGraph DB: `.codegraph/codegraph.db` (v1.4.1, 12,611 nodes, 44,701 edges across 648 files)
