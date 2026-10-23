// Synthetic notification state for the fixture server: a few shared
// channels, a delivery log with the occasional failure, and a push key.

import { rng } from "./density.mjs";

export function createNotify({ seed = 7, alerts = [], nowMs = Date.now() } = {}) {
  const random = rng(seed * 17 + 3);
  const channels = [
    { id: "ch_slack", kind: "slack", name: "#ops-alerts", target: "https://hooks.slack.com/services/T000/B000/xxxx", minSeverity: "warning", onResolved: true, enabled: true, createdAtMs: nowMs - 40 * 86_400_000 },
    { id: "ch_pager", kind: "webhook", name: "PagerDuty bridge", target: "https://events.pagerduty.example/restream", minSeverity: "critical", onResolved: true, enabled: true, createdAtMs: nowMs - 30 * 86_400_000 },
    { id: "ch_discord", kind: "discord", name: "Broadcast Discord", target: "https://discord.com/api/webhooks/1/abc", minSeverity: "critical", onResolved: false, enabled: false, createdAtMs: nowMs - 9 * 86_400_000 },
  ];
  const deliveries = [];
  let atMs = nowMs - 3 * 3_600_000;
  for (let id = 1; id <= 160; id += 1) {
    atMs += random.int(20, 90) * 1000;
    const alert = alerts.length ? random.pick(alerts) : { id: "a", title: "Destination down", severity: "warning" };
    const channel = alert.severity === "critical" ? random.pick(channels.slice(0, 2)) : channels[0];
    const ok = !random.chance(0.06);
    deliveries.push({
      id,
      atMs,
      targetId: channel.id,
      targetName: channel.name,
      event: random.chance(0.35) ? "resolved" : "opened",
      alertId: alert.id,
      title: alert.title,
      ok,
      attempts: ok ? (random.chance(0.1) ? 2 : 1) : 3,
      detail: ok ? (channel.kind === "slack" ? "HTTP 200" : "HTTP 202") : random.pick(["HTTP 503", "timed out after 10 s"]),
    });
  }
  deliveries.reverse();

  function handle(method, path, body) {
    if (path === "/api/v1/notifications/channels" && method === "GET") return { body: { channels } };
    if (path === "/api/v1/notifications/channels" && method === "POST") {
      const channel = { ...body, id: `ch_${Date.now().toString(16)}`, createdAtMs: Date.now() };
      channels.push(channel);
      return { status: 201, body: { channel, secret: body.kind === "webhook" ? "whsec_fixture_0123456789abcdef" : null } };
    }
    const one = path.match(/^\/api\/v1\/notifications\/channels\/([^/]+)(\/test|\/secret)?$/);
    if (one) {
      const index = channels.findIndex((channel) => channel.id === one[1]);
      if (index < 0) return { status: 404, body: { error: "no such channel" } };
      const channel = channels[index];
      if (one[2] === "/test") {
        const delivery = { id: (deliveries[0]?.id ?? 0) + 1, atMs: Date.now(), targetId: channel.id, targetName: channel.name, event: "test", alertId: "test", title: "Test notification from restream", ok: true, attempts: 1, detail: "HTTP 200" };
        deliveries.unshift(delivery);
        return { body: delivery };
      }
      if (one[2] === "/secret") return { body: { secret: channel.kind === "webhook" ? "whsec_fixture_0123456789abcdef" : null } };
      if (method === "PUT") {
        channels[index] = { ...channel, ...body, kind: channel.kind };
        return { body: channels[index] };
      }
      if (method === "DELETE") {
        channels.splice(index, 1);
        return { body: { deleted: true } };
      }
    }
    if (path === "/api/v1/notifications/deliveries") return { body: { deliveries } };
    if (path === "/api/v1/notifications/push/key") {
      return { body: { publicKey: "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8" } };
    }
    if (path === "/api/v1/notifications/push/subscriptions" && method === "GET") return { body: { subscriptions: [] } };
    return undefined;
  }

  return { handle };
}
