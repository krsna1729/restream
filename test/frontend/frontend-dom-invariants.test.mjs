import assert from "node:assert/strict";
import { readdir, readFile } from "node:fs/promises";
import test from "node:test";

async function readPublicFile(name) {
  return readFile(new URL(`../../public/${name}`, import.meta.url), "utf8");
}

async function sourceFilesUnder(dirUrl) {
  const entries = await readdir(dirUrl, { withFileTypes: true });
  const nested = await Promise.all(
    entries.map(async (entry) => {
      const url = new URL(`${entry.name}${entry.isDirectory() ? "/" : ""}`, dirUrl);
      if (entry.isDirectory()) return sourceFilesUnder(url);
      if (/\.(tsx?|mts|cts)$/.test(entry.name)) return [url];
      return [];
    }),
  );
  return nested.flat();
}

function idsIn(html) {
  return [...html.matchAll(/\sid="([^"]+)"/g)].map((match) => match[1]);
}

test("static HTML keeps core DOM accessibility and layout invariants", async () => {
  const [indexHtml, loginHtml, loginJs, hlsBundle] = await Promise.all([
    readPublicFile("index.html"),
    readPublicFile("login.html"),
    readPublicFile("login.js"),
    readPublicFile("js/lib/hls.min.js"),
  ]);
  const allIds = [...idsIn(indexHtml), ...idsIn(loginHtml)];
  const duplicateIds = allIds.filter(
    (id, index) => allIds.indexOf(id) !== index,
  );

  assert.deepEqual(
    duplicateIds,
    [],
    "HTML IDs must be unique across shipped pages",
  );
  assert.match(
    indexHtml,
    /<meta name="viewport" content="width=device-width, initial-scale=1" \/>/,
  );
  assert.match(
    loginHtml,
    /<meta name="viewport" content="width=device-width, initial-scale=1" \/>/,
  );
  assert.match(loginHtml, /<form id="login-form"/);
  assert.match(loginHtml, /<main class="card/);
  assert.match(loginHtml, /id="username-input"[\s\S]*aria-label="Username"/);
  assert.match(loginHtml, /id="password-input"[\s\S]*aria-label="Password"/);
  assert.doesNotMatch(loginHtml, /\son(?:click|keydown)=/);
  assert.doesNotMatch(loginHtml, /tabindex="-1"/);
  assert.match(loginHtml, /<script src="login\.js"><\/script>/);
  assert.match(loginJs, /addEventListener\("submit"/);
  assert.match(indexHtml, /<header class="navbar/);
  assert.match(
    indexHtml,
    /id="skip-to-dashboard-main"[\s\S]*href="#dashboard-main"[\s\S]*Skip to main content/,
  );
  assert.match(indexHtml, /<main id="dashboard-main"/);
  assert.match(indexHtml, /<main id="dashboard-main"[\s\S]*tabindex="-1"/);
  assert.match(indexHtml, /role="tablist" aria-label="Workspace mode"/);
  assert.match(
    indexHtml,
    /<section[\s\S]*id="workspace-mode-bar"[\s\S]*aria-label="Workspace navigation"/,
  );
  assert.match(
    indexHtml,
    /<section[\s\S]*id="pipeline-workspace-view-bar"[\s\S]*aria-label="Pipeline navigation"/,
  );
  assert.match(indexHtml, /id="dashboard-v2-pipeline-header-root"/);
  assert.doesNotMatch(indexHtml, /id="pipe-name"/);
  assert.match(
    indexHtml,
    /role="tab"[\s\S]*aria-controls="overview-mode-panel"/,
  );
  assert.match(indexHtml, /id="overview-mode-panel"[\s\S]*role="tabpanel"/);
  assert.doesNotMatch(indexHtml, /Cy Ganderton|Quality Control Specialist/);
  assert.doesNotMatch(indexHtml, /grid-template-columns:/);
  assert.doesNotMatch(indexHtml, /id="stats-col"/);
  assert.doesNotMatch(indexHtml, /id="stats-table"/);
  assert.match(indexHtml, /<details[\s\S]*id="pipe-srt-ingest-fields"/);
  assert.match(indexHtml, /id="out-srt-passphrase-input"/);
  assert.match(indexHtml, /id="out-srt-pbkeylen-input"/);
  assert.match(
    indexHtml,
    /min-w-0 overflow-y-auto rounded-lg border p-4 xl:min-w-\[24rem\]/,
  );
  assert.match(indexHtml, /id="dashboard-v2-operate-panel"/);
  assert.doesNotMatch(indexHtml, /id="dashboard-grid"/);
  assert.doesNotMatch(indexHtml, /id="pipe-info-col"/);
  assert.doesNotMatch(indexHtml, /id="outs-col"/);
  assert.doesNotMatch(hlsBundle, /sourceMappingURL=hls\.min\.js\.map/);
});

test("dashboard bootstrap keeps the skip link first in keyboard flow", async () => {
  const dashboardEntryTs = await readFile(
    new URL("../../web/ts/app/dashboard-entry.ts", import.meta.url),
    "utf8",
  );

  assert.match(dashboardEntryTs, /document\.addEventListener\("keydown"/);
  assert.match(dashboardEntryTs, /event\.key !== "Tab"/);
  assert.match(dashboardEntryTs, /document\.activeElement !== document\.body/);
  assert.match(dashboardEntryTs, /skipLink\.focus\(\)/);
});

test("dashboard grid sizing lives in responsive CSS instead of inline scripts", async () => {
  const [inputCss, renderTs] = await Promise.all([
    readFile(new URL("../../web/styles/input.css", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/render.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.match(
    inputCss,
    /#dashboard-v2-operate-panel\s*{\s*grid-template-columns: minmax\(0, 1fr\);/s,
  );
  assert.match(inputCss, /#dashboard-v2-operate-panel:has/s);
  assert.match(inputCss, /\.text-base-content\\\/50,[\s\S]*\.stat-title/s);
  assert.doesNotMatch(renderTs, /dashboard-grid/);
  assert.doesNotMatch(renderTs, /has-selected-pipeline/);
  assert.doesNotMatch(renderTs, /stats-col/);
  assert.doesNotMatch(renderTs, /renderStatsColumn/);
  assert.doesNotMatch(
    renderTs,
    /minmax\(24rem,\s*34rem\).*minmax\(24rem,\s*1fr\)/s,
  );
});

test("feature modules receive dashboard UI version through app-owned configuration", async () => {
  const featureFiles = await sourceFilesUnder(
    new URL("../../web/ts/features/", import.meta.url),
  );
  const violations = [];
  await Promise.all(
    featureFiles.map(async (fileUrl) => {
      const source = await readFile(fileUrl, "utf8");
      if (
        source.includes("dashboard-ui-v2-toggle") ||
        /URLSearchParams\(window\.location\.search\)[\s\S]{0,120}\.get\("ui"\)/.test(
          source,
        )
      ) {
        violations.push(fileUrl.pathname);
      }
    }),
  );

  assert.deepEqual(
    violations.sort(),
    [],
    "feature modules must not read app chrome or URL UI-version state directly",
  );
});

test("dashboard app source has no UI-version experiment gate after cutover", async () => {
  const appFiles = await sourceFilesUnder(
    new URL("../../web/ts/app/", import.meta.url),
  );
  const violations = [];
  await Promise.all(
    appFiles.map(async (fileUrl) => {
      const source = await readFile(fileUrl, "utf8");
      if (
        source.includes("dashboard-ui-v2-toggle") ||
        source.includes("dashboardV2ExperimentEnabled") ||
        source.includes("startDashboardV2Experiment")
      ) {
        violations.push(fileUrl.pathname);
      }
    }),
  );

  assert.deepEqual(
    violations.sort(),
    [],
    "dashboard app modules must not retain UI-version experiment switches",
  );
});

test("status route body uses the v2-owned renderer", async () => {
  const [routerSource, statusSource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/status/route-body.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(
    routerSource,
    /document\.getElementById\("status-mode-content"\)/,
  );
  assert.doesNotMatch(routerSource, /legacyBodyId/);
  assert.doesNotMatch(routerSource, /id="status-versions"/);
  assert.doesNotMatch(routerSource, /refresh-status-btn/);
  assert.match(routerSource, /renderDashboardV2StatusBody/);
  assert.match(
    routerSource,
    /renderStatusMode\(dashboardV2RouteBodyConfig\("status"\)\.hostId\)/,
  );
  assert.match(
    statusSource,
    /export function renderDashboardV2StatusBody\(\s*container: HTMLElement,\s*\): Promise<void>/,
  );
  assert.match(statusSource, /container\.dataset\.statusRouteBody = "v2"/);
});

test("pipeline inspect route body uses the v2-owned renderer", async () => {
  const [routerSource, routeBodySource, shellSource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL(
        "../../web/ts/features/pipeline-inspect-route-body.ts",
        import.meta.url,
      ),
      "utf8",
    ),
    readFile(
      new URL(
        "../../web/ts/features/pipeline-inspector/shell.ts",
        import.meta.url,
      ),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderPipelineInspector/);
  assert.doesNotMatch(routerSource, /setPipelineInspectorContainerId/);
  assert.match(routerSource, /renderDashboardV2PipelineInspectBody/);
  assert.match(
    routerSource,
    /dashboardV2RouteBodyConfig\("pipeline-inspect"\)\.hostId/,
  );
  assert.match(
    routeBodySource,
    /export function renderDashboardV2PipelineInspectBody\(\s*containerId: string,/,
  );
  assert.match(routeBodySource, /setPipelineInspectorContainerId\(containerId\)/);
  assert.match(routeBodySource, /renderPipelineInspector\(\)/);
  assert.match(
    routeBodySource,
    /container\.dataset\.pipelineInspectRouteBody = "v2"/,
  );
  assert.doesNotMatch(shellSource, /v2RouteBody/);
  assert.doesNotMatch(shellSource, /inspect-route-summary/);
  assert.doesNotMatch(shellSource, /inspect-open-pipeline-btn/);
  assert.doesNotMatch(shellSource, /inspect-open-diagnostics-btn/);
  assert.doesNotMatch(routeBodySource, /v2Active/);
});

test("pipeline monitor route body uses the v2-owned renderer", async () => {
  const [routerSource, routeBodySource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/control-room-route-body.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderControlRoom/);
  assert.doesNotMatch(routerSource, /setControlRoomContainerId/);
  assert.match(routerSource, /renderDashboardV2ControlRoomBody/);
  assert.match(
    routerSource,
    /dashboardV2RouteBodyConfig\("pipeline-monitor"\)\.hostId/,
  );
  assert.match(
    routeBodySource,
    /export function renderDashboardV2ControlRoomBody\(containerId: string\): void/,
  );
  assert.match(routeBodySource, /setControlRoomContainerId\(containerId\)/);
  assert.match(routeBodySource, /renderControlRoom\(\)/);
  assert.match(routeBodySource, /container\.dataset\.controlRoomRouteBody = "v2"/);
  assert.doesNotMatch(routeBodySource, /v2Active/);
});

test("settings route body uses the v2-owned renderer", async () => {
  const [routerSource, settingsSource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/settings/index.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderSettingsPanel/);
  assert.doesNotMatch(settingsSource, /renderSettingsPanel/);
  assert.doesNotMatch(settingsSource, /v2RouteBody/);
  assert.doesNotMatch(settingsSource, /settingsRouteBody = "legacy"/);
  assert.match(routerSource, /renderDashboardV2SettingsBody/);
  assert.match(
    routerSource,
    /renderSettingsMode\(dashboardV2RouteBodyConfig\("settings"\)\.hostId\)/,
  );
  assert.match(
    settingsSource,
    /export function renderDashboardV2SettingsBody\(container: HTMLElement\): void/,
  );
  assert.match(settingsSource, /renderSettingsRoute\(container, \{ routeChrome: false \}\)/);
  assert.match(settingsSource, /container\.dataset\.settingsRouteBody = "v2"/);
  assert.match(settingsSource, /mountSettingsV2Disclosures\(container\)/);
});

test("media route body uses the v2-owned renderer", async () => {
  const [routerSource, mediaSource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/media-library.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderMediaLibraryMode/);
  assert.match(routerSource, /renderDashboardV2MediaBody/);
  assert.match(
    routerSource,
    /dashboardV2RouteBodyConfig\("media"\)\.hostId/,
  );
  assert.match(
    mediaSource,
    /export function renderDashboardV2MediaBody\(\s*container: HTMLElement,/,
  );
  assert.match(mediaSource, /container\.dataset\.mediaRouteBody = "v2"/);
  assert.match(mediaSource, /setMediaLibraryContainerId\(container\.id\)/);
  assert.match(mediaSource, /mediaLibraryShellMountedInCurrentContainer\(\)/);
  assert.match(mediaSource, /refreshMediaLibraryMetricsOnly\(\)/);
});

test("incidents route body uses the v2-owned renderer", async () => {
  const [routerSource, incidentsSource, routeBodySource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/incidents.ts", import.meta.url),
      "utf8",
    ),
    readFile(
      new URL("../../web/ts/features/incidents-route-body.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderIncidentsMode/);
  assert.match(routerSource, /renderDashboardV2IncidentsBody/);
  assert.match(routerSource, /clearDashboardV2IncidentsBody/);
  assert.match(
    routerSource,
    /dashboardV2RouteBodyConfig\("incidents"\)\.hostId/,
  );
  assert.match(
    routeBodySource,
    /export function renderDashboardV2IncidentsBody\(\s*containerId: string,/,
  );
  assert.match(routeBodySource, /container\.dataset\.incidentsRouteBody = "v2"/);
  assert.match(routeBodySource, /renderIncidentsMode\(\{\s*active: true,/);
  assert.match(routeBodySource, /routeChrome: false/);
  assert.match(routeBodySource, /v2Active: true/);
  assert.doesNotMatch(incidentsSource, /suppressV2RouteChrome/);
  assert.match(incidentsSource, /export function clearIncidentsMode\(\): void/);
});

test("telemetry route body uses the v2-owned renderer", async () => {
  const [routerSource, telemetrySource, routeBodySource] = await Promise.all([
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL("../../web/ts/features/engineer-telemetry.ts", import.meta.url),
      "utf8",
    ),
    readFile(
      new URL("../../web/ts/features/telemetry-route-body.ts", import.meta.url),
      "utf8",
    ),
  ]);

  assert.doesNotMatch(routerSource, /renderEngineerTelemetryMode/);
  assert.match(routerSource, /renderDashboardV2TelemetryBody/);
  assert.match(routerSource, /clearDashboardV2TelemetryBody/);
  assert.match(
    routerSource,
    /dashboardV2RouteBodyConfig\("telemetry"\)\.hostId/,
  );
  assert.match(
    routeBodySource,
    /export function renderDashboardV2TelemetryBody\(\s*containerId: string,/,
  );
  assert.match(routeBodySource, /container\.dataset\.telemetryRouteBody = "v2"/);
  assert.match(routeBodySource, /renderEngineerTelemetryMode\(\{\s*active: true,/);
  assert.match(routeBodySource, /routeChrome: false/);
  assert.doesNotMatch(telemetrySource, /suppressV2RouteChrome/);
  assert.match(
    telemetrySource,
    /export function clearEngineerTelemetryMode\(\): void/,
  );
});

test("dashboard v2 route-body hosts are app-owned in one contract", async () => {
  const [routeBodiesSource, routerSource, checkpointsSource] = await Promise.all([
    readFile(
      new URL("../../web/ts/app/dashboard-v2-route-bodies.ts", import.meta.url),
      "utf8",
    ),
    readFile(new URL("../../web/ts/app/modes/router.ts", import.meta.url), "utf8"),
    readFile(
      new URL(
        "../../web/ts/app/dashboard-v2-checkpoints-entry.tsx",
        import.meta.url,
      ),
      "utf8",
    ),
  ]);
  const hostIds = [
    "dashboard-v2-pipeline-inspect-content",
    "dashboard-v2-control-room-content",
    "dashboard-v2-incidents-content",
    "dashboard-v2-telemetry-content",
    "dashboard-v2-media-content",
    "dashboard-v2-settings-content",
    "dashboard-v2-status-content",
  ];

  for (const hostId of hostIds) {
    assert.match(routeBodiesSource, new RegExp(`hostId: "${hostId}"`));
    assert.doesNotMatch(routerSource, new RegExp(hostId));
    assert.doesNotMatch(checkpointsSource, new RegExp(hostId));
  }
  assert.match(checkpointsSource, /routeBody={dashboardV2RouteBodyConfig/);
  assert.match(checkpointsSource, /data-dashboard-v2-owned-route-body={routeBody\.mode}/);
});
