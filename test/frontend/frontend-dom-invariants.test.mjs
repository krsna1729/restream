import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

async function readPublicFile(name) {
  return readFile(new URL(`../../public/${name}`, import.meta.url), "utf8");
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
  assert.match(indexHtml, /<h1[\s\S]*id="pipe-name"/);
  assert.match(
    indexHtml,
    /role="tab"[\s\S]*aria-controls="overview-mode-panel"/,
  );
  assert.match(indexHtml, /id="overview-mode-panel"[\s\S]*role="tabpanel"/);
  assert.doesNotMatch(indexHtml, /Cy Ganderton|Quality Control Specialist/);
  assert.doesNotMatch(indexHtml, /grid-template-columns:/);
  assert.match(indexHtml, /id="stats-table"><\/tbody>/);
  assert.match(indexHtml, /<details[\s\S]*id="pipe-srt-ingest-fields"/);
  assert.match(indexHtml, /id="out-srt-passphrase-input"/);
  assert.match(indexHtml, /id="out-srt-pbkeylen-input"/);
  assert.match(
    indexHtml,
    /min-w-0 overflow-y-auto rounded-lg border p-4 xl:min-w-\[24rem\]/,
  );
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
    /#dashboard-grid\s*{\s*grid-template-columns: minmax\(0, 1fr\);/s,
  );
  assert.match(inputCss, /#dashboard-grid\.has-selected-pipeline/s);
  assert.match(inputCss, /\.text-base-content\\\/50,[\s\S]*\.stat-title/s);
  assert.match(renderTs, /classList\.toggle\("has-selected-pipeline"/);
  assert.doesNotMatch(
    renderTs,
    /minmax\(24rem,\s*34rem\).*minmax\(24rem,\s*1fr\)/s,
  );
});
