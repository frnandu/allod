// Allod service worker: shows Web Push notifications (SPEC §8.9). It caches nothing;
// the app always loads from the node.

self.addEventListener("install", () => self.skipWaiting());
self.addEventListener("activate", (event) => event.waitUntil(self.clients.claim()));

function sats(msat) {
  return `${Math.floor(msat / 1000).toLocaleString()} sats`;
}

/** Title and body for one payload: {v, type, transaction_id, account, amount_msat, description}. */
function render(p) {
  const amount = typeof p.amount_msat === "number" ? sats(p.amount_msat) : null;
  const where = p.account ? `Account: ${p.account}` : null;
  const body = [p.description, where].filter(Boolean).join(" · ");
  switch (p.type) {
    case "payment_received":
      return { title: amount ? `Received ${amount}` : "Payment received", body };
    case "payment_sent":
      return { title: amount ? `Sent ${amount}` : "Payment sent", body };
    case "hold_invoice_accepted":
      return { title: amount ? `Hold invoice accepted: ${amount}` : "Hold invoice accepted", body: body || "Settle or cancel it before it expires." };
    case "test":
      return { title: "Allod notifications work", body: "This device will be told about payments." };
    default:
      return { title: "Allod", body: body || "New activity" };
  }
}

self.addEventListener("push", (event) => {
  let payload = {};
  try {
    payload = event.data ? event.data.json() : {};
  } catch {
    payload = {};
  }
  const { title, body } = render(payload);
  event.waitUntil(
    self.registration.showNotification(title, {
      body,
      tag: payload.transaction_id || `allod-${payload.type || "event"}`,
      icon: "/icon-192.png",
      badge: "/icon-192.png",
      data: { url: payload.transaction_id ? "/activity" : "/" },
    }),
  );
});

self.addEventListener("notificationclick", (event) => {
  event.notification.close();
  const url = (event.notification.data && event.notification.data.url) || "/";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then((windows) => {
      for (const w of windows) {
        if (new URL(w.url).origin === self.location.origin && "focus" in w) {
          if ("navigate" in w) w.navigate(url).catch(() => {});
          return w.focus();
        }
      }
      return self.clients.openWindow(url);
    }),
  );
});
