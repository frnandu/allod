// API client. Types mirror crates/allod-types and the allod-api routes; from M2 on
// they are generated from openapi.yaml (SPEC §11.3).
import { nip98Header } from "./nip98";
import { clearSession, getSession, setSession, type Session } from "./session";
import type { Signer } from "./signer";

export type HealthStatus = "ok" | "degraded" | "down";

export interface Check {
  ok: boolean;
  detail?: string;
}

export interface NodeSummary {
  pubkey: string;
  block_height: number;
  block_hash: string;
}

export interface HealthResponse {
  status: HealthStatus;
  db: Check;
  ldk_server: Check;
  node?: NodeSummary;
}

export interface InfoResponse {
  version: string;
  setup_completed: boolean;
  running: boolean;
  backend_type: string;
  network?: string;
  deployment: {
    mode: "managed" | "standalone";
    platform: "docker" | "umbrel" | "startos" | "other";
    chain_mode?: string;
  };
}

export interface SetupProgress {
  setup_completed: boolean;
  mode: "managed" | "standalone";
  seed_handoff: "not_started" | "waiting_for_ldk_server" | "done" | "not_applicable";
  credentials: { state: "not_applicable" | "waiting" | "done"; detail?: string };
  node?: { pubkey?: string; block_height?: number; network?: string; error?: string } | null;
  bitcoind?: {
    chain?: string;
    blocks?: number;
    headers?: number;
    verification_progress?: number;
    initial_block_download?: boolean;
    pruned?: boolean;
    /** Null while bitcoind is still syncing: there is nothing to ask yet. */
    fees?: FeeEstimates | null;
    error?: unknown;
  } | null;
}

/** bitcoind's fee estimates for the block targets ldk-node asks for. */
export interface FeeEstimates {
  /** null in light modes: no bitcoind of Allod's own, so nothing to show. */
  source: "bitcoind" | null;
  available: boolean;
  targets: { blocks: number; sat_per_vb: number | null }[];
  /** Block targets bitcoind cannot answer yet. */
  missing: number[];
  /** Why bitcoind could not be asked at all. */
  error?: string | null;
}

export type SetupBody =
  | { mode: "create" }
  | { mode: "restore"; recovery_phrase: string; birthday_height?: number }
  | {
      mode: "standalone";
      ldk_server_address: string;
      ldk_server_tls_cert_pem: string;
      ldk_server_macaroon_hex: string;
      recovery_phrase?: string;
    };

export class ApiError extends Error {
  constructor(
    public status: number,
    public code: string,
    message: string,
  ) {
    super(message);
  }
}

async function parse<T>(res: Response): Promise<T> {
  const text = await res.text();
  const body = text ? JSON.parse(text) : null;
  if (!res.ok) {
    const code = body?.error?.code ?? String(res.status);
    const message = body?.error?.message ?? `${res.status} ${res.statusText}`;
    if (res.status === 401) clearSession();
    throw new ApiError(res.status, code, message);
  }
  return body as T;
}

function authHeaders(): Record<string, string> {
  const s = getSession();
  return s ? { Authorization: `Bearer ${s.token}` } : {};
}

/** GET with the session, for SWR. */
export async function fetcher<T>(path: string): Promise<T> {
  return parse<T>(await fetch(path, { headers: { Accept: "application/json", ...authHeaders() } }));
}

function absolute(path: string): string {
  return `${window.location.origin}${path}`;
}

/** POST signed with NIP-98 (setup, login, re-authentication). */
async function signedPost<T>(signer: Signer, path: string, body: unknown, withSession = false): Promise<T> {
  const text = body === undefined ? "" : JSON.stringify(body);
  const headers: Record<string, string> = {
    Accept: "application/json",
    Authorization: await nip98Header(signer, absolute(path), "POST", text),
  };
  if (text) headers["Content-Type"] = "application/json";
  const s = getSession();
  if (withSession && s) headers["X-Allod-Session"] = s.token;
  return parse<T>(await fetch(path, { method: "POST", headers, body: text || undefined }));
}

export async function runSetup(signer: Signer, body: SetupBody) {
  const res = await signedPost<Session & { recovery_phrase?: string }>(signer, "/api/setup", body);
  setSession({ token: res.token, expires_at: res.expires_at, pubkey: res.pubkey });
  return res;
}

