import assert from "node:assert/strict";
import test from "node:test";

import { loadCompiledFrontendModule } from "../support/helpers/fake-dom.mjs";

const { guessKind, channelProblem, newlyOpened, tabTitle, base64UrlToBytes, channelHealth } = await loadCompiledFrontendModule(
  "control-room/notify-model.js",
);

const draft = (change) => ({ kind: "webhook", name: "ops", target: "https://x.example/h", minSeverity: "critical", onResolved: true, enabled: true, ...change });

test("a pasted URL suggests its channel kind", () => {
  assert.equal(guessKind("https://hooks.slack.com/services/T/B/x"), "slack");
  assert.equal(guessKind("https://discord.com/api/webhooks/1/abc"), "discord");
  assert.equal(guessKind("https://canary.discordapp.com/api/webhooks/1/abc"), "discord");
  assert.equal(guessKind("https://events.pagerduty.com/x"), null);
});

test("a draft needs a name and an http(s) URL", () => {
  assert.equal(channelProblem(draft({})), null);
  assert.equal(channelProblem(draft({ name: " " })), "Name the channel.");
  assert.equal(channelProblem(draft({ target: "ftp://x/y" })), "The target must be an http(s) URL.");
  assert.equal(channelProblem(draft({ target: "http://10.0.0.5:9000/hook" })), null, "an admin may post on the LAN");
});

test("in-tab notifications learn first, then announce only new alerts of the chosen severity", () => {
  const alert = (id, severity) => ({ id, severity, title: id, cause: "" });
  const now = [alert("a", "critical"), alert("b", "warning")];
  assert.deepEqual(newlyOpened(null, now, "warning"), [], "the first look only learns");
  const seen = new Set(["a"]);
  assert.deepEqual(newlyOpened(seen, now, "warning").map((a) => a.id), ["b"]);
  assert.deepEqual(newlyOpened(seen, now, "critical"), []);
});

test("a VAPID key decodes to its 65 bytes", () => {
  const bytes = base64UrlToBytes("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
  assert.equal(bytes.length, 65);
  assert.equal(bytes[0], 4, "uncompressed point");
  assert.equal(bytes[64], 0x0f);
});

test("each channel's health comes from its deliveries, latest first", () => {
  const health = channelHealth([
    { targetId: "c", ok: false, detail: "HTTP 503", atMs: 3 },
    { targetId: "c", ok: true, detail: "HTTP 200", atMs: 2 },
    { targetId: "d", ok: true, detail: "HTTP 200", atMs: 1 },
  ]);
  assert.deepEqual([health.get("c").sent, health.get("c").failed, health.get("c").last.detail], [1, 1, "HTTP 503"]);
  assert.equal(health.get("d").failed, 0);
});

test("the tab title counts open alerts and marks a critical one", () => {
  assert.equal(tabTitle("Fleet · Restream", []), "Fleet · Restream");
  assert.equal(tabTitle("Fleet · Restream", [{ severity: "warning" }, { severity: "warning" }]), "(2) Fleet · Restream");
  assert.equal(tabTitle("Fleet · Restream", [{ severity: "warning" }, { severity: "critical" }]), "(2!) Fleet · Restream");
});
