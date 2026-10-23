#!/usr/bin/env node
// Screenshot matrix and visual-ergonomics checks for the control room.
//
//   node scripts/dev/frontend/shots.mjs --base http://127.0.0.1:3031 --label before \
//     [--workspaces fleet,operate] [--widths 375,768,1440] [--themes dark,light] \
//     [--details summary,detail] [--password admin] [--full 1] \
//     [--role viewer|operator]   (view the dashboard as a lower role)
//
// Writes PNGs and report.json to .local/shots/<label>/. For each page it
// checks: no horizontal page scroll, no text clipped by its own box, touch
// targets of at least 36 px on phone widths, and console errors. It uses
// the preinstalled Chromium (PLAYWRIGHT_BROWSERS_PATH) and never downloads.

import { mkdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";

import { chromium } from "@playwright/test";

const args = new Map();
for (let index = 2; index < process.argv.length; index += 1) {
  const arg = process.argv[index];
  if (arg.startsWith("--")) args.set(arg.slice(2), process.argv[index + 1]);
}
const list = (key, fallback) => (args.get(key) ?? fallback).split(",").filter(Boolean);

const base = args.get("base") ?? "http://127.0.0.1:3031";
const label = args.get("label") ?? "current";
const workspaces = list(
  "workspaces",
  "fleet,pipelines,operate,inspect,monitor,incidents,logs,capacity,media,system,changes,settings",
);
const widths = list("widths", "375,768,1440").map(Number);
const themes = list("themes", "dark");
// "detail" opens every section's Detail switch (ui/prefs.tsx).
const details = list("details", "summary");
const DETAIL_SECTIONS = ["destinations", "channels"];
const pipeline = args.get("pipeline") ?? "";
const settleMs = Number(args.get("settle-ms") ?? 3500);
const fullPage = args.get("full") === "1";
const password = args.get("password");
const role = args.get("role") ?? "";
const out = join(".local", "shots", label);
mkdirSync(out, { recursive: true });

const browser = await chromium.launch({
  executablePath: process.env.CHROMIUM_PATH || "/opt/pw-browsers/chromium",
});
const report = [];

for (const width of widths) {
  for (const theme of themes) {
    for (const mode of details) {
      const context = await browser.newContext({
        viewport: { width, height: width < 768 ? 812 : width < 1280 ? 1024 : 960 },
        deviceScaleFactor: 1,
        colorScheme: theme === "light" ? "light" : "dark",
        hasTouch: width < 768,
        isMobile: width < 768,
      });
      await context.addInitScript(
        ([themeValue, detailValue, sections]) => {
          try {
            localStorage.setItem("restream.theme", themeValue);
            for (const section of sections) localStorage.setItem(`restream.detail.${section}`, detailValue === "detail" ? "1" : "0");
          } catch {
            /* storage blocked: defaults apply */
          }
        },
        [theme, mode, DETAIL_SECTIONS],
      );
      const page = await context.newPage();
      if (password) {
        const login = await page.request.post(`${base}/api/v1/auth/login`, { data: { password } });
        if (!login.ok()) throw new Error(`login failed: ${login.status()}`);
      }
      if (role) {
        const viewAs = await page.request.post(`${base}/api/v1/auth/view-as`, { data: { role } });
        if (!viewAs.ok()) throw new Error(`view-as ${role} failed: ${viewAs.status()}`);
      }
      const errors = [];
      page.on("console", (message) => {
        if (message.type() === "error") errors.push(message.text());
      });
      page.on("pageerror", (error) => errors.push(String(error)));
      for (const workspace of workspaces) {
        errors.length = 0;
        const query = new URLSearchParams({ w: workspace });
        if (pipeline) query.set("p", pipeline);
        await page.goto(`${base}/?${query}`, { waitUntil: "domcontentloaded" });
        await page.waitForTimeout(settleMs);
        const checks = await page.evaluate((phone) => {
          const doc = document.documentElement;
          const horizontalScroll = doc.scrollWidth > doc.clientWidth + 1;
          const clipped = [];
          const smallTargets = [];
          for (const element of document.querySelectorAll("body *")) {
            const style = getComputedStyle(element);
            if (style.display === "none" || style.visibility === "hidden") continue;
            const rect = element.getBoundingClientRect();
            if (rect.width === 0 || rect.height === 0) continue;
            const hasText = [...element.childNodes].some(
              (node) => node.nodeType === 3 && node.textContent.trim().length > 0,
            );
            if (
              hasText &&
              style.overflowX !== "visible" === false &&
              element.scrollWidth > element.clientWidth + 2 &&
              style.textOverflow !== "ellipsis" &&
              style.overflowX !== "auto" &&
              style.overflowX !== "scroll"
            ) {
              clipped.push(element.textContent.trim().slice(0, 40));
            }
            if (
              phone &&
              element.matches("a[href], button, select, input, [role=button], [role=tab]") &&
              // WCAG 2.5.8 exempts links inline in text.
              !(element.tagName === "A" && style.display === "inline") &&
              element.type !== "checkbox" &&
              element.type !== "radio" &&
              element.type !== "range" &&
              (rect.height < 35.5 || rect.width < 35.5) &&
              // A control inside a touch-sized label is hit through it.
              !((target) => target && target.getBoundingClientRect().height >= 35.5)(element.closest("label.touch-36")) &&
              !element.closest("[data-dense-ok]")
            ) {
              smallTargets.push(
                `${element.tagName.toLowerCase()} ${(element.textContent || element.getAttribute("aria-label") || "").trim().slice(0, 24)} ${Math.round(rect.width)}x${Math.round(rect.height)}`,
              );
            }
          }
          return {
            horizontalScroll,
            scrollWidth: doc.scrollWidth,
            clientWidth: doc.clientWidth,
            clipped: clipped.slice(0, 12),
            clippedCount: clipped.length,
            smallTargets: smallTargets.slice(0, 12),
            smallTargetCount: smallTargets.length,
            domNodes: document.getElementsByTagName("*").length,
          };
        }, width < 768);
        const file = `${workspace}-${width}-${theme}-${mode}${role ? `-${role}` : ""}.png`;
        await page.screenshot({ path: join(out, file), fullPage });
        report.push({ workspace, width, theme, detail: mode, role: role || "admin", file, errors: [...errors], ...checks });
        const flags = [
          checks.horizontalScroll ? `HSCROLL(${checks.scrollWidth}>${checks.clientWidth})` : "",
          checks.clippedCount ? `clipped:${checks.clippedCount}` : "",
          checks.smallTargetCount ? `small:${checks.smallTargetCount}` : "",
          errors.length ? `errors:${errors.length}` : "",
        ]
          .filter(Boolean)
          .join(" ");
        console.log(`${file.padEnd(44)} nodes:${checks.domNodes} ${flags}`);
      }
      await context.close();
    }
  }
}

await browser.close();
writeFileSync(join(out, "report.json"), JSON.stringify(report, null, 2));
