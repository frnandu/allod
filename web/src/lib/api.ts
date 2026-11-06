// API client. Types mirror crates/allod-types and the allod-api routes; from M2 on
// they are generated from openapi.yaml (SPEC §11.3).
import { nip98Header } from "./nip98";
import { clearSession, getSession, setSession, type Session } from "./session";
import { lockLocalKey, type Signer } from "./signer";

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
    /** Explorer link template with `{pubkey}`; absent: no links. */
    node_explorer_url?: string;
    /** Testing only: setup may skip the recovery phrase check (`ALLOD_SKIP_BACKUP_CHECK`). */
    skip_backup_check?: boolean;
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
    /** When bitcoind gave these figures (unix seconds). A slow bitcoind's last answer is
     * served until the next one arrives. */
    updated_at?: number;
    /** Why the latest query failed, when the figures are from an earlier one. */
    last_error?: string;
    /** Recent answers, oldest first: [unix seconds, blocks, verification progress]. */
    history?: [number, number | null, number | null][];
    /** Null while bitcoind is still syncing: there is nothing to ask yet. */
    fees?: FeeEstimates | null;
    error?: unknown;
  } | null;
  /** Local gossip sidecar, present when ldk-server takes its graph from it. */
  gossip?: { status?: GossipSidecarStatus | null; error?: string | null } | null;
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

export async function claimSetup(token: string): Promise<void> {
  const res = await fetch("/api/setup/claim", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ token }),
  });
  if (!res.ok) await parse(res);
}