export async function login(signer: Signer): Promise<Session> {
  const { challenge } = await fetcher<{ challenge: string }>("/api/auth/challenge");
  const s = await signedPost<Session>(signer, "/api/auth/login", { challenge });
  setSession(s);
  return s;
}

export async function logout() {
  try {
    await fetch("/api/auth/logout", { method: "POST", headers: authHeaders() });
  } finally {
    clearSession();
  }
}

export type RateSource = "yadio" | "mempool" | "coingecko";

export interface Settings {
  fiat_currency: string;
  fiat_source: RateSource;
  fiat_source_host: string;
}

export const RATE_SOURCES: { id: RateSource; name: string; host: string; note: string }[] = [
  { id: "yadio", name: "Yadio", host: "api.yadio.io", note: "About 145 currencies" },
  { id: "mempool", name: "mempool.space", host: "mempool.space", note: "7 major currencies" },
  { id: "coingecko", name: "CoinGecko", host: "api.coingecko.com", note: "About 45 currencies" },
];

export async function saveSettings(patch: Partial<{ fiat_currency: string; fiat_source: RateSource }>): Promise<Settings> {
  return parse<Settings>(
    await fetch("/api/settings", {
      method: "PATCH",
      headers: { "Content-Type": "application/json", Accept: "application/json", ...authHeaders() },
      body: JSON.stringify(patch),
    }),
  );
}

/** Display name of an ISO 4217 code, e.g. "ARS" -> "Argentine Peso". */
export function currencyName(code: string): string {
  try {
    return new Intl.DisplayNames(undefined, { type: "currency" }).of(code) ?? code;
  } catch {
    return code;
  }
}

// ---------------------------------------------------------------- generic calls

async function send<T>(method: string, path: string, body?: unknown): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json", ...authHeaders() };
  if (body !== undefined) headers["Content-Type"] = "application/json";
  return parse<T>(await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body) }));
}

export const api = {
  get: <T>(path: string) => fetcher<T>(path),
  post: <T>(path: string, body?: unknown) => send<T>("POST", path, body),
  patch: <T>(path: string, body?: unknown) => send<T>("PATCH", path, body),
  del: <T>(path: string) => send<T>("DELETE", path),
};

// ---------------------------------------------------------------- node (M2)

export interface NodeInfo {
  pubkey: string;
  alias?: string;
  network?: string;
  block_height: number;
  version: string;
  listening_addresses: string[];
  announcement_addresses: string[];
  node_uris: string[];
  capabilities: {
    bolt12: boolean;
    force_close: boolean;
    splicing: boolean;
    hold_invoices: boolean;
    /** LSPS2 LSP configured: Receive can ask for a just-in-time channel. */
    jit_receive: boolean;
  };
}

export interface Channel {
  channel_id: string;
  user_channel_id: string;
  counterparty_node_id: string;
  short_channel_id?: number;
  funding_txid?: string;
  capacity_sat: number;
  outbound_capacity_msat: number;
  inbound_capacity_msat: number;
  is_outbound: boolean;
  is_channel_ready: boolean;
  is_usable: boolean;
  is_announced: boolean;
  confirmations?: number;
  confirmations_required?: number;
  fee_base_msat?: number;
  fee_ppm?: number;
  cltv_expiry_delta?: number;
}

export interface Peer {
  node_id: string;
  address: string;
  is_persisted: boolean;
  is_connected: boolean;
}

export interface Balances {
  lightning: { total_msat: number; spendable_msat: number; receivable_msat: number };
  onchain: { total_sat: number; spendable_sat: number; reserved_sat: number; pending_from_closures_sat: number };
  accounts_total_msat: number | null;
  main_spendable_msat: number | null;
  in_flight_msat: number | null;
}

export interface LnPayment {
  id: string;
  kind: { type: string; txid?: string; confirmed_height?: number | null };
  direction: "inbound" | "outbound";
  status: "pending" | "succeeded" | "failed";
  amount_msat?: number;
  fee_msat?: number;
  updated_at: number;
}

// ---------------------------------------------------------------- wallet (M3)

export type TxState = "PENDING" | "ACCEPTED" | "SETTLED" | "FAILED" | "EXPIRED" | "CANCELED";

