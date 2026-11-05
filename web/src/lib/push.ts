// Device notifications over Web Push (SPEC §8.9): the service worker, the browser's
// push subscription and its registration with Allod.

import { api } from "./api";

export type PushEvent = "payment_received" | "payment_sent" | "hold_invoice_accepted";

export const PUSH_EVENTS: { id: PushEvent; label: string }[] = [
  { id: "payment_received", label: "Payments received" },
  { id: "payment_sent", label: "Payments sent" },
  { id: "hold_invoice_accepted", label: "Hold invoices accepted" },
];

export interface PushSubscriptionView {
  id: number;
  label: string;
  endpoint_host: string;
  events: PushEvent[];
  hide_amounts: boolean;
  created_at: number;
  last_success_at: number | null;
  last_error: string | null;
  failures: number;
}

export interface PushInfo {
  vapid_public_key: string;
  events: PushEvent[];
  subscriptions: PushSubscriptionView[];
}

/** Why this browser cannot receive notifications, or null when it can. */
export type Unsupported = "insecure" | "ios-not-installed" | "browser";

export function isIos(): boolean {
  return /iPad|iPhone|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);
}

function isStandalone(): boolean {
  return (
    window.matchMedia?.("(display-mode: standalone)").matches ||
    (navigator as Navigator & { standalone?: boolean }).standalone === true
  );
}

export function unsupported(): Unsupported | null {
  if (!window.isSecureContext) return "insecure";
  const capable = "serviceWorker" in navigator && "PushManager" in window && "Notification" in window;
  // iOS offers Web Push only to web apps added to the Home Screen (iOS 16.4+).
  if (isIos() && !isStandalone()) return "ios-not-installed";
  return capable ? null : "browser";
}

/** A short name for this browser, used as the device label. */
export function deviceLabel(): string {
  const ua = navigator.userAgent;
  const browser = /Edg\//.test(ua)
    ? "Edge"
    : /Firefox\//.test(ua)
      ? "Firefox"
      : /Chrome\//.test(ua)
        ? "Chrome"
        : /Safari\//.test(ua)
          ? "Safari"
          : "Browser";
  const os = /Android/.test(ua)
    ? "Android"
    : isIos()
      ? "iOS"
      : /Mac OS X/.test(ua)
        ? "macOS"
        : /Windows/.test(ua)
          ? "Windows"
          : /Linux/.test(ua)
            ? "Linux"
            : "";
  return os ? `${browser} on ${os}` : browser;
}

function keyBytes(base64url: string): Uint8Array<ArrayBuffer> {
  const pad = "=".repeat((4 - (base64url.length % 4)) % 4);
  const raw = atob((base64url + pad).replace(/-/g, "+").replace(/_/g, "/"));
  const out = new Uint8Array(new ArrayBuffer(raw.length));
  for (let i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i);
  return out;
}

function sameKey(a: ArrayBuffer | null, b: Uint8Array): boolean {
  if (!a) return false;
  const x = new Uint8Array(a);
  return x.length === b.length && x.every((v, i) => v === b[i]);
}

async function registration(): Promise<ServiceWorkerRegistration> {
  await navigator.serviceWorker.register("/sw.js", { scope: "/" });
  return navigator.serviceWorker.ready;
}

/** This browser's current subscription, if any. */
export async function currentSubscription(): Promise<PushSubscription | null> {
  if (unsupported()) return null;
  const reg = await navigator.serviceWorker.getRegistration("/");
  return (await reg?.pushManager.getSubscription()) ?? null;
}

async function register(sub: PushSubscription, label?: string): Promise<PushSubscriptionView> {
  const json = sub.toJSON();
  return api.post<PushSubscriptionView>("/api/push/subscriptions", {
    endpoint: json.endpoint,
    keys: json.keys,
    ...(label ? { label } : {}),
  });
}

/** Asks for permission, subscribes this browser and registers it with Allod. */
export async function enablePush(vapidKey: string): Promise<PushSubscriptionView> {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") throw new Error("Notifications are blocked for this site. Allow them in the browser's site settings.");
  const reg = await registration();
  const key = keyBytes(vapidKey);
  let sub = await reg.pushManager.getSubscription();
  // A subscription made for another server key (e.g. after a restore) cannot be reused.
  if (sub && !sameKey(sub.options.applicationServerKey, key)) {
    await sub.unsubscribe();
    sub = null;
  }
  sub ??= await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: key });
  return register(sub, deviceLabel());
}

/** Unsubscribes this browser; Allod forgets it through `DELETE`. */
export async function disablePush(id: number | null): Promise<void> {
  const sub = await currentSubscription();
  await sub?.unsubscribe();
  if (id !== null) await api.del(`/api/push/subscriptions/${id}`);
}

/**
 * Re-registers this browser's subscription (keys can rotate), keeping its preferences.
 * Returns its id, or null when the browser has none.
 */
export async function syncPush(): Promise<number | null> {
  if (unsupported() || Notification.permission !== "granted") return null;
  const sub = await currentSubscription();
  if (!sub) return null;
  return (await register(sub)).id;
}
