// NWC-08: a client asks for a connection to its own key, by an HTTP confirmation page
// (`/connections/new?pubkey=…`) or a `nostr+walletauth://` link.

export interface AuthRequest {
  pubkey: string;
  state: string;
  name: string | null;
  icon: string | null;
  returnTo: string | null;
  expiresAt: number | null;
  /** Millisatoshis per renewal period. */
  maxAmount: number | null;
  renewalPeriod: string | null;
  requestMethods: string[];
  optionalRequestMethods: string[];
  notificationTypes: string[];
  isolated: boolean;
  metadata: Record<string, unknown> | null;
  /** Nostr flow: the client's relays. Empty in the HTTP flow. */
  relays: string[];
}

/** What a client can be given this way. The server decides; this is for the page. */
export const METHODS: Record<string, string> = {
  get_info: "Read node info",
  get_budget: "Read its own budget",
  get_balance: "Read balance",
  pay_invoice: "Pay invoices",
  multi_pay_invoice: "Pay several invoices at once",
  pay: "Pay invoices, offers and Lightning addresses",
  make_invoice: "Create invoices",
  make_hold_invoice: "Create hold invoices",
  settle_hold_invoice: "Settle hold invoices",
  cancel_hold_invoice: "Cancel hold invoices",
  receive: "Create payment requests",
  make_offer: "Create offers",
  lookup_invoice: "Look up an invoice",
  lookup_payment: "Look up a payment",
  list_transactions: "Read history",
  sign_message: "Sign messages with the node key",
  register_push: "Register a device for notifications",
  unregister_push: "Remove a device from notifications",
};

export const SPENDING_METHODS = ["pay_invoice", "multi_pay_invoice", "pay"];

export const NOTIFICATIONS: Record<string, string> = {
  payment_received: "Payments received",
  payment_sent: "Payments sent",
  hold_invoice_accepted: "Hold invoices waiting to be settled",
};

const words = (v: string | null) => (v ?? "").split(/\s+/).filter(Boolean);
const uniq = (v: string[]) => [...new Set(v)];

/** Reads a request from the page's query. Returns the reason when it cannot be used. */
export function parseAuthRequest(q: URLSearchParams): AuthRequest | string {
  const pubkey = (q.get("pubkey") ?? "").toLowerCase();
  if (!/^[0-9a-f]{64}$/.test(pubkey)) return "The request has no valid app key.";
  const state = q.get("state") ?? "";
  if (!/^[0-9a-f]{32,128}$/.test(state)) {
    return "The request has no valid state value, so the app could not tell that the answer is Allod's.";
  }
  const number = (name: string): number | null | string => {
    const raw = q.get(name);
    if (raw == null || raw === "") return null;
    return /^\d+$/.test(raw) && Number(raw) > 0 ? Number(raw) : `The request's ${name} is not a positive number.`;
  };
  const maxAmount = number("max_amount");
  if (typeof maxAmount === "string") return maxAmount;
  const expiresAt = number("expires_at");
  if (typeof expiresAt === "string") return expiresAt;
  const renewalPeriod = q.get("renewal_period");
  if (renewalPeriod && !["never", "daily", "weekly", "monthly", "yearly"].includes(renewalPeriod)) {
    return "The request's renewal period is unknown.";
  }
  if (renewalPeriod && maxAmount == null) return "The request has a renewal period without a spending limit.";
  let metadata: Record<string, unknown> | null = null;
  const rawMeta = q.get("metadata");
  if (rawMeta) {
    try {
      const m: unknown = JSON.parse(rawMeta);
      if (!m || typeof m !== "object" || Array.isArray(m)) throw new Error("not an object");
      metadata = m as Record<string, unknown>;
    } catch {
      return "The request's metadata is not a JSON object.";
    }
  }
  const relays = uniq(q.getAll("relay").map((r) => r.trim()).filter(Boolean));
  if (relays.some((r) => !/^wss?:\/\//.test(r))) return "The request names a relay that is not a ws:// or wss:// address.";
  const requestMethods = uniq(words(q.get("request_methods")));
  return {
    pubkey,
    state,
    name: q.get("name")?.trim().slice(0, 100) || null,
    icon: q.get("icon"),
    returnTo: q.get("return_to"),
    expiresAt,
    maxAmount,
    renewalPeriod,
    requestMethods,
    optionalRequestMethods: uniq(words(q.get("optional_request_methods"))).filter((m) => !requestMethods.includes(m)),
    notificationTypes: uniq(words(q.get("notification_types"))),
    isolated: q.get("isolated") === "true",
    metadata,
    relays,
  };
}

/** `nostr+walletauth://<pubkey>?relay=…` (also `nostr+walletauth+allod://`) → the
 * confirmation page's path, or null when the text is not such a link. */
export function walletAuthPath(text: string): string | null {
  const m = /^nostr\+walletauth(?:\+[a-z0-9-]+)?:\/\/([0-9a-f]{64})\/?\?(.*)$/i.exec(text.trim());
  const [, key, query] = m ?? [];
  if (!key || query == null) return null;
  const q = new URLSearchParams(query);
  q.delete("pubkey");
  // The link form must name where the app listens.
  if (!q.getAll("relay").length) return null;
  q.set("pubkey", key.toLowerCase());
  return `/connections/new?${q.toString()}`;
}

/** An icon is only ever loaded over HTTPS. */
export function safeIcon(url: string | null): string | null {
  try {
    return url && new URL(url).protocol === "https:" ? url : null;
  } catch {
    return null;
  }
}

/** The app's `return_to` with the result added. Links that would run code in this page
 * (`javascript:`, `data:`…) are refused; app links (`myapp://…`) and web pages are fine. */
export function returnUrl(returnTo: string | null, result: Record<string, string | null | undefined>): string | null {
  if (!returnTo) return null;
  let url: URL;
  try {
    url = new URL(returnTo);
  } catch {
    return null;
  }
  if (["javascript:", "data:", "vbscript:", "blob:", "file:", "about:"].includes(url.protocol)) return null;
  for (const [k, v] of Object.entries(result)) if (v) url.searchParams.set(k, v);
  return url.toString();
}
