import { chromium } from "@playwright/test";
import { auditLayout } from "./layout-audit.mjs";
const b = await chromium.launch({ executablePath: "/opt/pw-browsers/chromium" });
const p = await b.newPage({ viewport: { width: 1440, height: 900 }, colorScheme: "dark" });
for (const w of ["fleet", "operate"]) {
  await p.goto(`http://127.0.0.1:8766/index.html?w=${w}&p=pl_0000`);
  await p.waitForTimeout(2000);
  const a = await p.evaluate(auditLayout, { phone: false });
  console.log(w, a.lowContrast.length, a.lowContrast.slice(0, 5));
}
await p.screenshot({ path: process.argv[2] });
await b.close();
