# Frontend Layering Audit — 2026-07-21

**Methodology**: Three-lens analysis — manual source reading + graphify AST dependency graph (1,592 nodes, 4,576→4,517 edges across 71 TS/TSX files) + CodeGraph v1.4.1 SQLite index (12,611 nodes, 44,701 edges across 648 files, 71 web/ts files covered).

> **Changelog — 2026-07-21 (Wave 1 implemented)**
> - **escapeHtml triplication fixed**: Removed duplicate `export function escapeHtml` from `features/diagnostics.ts` and internal copy from `features/settings.ts`. All 14 importers already pointed to canonical `core/utils.ts`. Graphify edges dropped from 4,576→4,517 (−59 from removed exports/copies). Build passes, 133/133 tests pass, Playwright confirms dashboard loads correctly.
> - **modes.ts moved to app/**: degree-126 composition hub relocated from `features/modes.ts` to `app/modes.ts`. Internal imports (`./foo` → `../features/foo`), test module paths (`features/modes.js` → `app/modes.js`), and `dashboard-app.ts` import all updated. Graphify confirms `src=app/modes.ts`. 133/133 tests pass, Playwright screenshot captured.

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

## 3. Size — 10 Files Over 1,000 Lines

Using the backend size bands from `docs/layering-roadmap.md`:

**FAIL (≥1,000 lines):**

| File | Lines | Layer | Graphify Degree | CodeGraph Outgoing Imports |
|---|---|---|---|---|
| `features/editor.ts` | **1,997** | features | **139** (2nd) | 77 |
| `features/control-room.ts` | **1,978** | features | **132** (3rd) | 52 |
| `app/dashboard-v2-entry.tsx` | **1,949** | app | 55 (internal bloat) | n/a (10 components) |
| `history/render.ts` | **1,663** | history | 57 | internal |
| `features/pipeline-view.ts` | **1,655** | features | **121** (5th) | 52 |
| `features/settings.ts` | **1,576** | features | 102 | 23 |
| `features/pipeline-inspector.ts` | **1,272** | features | **100** | 42 |
| `core/api.ts` | **1,250** | core | **150 (1st)** | 26 (type depot, 63 internal functions) |
| `features/modes.ts` | **1,227** | features | **126** (4th) | **63** |
| `features/status.ts` | **1,058** | features | 82 | 23 |

**WARN (800–999):** `app/dashboard-v2-loader.ts` (993, degree 108), `features/pipeline-output-list.ts` (914, degree 57), `features/incidents.ts` (859, degree 61), `features/pipeline-operate-view-model.ts` (802, degree 54)

**CodeGraph confirms**: The top 5 files by outgoing import count (dashboard-app 114, editor 77, modes 63, pipeline-view 52, control-room 52) are all ≥1,600 lines except dashboard-app (394 lines — dense composition root, correctly in `app/`).

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

| # | Finding | Lines | Graphify Degree | CodeGraph Insight | Effort | Value |
|---|---|---|---|---|---|---|
| 1 | **Move `modes.ts` to `app/`** | 1,227 | **126** (#4) | 63 outgoing imports, 13 feature deps | Medium | **High** |
| 2 | **Triaged `escapeHtml` — remove from diagnostics.ts, deduplicate** | trivial | n/a | **3 copies**, 14 importers pointing to wrong file | **Low** | **High** (quick win) |
| 3 | **Split `dashboard-v2-entry.tsx`** | 1,949 | 55 | 53 AST nodes, internal bloat only | Medium | **High** |
| 4 | **Factor `core/api.ts`** — degree 150 bottleneck, extract domain API modules | 1,250 | **150** (#1) | 63 internal functions, 20 importers, 27 types | Medium | **High** |
| 5 | **Split `control-room.ts`** | 1,978 | **132** (#3) | 52 outgoing imports, 9 feature deps | Hard | **High** |
| 6 | **Stabilize `core/state.ts`** — hottest singleton, formalize access pattern | 18 | n/a | **17 importers**, every feature reads from it | Low | Medium |
| 7 | **Split `editor.ts`** | 1,997 | **139** (#2) | 77 outgoing imports | Hard | Medium |
| 8 | **Split `pipeline-view.ts`** | 1,655 | **121** (#5) | 52 imports, 9 feature deps | Hard | Medium |
| 9 | **Deduplicate loader boilerplate** in `dashboard-v2-loader.ts` | ~400 | 108 | 133 AST nodes, repetitive patterns | Low | Medium |
| 10 | **Review `features/dashboard.ts`** | 715 | **92** (#10) | 34 outgoing imports, 15 exported functions | Low | Medium |
| 11 | **Split `history/render.ts`** extract modal/table/search | 1,663 | 57 | Clean layer boundary, internal size only | Medium | Low-Med |

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
