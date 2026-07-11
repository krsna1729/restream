import assert from "node:assert/strict";
import fs from "node:fs";
import test from "node:test";

test("operator navigation exposes incidents and telemetry panels without an agent surface", () => {
  const html = fs.readFileSync(
    new URL("../public/index.html", import.meta.url),
    "utf8",
  );
  assert.match(html, /data-dashboard-mode="incidents"/);
  assert.match(html, /id="incidents-mode-panel"/);
  assert.match(html, /data-dashboard-mode="telemetry"/);
  assert.match(html, /id="telemetry-mode-panel"/);
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
