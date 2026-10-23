// The control room's service worker: it shows the engine's Web Push alerts
// as system notifications, even with no dashboard tab open, and opens the
// incident view when one is clicked. It caches nothing.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

self.addEventListener("push", (event) => {
  let message = {};
  try {
    message = event.data ? event.data.json() : {};
  } catch {
    message = { title: "restream", body: event.data ? event.data.text() : "" };
  }
  const scope = self.registration.scope;
  event.waitUntil(
    self.registration.showNotification(message.title || "restream", {
      body: message.body || "",
      // One notification per alert: a resolution replaces its opening.
      tag: message.tag || undefined,
      renotify: true,
      requireInteraction: message.severity === "critical",
      icon: new URL("logo.png", scope).href,
      badge: new URL("logo.png", scope).href,
      data: { url: new URL((message.url || "/?w=incidents").replace(/^\//, ""), scope).href },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = event.notification.data && event.notification.data.url;
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((clients) => {
      for (const client of clients) {
        if ("focus" in client) {
          client.navigate(url).catch(() => undefined);
          return client.focus();
        }
      }
      return self.clients.openWindow(url);
    }),
  );
});