export interface Tx {
  public_id: string;
  app_id: number | null;
  direction: "incoming" | "outgoing";
  payment_type: "bolt11" | "bolt12" | "internal";
  state: TxState;
  amount_msat: number;
  fee_msat: number;
  fee_reserve_msat: number;
  payment_request?: string | null;
  payment_hash?: string | null;
  preimage?: string | null;
  offer_id?: string | null;
  offer?: string | null;
  payer_note?: string | null;
  payer_proof?: string | null;
  description: string;
  self_payment: boolean;
  hold: boolean;
  settle_deadline?: number | null;
  failure_reason?: string | null;
  labels: string[];
  created_at: number;
  updated_at: number;
  expires_at?: number | null;
  settled_at?: number | null;
  counterparty?: Counterparty;
}

/** Who a payment came from or went to, as the payment says (not verified). */
export interface Counterparty {
  pubkey?: string;
  name?: string;
  identifier?: string;
  comment?: string;
  source: "zap" | "lnurl" | "bolt12" | "contact";
}

/** One way to pay a contact (BOLT12 offers come first). */
export interface PayTarget {
  kind: "bolt12" | "lightning_address" | "lnurl";
  value: string;
  /** nip-b1 (kind 10058) | payto (kind 10133, NIP-A3) | profile (kind 0) */
  source: "nip-b1" | "payto" | "profile";
}

/** A Nostr profile; `pay_target` is the preferred of its `targets`. */
export interface Profile {
  pubkey: string;
  name?: string;
  /** NIP-01 `name` when `name` shows the display name. */
  username?: string;
  targets?: PayTarget[];
  picture?: string;
  nip05?: string;
  lud16?: string;
  lud06?: string;
  offer?: string;
  pay_target?: string;
}

export interface Offer {
  offer_id: string;
  offer: string;
  app_id: number | null;
  amount_msat: number | null;
  description: string | null;
  state: "active" | "disabled" | "used" | "expired";
  expires_at: number | null;
  created_at: number;
}

export interface Decoded {
  kind: "bolt11" | "bolt12" | "onchain" | "lnurl";
  /** Lightning address / LNURL limits and LUD-12 comment length. */
  min_msat?: number;
  max_msat?: number;
  comment_allowed?: number;
  amount_msat: number | null;
  description: string | null;
  payee: string | null;
  expires_at: number | null;
  is_expired: boolean;
  network_ok: boolean;
  self_payment: boolean;
  address: string | null;
}

export async function revealRecoveryPhrase(signer: Signer): Promise<string> {
  const res = await signedPost<{ recovery_phrase: string }>(signer, "/api/recovery-phrase", undefined, true);
  return res.recovery_phrase;
}

// ---------------------------------------------------------------- connections (M4)

export interface Budget {
  used_budget: number;
  total_budget: number;
  renews_at?: number;
  renewal_period: string;
}

export interface AppConn {
  id: number;
  name: string;
  description: string;
  kind: string;
  app_pubkey: string;
  wallet_pubkey: string | null;
  isolated: boolean;
  max_amount_msat: number;
  budget_renewal: string;
  expires_at: number | null;
  metadata: Record<string, unknown>;
  relay_urls: string[] | null;
  created_at: number;
  last_used_at: number | null;
  scopes: string[];
  budget: Budget | null;
  balance_msat: number | null;
  requests?: { id: number; method: string | null; state: string; created_at: number }[];
}

// ---------------------------------------------------------------- fast sync (AssumeUTXO)

export interface FastSyncStatus {
  state:
    | "downloading"
    | "download_error"
    | "waiting_for_headers"
    | "loading"
    | "load_interrupted"
    | "loaded"
    | "failed"
    | "not_needed"
    | "canceled";
  bytes?: number;
  total?: number;
  headers?: number;
  base_height?: number;
  /** While loading: starting | coins | writing | checking | finishing; `pct` for coins. */
  phase?: "starting" | "coins" | "writing" | "checking" | "finishing";
  pct?: number;
  coins?: number;
  error?: string;
  detail?: string;
}

export interface FastSyncInfo {
  available: boolean;
  requested_url?: string | null;
  status?: FastSyncStatus | null;
  suggestion?: { height: number; url: string; source: string } | null;
}
