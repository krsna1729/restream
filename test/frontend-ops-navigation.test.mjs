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
  assert.match(html, /<nav class="join" aria-label="Workspace mode">/);
});
