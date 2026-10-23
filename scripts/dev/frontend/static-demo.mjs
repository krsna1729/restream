#!/usr/bin/env node
// A self-contained demo of the control room: the built dashboard plus the
// fixture engine running in the page (fixtures/browser-backend.mjs), as a
// static folder any web server or artifact host can serve. No engine, no
// login; every number is synthetic and changes every 2 s.
//
//   npm run build:frontend && node scripts/dev/frontend/static-demo.mjs \
//     [--out .local/demo] [--preset small|plant|extreme]
//
// Writes index.html, demo-backend.js, app.js, output.css, logo.png and
// hls.min.js.

import { copyFileSync, mkdirSync, rmSync, writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import { build } from "rolldown";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 1) {
  const arg = process.argv[index];
  if (arg.startsWith("--")) args.set(arg.slice(2), process.argv[index + 1]);
}
const repo = resolve(fileURLToPath(new URL("../../..", import.meta.url)));
const out = resolve(repo, args.get("out") ?? ".local/demo");
const preset = args.get("preset") ?? "plant";
const publicDir = join(repo, "public");

rmSync(out, { recursive: true, force: true });
mkdirSync(out, { recursive: true });

await build({
  input: join(repo, "scripts/dev/frontend/fixtures/browser-backend.mjs"),
  output: { file: join(out, "demo-backend.js"), format: "iife", minify: true },
  logLevel: "warn",
});
await build({
  input: join(publicDir, "js/control-room/main.js"),
  output: { file: join(out, "app.js"), format: "esm", minify: true, codeSplitting: false },
  logLevel: "warn",
  // React libraries mark modules "use client"; one bundle has no server side.
  onLog(level, log, handler) {
    if (log.code === "MODULE_LEVEL_DIRECTIVE") return;
    handler(level, log);
  },
});
copyFileSync(join(publicDir, "output.css"), join(out, "output.css"));
copyFileSync(join(publicDir, "logo.png"), join(out, "logo.png"));
copyFileSync(join(publicDir, "js/lib/hls.min.js"), join(out, "hls.min.js"));

// The base path is wherever the page is served from, so the router and the
// fetch shim agree on it under any host. The page carries no document
// skeleton of its own: hosts that wrap pages (artifact viewers) add one,
// and browsers supply one when served as is.
writeFileSync(
  join(out, "index.html"),
  `<title>Restream Control Room</title>
<meta name="viewport" content="width=device-width, initial-scale=1" />
<link rel="icon" type="image/png" href="logo.png" />
<link rel="stylesheet" type="text/css" href="output.css" />
<script>
  document.documentElement.setAttribute("data-theme", "restream-dark");
  window.__RESTREAM_BASE_PATH__ = location.pathname.replace(/\\/[^/]*$/, "");
  window.__RESTREAM_DEMO_PRESET__ = ${JSON.stringify(preset)};
</script>
<script src="demo-backend.js"></script>
<script src="hls.min.js"></script>
<script type="module" src="app.js"></script>
<div id="root"></div>
`,
);
console.log(`static demo (${preset}) in ${out}`);
