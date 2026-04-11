#!/usr/bin/env node

import fs from 'node:fs';
import path from 'node:path';
import process from 'node:process';

function parseArgs(argv) {
  const args = {
    url: 'http://localhost:3030/',
    outDir: 'test/artifacts/runs/screenshots',
    width: 1920,
    height: 1080,
    pipelineDwellMs: 500,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const a = argv[i];
    if (a === '--url') args.url = argv[++i];
    else if (a === '--out-dir') args.outDir = argv[++i];
    else if (a === '--width') args.width = Number(argv[++i]);
    else if (a === '--height') args.height = Number(argv[++i]);
    else if (a === '--pipeline-dwell-ms') args.pipelineDwellMs = Number(argv[++i]);
  }

  return args;
}

async function loadPlaywright() {
  try {
    return await import('playwright');
  } catch (err) {
    console.error('playwright is not installed.');
    console.error('Install with: npm i -D playwright && npx playwright install chromium');
    process.exit(1);
  }
}

function safeName(value) {
  return String(value || 'unknown').replace(/[^a-zA-Z0-9._-]/g, '_');
}

async function main() {
  const args = parseArgs(process.argv.slice(2));
  fs.mkdirSync(args.outDir, { recursive: true });

  const ts = new Date().toISOString().replace(/[:.]/g, '-');
  const runDir = path.join(args.outDir, `dashboard-screenshots-${ts}`);
  fs.mkdirSync(runDir, { recursive: true });

  const { chromium } = await loadPlaywright();
  const browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({
    viewport: { width: args.width, height: args.height },
  });

  const page = await context.newPage();
  await page.goto(args.url, { waitUntil: 'networkidle' });
  await page.waitForSelector('#pipelines', { timeout: 10000 });

  const rows = page.locator('#pipelines li > div[onclick]');
  const count = await rows.count();
  const files = [];

  for (let i = 0; i < count; i += 1) {
    const row = rows.nth(i);
    await row.click({ timeout: 3000 });
    await page.waitForTimeout(args.pipelineDwellMs);

    const pipelineName = (await row.innerText()).split('\n')[0].trim() || `pipeline-${i + 1}`;
    const fileName = `${String(i + 1).padStart(2, '0')}-${safeName(pipelineName)}.png`;
    const filePath = path.join(runDir, fileName);
    await page.screenshot({ path: filePath, fullPage: true });
    files.push(filePath);
    console.log(`Saved screenshot: ${filePath}`);
  }

  const summaryPath = path.join(runDir, 'summary.json');
  fs.writeFileSync(
    summaryPath,
    JSON.stringify(
      {
        generatedAt: new Date().toISOString(),
        url: args.url,
        viewport: { width: args.width, height: args.height },
        count,
        files,
      },
      null,
      2,
    ),
  );

  await context.close();
  await browser.close();

  console.log(`Saved screenshot summary: ${summaryPath}`);
}

main().catch((err) => {
  console.error(err);
  process.exit(1);
});
