// The fixture engine inside the page, for the static demo
// (scripts/dev/frontend/static-demo.mjs): `fetch` and `EventSource` calls
// to the engine's routes are answered by createFixtureApi, with live state
// deltas every 2 s. Everything else goes to the network as usual.

import { createFixtureApi } from "./api.mjs";

const api = createFixtureApi({ preset: globalThis.__RESTREAM_DEMO_PRESET__ ?? "plant", seed: 7 });
const ENGINE = /^\/(api\/|metrics\/system$|healthz$|hls\/)/;

/** The engine route a URL names, under the page's base path, or null. */
function engineRoute(input) {
  const url = new URL(typeof input === "string" ? input : input instanceof URL ? input.href : input.url, location.href);
  if (url.origin !== location.origin) return null;
  const base = globalThis.__RESTREAM_BASE_PATH__ ?? "";
  const pathname = base && url.pathname.startsWith(base) ? url.pathname.slice(base.length) : url.pathname;
  return ENGINE.test(pathname) ? { pathname, searchParams: url.searchParams } : null;
}

const networkFetch = globalThis.fetch.bind(globalThis);
globalThis.fetch = async (input, init = {}) => {
  const route = engineRoute(input);
  if (!route) return networkFetch(input, init);
  const method = (init.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
  let body = null;
  if (typeof init.body === "string") {
    try {
      body = JSON.parse(init.body);
    } catch {
      body = {};
    }
  }
  // A beat of latency, so loading states show as they would.
  await new Promise((resolve) => setTimeout(resolve, 40));
  const answer = route.pathname.startsWith("/hls/") ? null : api.handle(method, route.pathname, route.searchParams, body);
  const status = answer?.status ?? 404;
  const payload = answer?.body ?? { error: "the demo has no data for this route" };
  return new Response(JSON.stringify(payload), { status, headers: { "content-type": "application/json" } });
};

const streams = new Set();
setInterval(() => {
  const delta = api.tick(Date.now());
  if (!delta) return;
  for (const stream of streams) stream.emit("delta", delta);
}, 2000);

const NetworkEventSource = globalThis.EventSource;
class DemoEventSource extends EventTarget {
  static CONNECTING = 0;
  static OPEN = 1;
  static CLOSED = 2;
  constructor(url, options) {
    const route = engineRoute(url);
    if (!route) return new NetworkEventSource(url, options);
    super();
    this.url = String(url);
    this.withCredentials = false;
    this.readyState = 0;
    this.onopen = null;
    this.onmessage = null;
    this.onerror = null;
    setTimeout(() => {
      if (this.readyState === 2) return;
      this.readyState = 1;
      this.dispatch("open", new Event("open"));
      if (route.pathname === "/api/v1/state/stream") {
        streams.add(this);
        this.emit("snapshot", api.snapshot());
      }
    }, 30);
  }
  emit(type, data) {
    this.dispatch(type, new MessageEvent(type, { data: JSON.stringify(data) }));
  }
  dispatch(type, event) {
    this.dispatchEvent(event);
    const handler = this[`on${type}`];
    if (typeof handler === "function") handler.call(this, event);
  }
  close() {
    this.readyState = 2;
    streams.delete(this);
  }
}
globalThis.EventSource = DemoEventSource;
