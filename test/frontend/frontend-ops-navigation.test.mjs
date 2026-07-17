import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

test("operator navigation keeps primary tabs focused while retaining detail panels", () => {
  const html = fs.readFileSync(
    new URL("../../web/pages/index.html", import.meta.url),
    "utf8",
  );
  assert.match(html, /id="incidents-mode-panel"/);
  assert.match(html, /id="telemetry-mode-panel"/);
  assert.match(html, /id="workspace-tab-incidents"[\s\S]*role="tab"/);
  assert.match(html, /data-dashboard-mode="incidents"/);
  assert.match(html, /aria-controls="incidents-mode-panel"/);
  assert.match(html, /id="workspace-tab-telemetry"[\s\S]*role="tab"/);
  assert.match(html, /data-dashboard-mode="telemetry"/);
  assert.match(html, /aria-controls="telemetry-mode-panel"/);
  assert.doesNotMatch(html, /data-dashboard-mode="agent"/);
  assert.match(html, /<header class="navbar/);
  assert.match(html, /<main id="dashboard-main"/);
  assert.match(
    html,
    /<nav class="join" role="tablist" aria-label="Workspace mode">/,
  );
  assert.match(html, /id="workspace-tab-overview"[\s\S]*role="tab"/);
  assert.match(html, /id="overview-mode-panel"[\s\S]*role="tabpanel"/);
  assert.match(html, /aria-controls="overview-mode-panel"/);
  assert.doesNotMatch(
    html,
    /data-dashboard-mode="[^"]+"[\s\S]{0,120}aria-pressed=/,
  );
});