export async function hasSetupClaim(): Promise<boolean> {
  const res = await fetch("/api/setup/claim", { headers: { Accept: "application/json" } });
  return res.ok;
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
    // A key kept in this browser needs its password again after signing out.
    lockLocalKey();
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

/** default = the stack's `.env` configuration (ALLOD_CHAIN). */
export type ChainKind = "default" | "bitcoind" | "electrum" | "esplora";

/** The owner's choice, as the API returns it (never the RPC password). */
export interface ChainSourceView {
  kind: ChainKind;
  url?: string;
  rpc_address?: string;
  rpc_user?: string;
  /** The stack's built-in (standby) Bitcoin node. */
  standby?: boolean;
}

export interface ChainSourceRequest extends ChainSourceView {
  /** Empty keeps the stored password for the same address and user. */
  rpc_password?: string;
}

export interface ChainSourcePreset {
  id: string;
  name: string;
  kind: "electrum" | "esplora";
  url: string;
}

/** What ldk-server reported after its last start. */
export interface ActiveChainSource {
  mode: string;
  kind: "bitcoind" | "electrum" | "esplora";
  url?: string;
  rpc_address?: string;
}

/** `GET /api/settings/chain-source` (SPEC §2.3.2). */
export interface ChainSourceSettings {
  /** ALLOD_CHAIN of the stack. */
  default_mode: string | null;
  network: string;
  /** Only an ldk-server that Allod manages can be switched. */
  editable: boolean;
  active: ActiveChainSource | null;
  requested: ChainSourceView | null;
  restart_pending: boolean;
  presets: ChainSourcePreset[];
  /** Built-in pruned node the package ships idle next to an external one; null: none. */
  standby: StandbyBitcoind | null;
}

/** SPEC §2.3.2: offered when the external Bitcoin node goes away (e.g. Umbrel's app removed). */
export interface StandbyBitcoind {
  /** Started (it keeps running). */
  enabled: boolean;
  /** The Lightning node is set to use it. */
  in_use: boolean;
  /** Unix time of the first failed check of the external node in the current outage. */
  outage_since: number | null;
  outage_error: string | null;
  /** Unreachable for a few minutes while the node depends on it: suggest the built-in node. */
  suggested: boolean;
}

/** Starts the built-in node and switches the Lightning node to it. */
export async function startStandbyBitcoind(): Promise<ChainSourceSettings> {
  return parse<ChainSourceSettings>(
    await fetch("/api/settings/chain-source/standby", {
      method: "POST",
      headers: { Accept: "application/json", ...authHeaders() },
    }),
  );
}

export async function saveChainSource(source: ChainSourceRequest): Promise<ChainSourceSettings> {
  return parse<ChainSourceSettings>(
    await fetch("/api/settings/chain-source", {
      method: "PUT",
      headers: { "Content-Type": "application/json", Accept: "application/json", ...authHeaders() },
      body: JSON.stringify(source),
    }),
  );
}

// ---------------------------------------------------------------- appliance network (SPEC §2.3.10)

export interface NetworkDevice {
  device: string;
  /** NetworkManager device type: ethernet, wifi, ... */
  type: string;
  /** connected, disconnected, unavailable, ... */
  state: string;
  connection: string;
  ipv4: string[];
}

/** `GET /api/system/network`; `available` is false outside the appliance images. */
export interface NetworkStatus {
  available: boolean;
  hostname?: string;
  /** Wi-Fi regulatory country (ISO 3166 alpha-2), null when unset. */
  country?: string | null;
  /** The setup hotspot is open (no other network). */
  hotspot?: boolean;
  wifi_device?: string | null;
  wifi?: { ssid: string; signal: number } | null;
  devices?: NetworkDevice[];
  /** Saved Wi-Fi networks. */
  saved?: string[];
}

export interface WifiNetwork {
  ssid: string;
  signal: number;
  /** Empty for an open network, e.g. "WPA2" or "WPA2 WPA3" otherwise. */
  security: string;
  in_use: boolean;
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
  put: <T>(path: string, body?: unknown) => send<T>("PUT", path, body),
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

/** `allod-gossip` sidecar status (SPEC §2.3.2 rule 5). */
export interface GossipSidecarStatus {
  state: "starting" | "syncing" | "verifying" | "ready" | "degraded" | string;
  ready: boolean;
  started_at: number;
  ready_at?: number | null;
  initial_sync_secs?: number | null;
  peers_configured: number;
  peers_connected: number;
  graph_channels: number;
  graph_nodes: number;
  last_graph_change_at?: number | null;
  verify: {
    enabled: boolean;
    scans_completed: number;
    scanning: boolean;
    scan_progress?: number | null;
    last_scan_started_at?: number | null;
    last_scan_finished_at?: number | null;
    last_scan_secs?: number | null;
    last_scan_height?: number | null;
    last_scan_txouts?: number | null;
    unspent_channels: number;
    missing_channels: number;
    pending_channels: number;
    unspent_capacity_sats: number;
    /** Set while the first scan waits for bitcoind's initial block download. */
    waiting_for_chain?: { synced: boolean; blocks: number; headers: number; progress: number } | null;
    last_error?: string | null;
  };
  snapshot: {
    served: number;
    last_served_at?: number | null;
    last_bytes?: number | null;
    channels?: number | null;
    nodes?: number | null;
    updates?: number | null;
    channels_with_capacity?: number | null;
    build_ms?: number | null;
  };
  last_error?: string | null;
}

export interface GossipStatus {
  source: "p2p" | "sidecar";
  state: "waiting" | "learning" | "active" | "degraded";
  channels: number;
  nodes: number;
  last_graph_change_at?: number | null;
  channels_per_hour?: number | null;
  nodes_per_hour?: number | null;
  sidecar?: GossipSidecarStatus | null;
  sidecar_error?: string | null;
}

export interface Channel {
  channel_id: string;
  user_channel_id: string;
  counterparty_node_id: string;
  /** Counterparty alias from the node's own gossip graph (sanitized), if announced. */
  alias?: string | null;
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
  /** Alias from the node's own gossip graph (sanitized), if announced. */
  alias?: string | null;
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

export type TxState = "PENDING" | "ACCEPTED" | "CANCELING" | "SETTLED" | "FAILED" | "EXPIRED" | "CANCELED";

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
  /** Received through an LSPS2 just-in-time channel. */
  jit: boolean;
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

/** Checks words (0-based position → word) against the stored phrase (SPEC §13.3). */
export async function verifyBackup(words: Record<number, string>): Promise<void> {
  await api.post("/api/backup/verify", { words });
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

// ---------------------------------------------------------------- accounts (M6)

/** An account (SPEC §10): an isolated app with its own balance and NWC connection. */
export interface Account extends AppConn {
  lud16_username: string | null;
  /** Set once closed; closed accounts are listed only while they still hold funds. */
  closed_at: number | null;
  balance_msat: number;
}

export interface CreatedAccount {
  account: Account;
  pairing_uri: string;
  bip353: { name: string; txt: string; offer_id: string } | null;
}

export interface StatementLine {
  posting_id: number;
  entry_kind: string;
  memo: string;
  amount_msat: number;
  balance_msat: number;
  created_at: number;
  transaction_id: string | null;
  description: string | null;
  direction: "incoming" | "outgoing" | null;
  payment_type: string | null;
}

export interface Statement {
  items: StatementLine[];
  total: number;
  balance_msat: number;
}

/**
 * Closes an account. Sweeping a balance to Main needs a fresh NIP-98 signature over
 * this exact request (SPEC §11.1).
 */
export async function closeAccount(id: number, sweep: Signer | null): Promise<{ account: Account; sweep: Tx | null }> {
  const path = sweep ? `/api/accounts/${id}?sweep=true` : `/api/accounts/${id}`;
  const headers: Record<string, string> = { Accept: "application/json" };
  const s = getSession();
  if (sweep) {
    headers.Authorization = await nip98Header(sweep, absolute(path), "DELETE");
    if (s) headers["X-Allod-Session"] = s.token;
  } else if (s) {
    headers.Authorization = `Bearer ${s.token}`;
  }
  return parse(await fetch(path, { method: "DELETE", headers }));
}

// ---------------------------------------------------------------- public name (SPEC §12.1)

export type PublicNameMode = "none" | "own" | "desec";
export type Exposure = "private" | "public";

export interface Bip353Record {
  account_id: number;
  account_name: string;
  username: string;
  address: string;
  /** Fully qualified record name, with the trailing dot. */
  name: string;
  txt: string;
  /** Written at deSEC by Allod. */
  published: boolean;
}

export interface PublicName {
  mode: PublicNameMode;
  name: string | null;
  exposure: Exposure;
  /** Set by ALLOD_PUBLIC_URL: the settings are read-only. */
  env_public_url: string | null;
  public_allowed: { own: boolean; desec: boolean };
  lightning_domain: string | null;
  desec: {
    account_email: string | null;
    account_domain: string | null;
    has_token: boolean;
    pending: { email: string; started_at: number; expires_at: number } | null;
  };
  status: {
    ipv4: string | null;
    ipv6: string | null;
    ip_updated_at: number | null;
    errors: string[];
    reach: { state: "waiting_for_certificate" | "reachable" | "not_reachable"; checked_at: number; detail?: string } | null;
    synced_at: number | null;
    tls_check: boolean;
  };
  records: Bip353Record[];
  /** Present on GET: twelve.cash works only on mainnet. */
  twelve_cash_available?: boolean;
  twelve_cash: TwelveCashEntry[];
}

export interface TwelveCashEntry {
  /** "main" or the account id. */
  target: string;
  address: string;
  offer_id: string;
  custom: boolean;
  verified: boolean;
  /** False once the offer was disabled (account closed). */
  active: boolean;
  created_at: number;
}

export const PUBLIC_NAME_KEY = "/api/public-name";

export const publicName = {
  set: (body: { mode: PublicNameMode; name?: string; exposure?: Exposure }) => api.put<PublicName>(PUBLIC_NAME_KEY, body),
  refresh: () => api.post<PublicName>("/api/public-name/refresh"),
  captcha: () => api.post<{ id: string; challenge: string }>("/api/public-name/desec/captcha"),
  available: (label: string) =>
    api.get<{ label: string; valid: boolean; available: boolean | null }>(
      `/api/public-name/desec/available?label=${encodeURIComponent(label)}`,
    ),
  signup: (body: { label: string; email: string; captcha_id: string; captcha_solution: string }) =>
    api.post<PublicName>("/api/public-name/desec/signup", body),
  token: (name: string, token: string) => api.post<PublicName>("/api/public-name/desec/token", { name, token }),
  twelveAdd: (target: string, address?: string) =>
    api.post<PublicName>("/api/public-name/twelve-cash", { target, address }),
  twelveOffer: (target: string) =>
    api.get<{ offer: string }>(`/api/public-name/twelve-cash/offer?target=${encodeURIComponent(target)}`),
  twelveForget: (target: string) => api.del<PublicName>(`/api/public-name/twelve-cash/${encodeURIComponent(target)}`),
  /** The deSEC login Allod created; needs a fresh Nostr signature. */
  credentials: (signer: Signer) =>
    signedPost<{ email: string; password: string }>(signer, "/api/public-name/desec/credentials", undefined, true),
};
