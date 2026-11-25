// API client. Types mirror crates/allod-types and the allod-api routes; from M2 on
// they are generated from openapi.yaml (SPEC §11.3).
import { nip98Header } from "./nip98";
import { clearSession, getSession, setSession, type Session } from "./session";
import { forgetSigner, lockLocalKey, type Signer } from "./signer";

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

/** Bitcoin Core's status for the Node page (`GET /api/node/bitcoind`). */
export interface BitcoindStatus {
  /** null in light modes: no bitcoind of Allod's own, so nothing to show. */
  source: "bitcoind" | null;
  /** bitcoind never answered. */
  error?: string;
  /** The first chain query still waits for bitcoind (a long call holds its chain lock). */
  chain_pending?: boolean;
  /** Why the latest query failed, when the figures are from an earlier one. */
  last_error?: string;
  /** When bitcoind gave these figures (unix seconds). */
  updated_at?: number;
  /** e.g. 310100 for 31.1.0. */
  version?: number | null;
  subversion?: string | null;
  chain?: string | null;
  blocks?: number | null;
  headers?: number | null;
  best_block_hash?: string | null;
  best_block_time?: number | null;
  median_time?: number | null;
  verification_progress?: number | null;
  initial_block_download?: boolean | null;
  size_on_disk?: number | null;
  pruned?: boolean | null;
  prune_height?: number | null;
  prune_target_size?: number | null;
  uptime?: number | null;
  peers?: {
    total: number | null;
    inbound: number | null;
    outbound: number | null;
    network_active: boolean | null;
    reachable: string[];
  } | null;
  mempool?: {
    loaded: boolean | null;
    transactions: number | null;
    vbytes: number | null;
    usage: number | null;
    max: number | null;
    min_fee_sat_vb: number | null;
    total_fee_sat: number | null;
  } | null;
  relay_fee_sat_vb?: number | null;
  /** Set after a fast sync (AssumeUTXO): the check of every block up to the snapshot. */
  background_validation?: {
    active: boolean;
    complete: boolean;
    blocks?: number | null;
    /** The snapshot's base height. */
    target_height?: number | null;
    /** Transactions validated so far, and up to the snapshot's base block. */
    transactions?: number | null;
    target_transactions?: number | null;
    /** 0..1: transactions over target transactions, or blocks over target height. */
    progress?: number | null;
    progress_basis?: "transactions" | "blocks" | null;
    /** Recent validation rate and the time left at it, when progress counts transactions. */
    tx_per_sec?: number | null;
    eta_secs?: number | null;
    /** Bitcoin Core's own figure (as in its log): against today's tip, not the snapshot. */
    verification_progress?: number | null;
  } | null;
  /** Last confirmed blocks, newest first (`getblockstats`). */
  recent_blocks?: ConfirmedBlock[];
  /** The mempool cut into blocks in mining order, next block first; the last entry may
   * sum up several (`count`). Null before Bitcoin Core 31 or while syncing. */
  mempool_blocks?: ProjectedBlock[] | null;
  warnings?: string[];
  /** RPC calls that failed, by method. */
  errors?: Record<string, string>;
  /** Where a tip newer than bitcoind's chain answer came from: its log (bundled node), its
   * ZMQ notifications, or ldk-server (stale figures and neither of the others). */
  tip_source?: "ldk-server" | "log" | "zmq";
  /** bitcoind's log or ZMQ vouches for the tip: it is current, however old the rest is. */
  tip_live?: boolean;
  /** When the newer tip was seen (log or ZMQ; its block time is not known). */
  tip_seen_at?: number;
  /** When the mempool part was read (it does not wait for bitcoind's chain lock). */
  mempool_updated_at?: number;
}

/** A mined block. Fee rates in whole sat/vB, as Bitcoin Core gives them. */
export interface ConfirmedBlock {
  height: number;
  hash: string;
  time: number;
  txs: number;
  size: number;
  vsize: number | null;
  fee_sat: number;
  min_rate: number;
  median_rate: number;
  max_rate: number;
  percentiles: number[];
}

/** A block the mempool would make now; fee rates in sat/vB. */
export interface ProjectedBlock {
  vsize: number;
  fee_sat: number;
  min_rate: number | null;
  median_rate: number | null;
  max_rate: number | null;
  /** Blocks this entry stands for (more than one for the last, summing up the rest). */
  count: number;
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
    /** A BOLT11 invoice to pay from another wallet when the node cannot. */
    public invoice?: string,
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
    throw new ApiError(res.status, code, message, body?.error?.invoice ?? undefined);
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
    // Forget the signer choice too: otherwise SignIn would restore it and log straight back in.
    forgetSigner();
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

/** A text answer (e.g. a gateway's setup script). */
async function fetchText(path: string): Promise<string> {
  const res = await fetch(path, { headers: { Accept: "text/plain, */*", ...authHeaders() } });
  if (!res.ok) {
    const body = await res.json().catch(() => null);
    throw new Error(body?.error?.message ?? `HTTP ${res.status}`);
  }
  return res.text();
}

export const api = {
  get: <T>(path: string) => fetcher<T>(path),
  post: <T>(path: string, body?: unknown) => send<T>("POST", path, body),
  patch: <T>(path: string, body?: unknown) => send<T>("PATCH", path, body),
  put: <T>(path: string, body?: unknown) => send<T>("PUT", path, body),
  del: <T>(path: string) => send<T>("DELETE", path),
};

// ---------------------------------------------------------------- Lightning connectivity (SPEC §12.7.2)

export type LightningReachState = "reachable" | "no_node" | "unreachable" | "waiting" | "unchecked";
export interface LightningReach {
  state: LightningReachState;
  detail: string;
  checked_at: number;
  address: string | null;
}
/** The clearnet path peers use: none (private or Tor only), a gateway's IP, or the host's own IP. */
export type LightningClearnet = "none" | "gateway" | "host";

export interface LightningSettings {
  /** False when this installation has no such setting at all (no domain service). */
  available: boolean;
  /** Reachable at an onion address over Tor. */
  tor?: boolean;
  clearnet?: LightningClearnet;
  /** The gateway (kept while another path is chosen). */
  gateway_id?: number | null;
  /** Public port on that gateway. */
  port?: number | null;
  /** Public IP or DNS name of the host (home IP option). */
  host?: string | null;
  host_port?: number | null;
  /** The port Compose publishes on the host: the home IP option's default. */
  default_host_port?: number;
  /** What the node is told to announce. */
  addresses?: string[];
  /** The clearnet `host:port` peers connect to; null until known. */
  address?: string | null;
  /** `<onion>.onion:9735`, once tor-proxy published it. */
  onion_address?: string | null;
  /** ldk-server's own Lightning port. */
  node_port?: number;
  /** Allod manages ldk-server's configuration (it can set what the node announces). */
  managed?: boolean;
  /** The tunnel service runs here (needed for gateways). */
  tunnel?: boolean;
  /** The Tor service for the node's onion runs here. */
  tor_available?: boolean;
  /** The onion address can be checked (a Tor proxy is configured). */
  tor_checked?: boolean;
  /** The last checks, connecting as a peer would. */
  reach?: { clearnet?: LightningReach | null; tor?: LightningReach | null } | null;
  /** The node announces exactly `addresses` (null: unknown, node not ready). */
  announced?: boolean | null;
  node?: { pubkey: string; alias?: string | null; announcement_addresses: string[] } | null;
  gateways: {
    id: number;
    label: string;
    provider: "manual" | "lnvps";
    ipv4: string | null;
    mode: "dedicated" | "shared";
    state: GatewayState;
  }[];
}

export interface LightningConnectivityRequest {
  clearnet: LightningClearnet;
  tor: boolean;
  gateway_id?: number | null;
  port?: number;
  host?: string;
  host_port?: number;
}

export const LIGHTNING_SETTINGS = "/api/settings/lightning";

export const lightning = {
  set: (body: LightningConnectivityRequest) => api.put<LightningSettings>(LIGHTNING_SETTINGS, body),
  check: () => api.post<LightningSettings>(`${LIGHTNING_SETTINGS}/check`),
  /** The public IP this machine's outgoing connections use, as an echo service sees it. */
  detectIp: () => api.post<{ ip: string; source: string }>(`${LIGHTNING_SETTINGS}/detect-ip`),
};

// ---------------------------------------------------------------- node (M2)

export interface NodeInfo {
  pubkey: string;
  alias?: string;
  network?: string;
  block_height: number;
  /** ldk-server's version, `0.1.0 (<commit>)`. */
  version: string;
  /** The ldk-node and rust-lightning it is built on, when its image says (managed mode). */
  components?: {
    ldk_node?: { version: string; rev?: string | null } | null;
    lightning?: { version: string; rev?: string | null } | null;
  } | null;
  listening_addresses: string[];
  announcement_addresses: string[];
  node_uris: string[];
  /** When the Lightning / on-chain wallet last synced to the chain tip (unix seconds);
   * null until the first sync. Absent from older Allod versions (unknown, not syncing). */
  lightning_synced_at?: number | null;
  onchain_synced_at?: number | null;
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
  source: "zap" | "lnurl" | "bip353" | "bolt12" | "contact";
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
  /** BIP-353 name (user@domain) whose DNSSEC-verified record gave this payment. */
  bip353?: string;
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

/** `GET /api/allod/status`: Allod's own services; a part is null when it has not started. */
export interface AllodStatus {
  relays: {
    configured: string[];
    states: {
      updated_at: number;
      relays: { url: string; connected: boolean; status: string; connected_at: number | null }[];
    } | null;
  } | null;
  activity: {
    since: number;
    received: { count: number; msat: number; fee_msat: number };
    sent: { count: number; msat: number; fee_msat: number };
    failed_sends: number;
  } | null;
  held_invoices: { count: number; msat: number; next_deadline: number | null } | null;
  /** Every push subscription; `via_connections` of them registered by an app's connection. */
  push: { devices: number; failing: number; last_success_at: number | null; via_connections: number } | null;
  database: { backend: string; size_bytes: number | null; litestream: boolean | null };
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
  /** Set when the app asked for the connection itself (NWC-08): only these methods work. */
  methods?: string[] | null;
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

// ---------------------------------------------------------------- domains (SPEC §12.1, §12.7, §12.8)

export interface Nip05Name {
  id: number;
  account_id: number | null;
  domain_id: number;
  domain: string;
  username: string;
  pubkey: string;
  npub: string;
}

export const nip05 = {
  list: () => api.get<{ names: Nip05Name[] }>("/api/nip05"),
  add: (domain_id: number, username: string, npub: string, account_id?: number) =>
    api.post<{ names: Nip05Name[] }>("/api/nip05", { domain_id, username, npub, account_id }),
  update: (id: number, body: { domain_id?: number; username?: string; npub?: string; account_id?: number | null }) =>
    api.patch<{ names: Nip05Name[] }>(`/api/nip05/${id}`, body),
  setAccount: (id: number, account_id: number | null) =>
    api.patch<{ names: Nip05Name[] }>(`/api/nip05/${id}`, { account_id }),
  remove: (id: number) => api.del<void>(`/api/nip05/${id}`),
};

export type ReachState = "waiting_for_certificate" | "reachable" | "not_reachable";

/** One split of an address's prism: a share forwarded to another Lightning target. */
export interface PrismSplit {
  id: number;
  /** LNURL / Lightning address, BIP-353 name or BOLT12 offer. */
  destination: string;
  /** The owner's own note: what or who this destination is. Never sent anywhere. */
  note?: string | null;
  kind: "lnurl" | "bip353" | "bolt12" | "address";
  position: number;
  /** 1..10000 (0.01% .. 100%). */
  percent_bps: number;
  forwarded_msat: number;
  failed_msat: number;
  pending_msat: number;
  pending_count: number;
  failed_count: number;
  skipped_msat: number;
  last_error?: string | null;
  last_error_at?: number | null;
}

/** What an address's splits forwarded (and did not) so far. */
export interface PrismStats {
  forwarded_msat: number;
  failed_msat: number;
  pending_msat: number;
  pending_count: number;
  failed_count: number;
  skipped_msat: number;
  payments: number;
}

/** The prism of an address: splits, and the totals of every payout recorded. */
export interface Prism {
  splits: PrismSplit[];
  /** Sum of the split percentages, in basis points; the rest stays in the wallet. */
  split_bps: number;
  stats: PrismStats;
}

/** One payout: a split of one received payment, with what became of it. */
export interface PrismPayout {
  id: number;
  /** Where it was (or is) sent. A paid payout keeps the destination it went to. */
  destination: string;
  /** The note of the split this destination belongs to now, if any. */
  note?: string | null;
  kind: PrismSplit["kind"];
  amount_msat: number;
  fee_msat: number;
  /** The split's share when the payment was divided. */
  percent_bps: number;
  state: "PENDING" | "PAYING" | "PAID" | "FAILED" | "CANCELED" | "SKIPPED";
  attempts: number;
  last_error?: string | null;
  created_at: number;
  updated_at: number;
  paid_at?: number | null;
}

/** A page of an address's payout history, newest first. */
export interface PrismPayoutPage {
  payouts: PrismPayout[];
  /** The id to page before with "Show older"; null on the last page. */
  next_before_id: number | null;
}

/** One address whose split payouts could not be sent. */
export interface PrismFailureAddress {
  address_id: number;
  address: string;
  failed_count: number;
  failed_msat: number;
}

/** Failed split payouts across every address, for the warning in the menus. */
export interface PrismFailures {
  failed_count: number;
  failed_msat: number;
  addresses: PrismFailureAddress[];
}

/** One Lightning address on an own domain. */
export interface DomainAddress {
  id: number;
  domain_id: number;
  username: string;
  address: string;
  /** "main" or the account id. */
  target: string;
  account_name: string;
  /** Fully qualified TXT record name, with the trailing dot. */
  record_name: string;
  /** The TXT value (null until the address's offer exists). */
  txt: string | null;
  /** The TXT record is served with this offer. */
  verified: boolean;
  /** LNURL is on for it (it answers LNURL whenever its domain is served). */
  lnurl_enabled: boolean;
  /** It publishes a BIP-353 record (off: LNURL only). */
  bip353: boolean;
  /** LNURL answers now (on, and the domain is served and reachable). */
  lnurl: boolean;
  /** Splits of what it receives (prism); absent from a server older than the feature. */
  prism?: Prism;
}

export interface DomainRecord {
  type: "TXT" | "A";
  name: string;
  value: string | null;
  ok: boolean;
}

export interface DomainEntry {
  id: number;
  name: string;
  /** Set by ALLOD_PUBLIC_URL: cannot be changed here. */
  env: boolean;
  dns_provider_id: number | null;
  gateway_id: number | null;
  /** Served from this machine's public IP. */
  direct: boolean;
  /** Served over HTTPS (LNURL, certificate). */
  served: boolean;
  /** Its own checks pass (DNS, DNSSEC, provider, and HTTPS when served), with or without addresses. */
  live: boolean;
  /** The domain's own checks; null before the first one. */
  checks: { dns: boolean; dnssec: boolean; provider: boolean | null } | null;
  /** DS values for the registrar ("keytag algorithm digesttype digest"), from the DNS
   * provider (deSEC); SHA-256 first. */
  ds?: string[] | null;
  reach: { state: ReachState; checked_at: number | null; detail: string | null } | null;
  synced_at: number | null;
  error: string | null;
  addresses: DomainAddress[];
  records: DomainRecord[];
}

export type DnsProviderKind = "njalla" | "desec";

export interface DnsProviderEntry {
  id: number;
  kind: DnsProviderKind;
  label: string;
  checked_at: number | null;
  error: string | null;
  domains: string[];
}

export type GatewayState = "ordering" | "setup" | "active" | "offline" | "expired" | "error";

export interface GatewayEntry {
  id: number;
  label: string;
  provider: "manual" | "lnvps";
  ipv4: string | null;
  ipv6: string | null;
  mode: "dedicated" | "shared";
  state: GatewayState;
  /** This gateway's tunnel (derived from its key): interface `allod-<id>` on the server. */
  tunnel: { id: string; interface: string; gateway: string; home: string; port: number } | null;
  last_handshake_at: number | null;
  expires_at: number | null;
  auto_renew: boolean;
  renew_max_sats: number | null;
  error: string | null;
  created_at: number;
  domains: string[];
  /** Public port Lightning connections arrive on, when this gateway carries them. */
  lightning_port?: number | null;
  /** LNVPS purchase progress. */
  lnvps: {
    step: "order" | "pay" | "provision" | "setup" | "done" | "renew";
    plan: { region: string; image: string; price_msat: number; interval: string };
    email_needed?: boolean;
    /** A cap in a fiat currency (VAT and fees included), converted when an invoice arrives. */
    cap?: { currency: string; amount: number } | null;
    /** The address LNVPS was given (Allod's own `npub…@uid.ovh`, or the owner's). */
    email?: string | null;
    paid_sats?: number;
  } | null;
}

export interface Domains {
  domains: DomainEntry[];
  dns_providers: DnsProviderEntry[];
  gateways: GatewayEntry[];
  /** The tunnel service is configured (gateways can reach this node). */
  tunnel: boolean;
  lnvps: { account_pubkey: string | null };
  /** A deSEC account Allod made (or is making) with its uid.ovh address. */
  desec_signup: DesecSignup | null;
  /** A Njalla account Allod made (or is making) with its uid.ovh address. */
  njalla_signup: DesecSignup | null;
  /** A domain being bought at Njalla (or the last one). */
  njalla_purchase: NjallaPurchase | null;
  errors: string[];
  synced_at: number | null;
}

export interface NjallaPurchase {
  provider_id: number;
  domain: string;
  years: number;
  price_eur: number;
  dnssec: boolean;
  state: "topup" | "registering" | "done" | "failed";
  /** The bitcoin top-up of the Njalla wallet to pay (on-chain). */
  topup: {
    id: string;
    amount_eur: number;
    amount_btc: string;
    sats: number;
    address: string;
    uri: string;
    status: string | null;
  } | null;
  balance_eur: number | null;
  error: string | null;
  since: number;
  gateway_id: number | null;
  direct: boolean;
  domain_id: number | null;
}

export interface NjallaName {
  name: string;
  /** available, taken, unavailable, own, in progress, failed */
  status: string;
  /** Euros a year. */
  price: number;
}

export interface DesecSignup {
  email: string;
  /** "removed": its provider was removed; the account remains and can be reconnected. */
  state: "activating" | "done" | "failed" | "removed";
  since: number;
  error: string | null;
  provider_id: number | null;
}

export interface LnvpsQuote {
  /** Before VAT and fees. */
  price_sats: number;
  /** What the invoice will ask: VAT and fees included. */
  total_sats: number;
  /** The same total in LNVPS's currency, in cents. */
  total_fiat: { amount: number; currency: string };
  vat_rate: number;
  fee_rate: number;
  interval: string;
  plan: { region: string; image: string; cpu: number; memory: number; disk: number };
}

export const DOMAINS_KEY = "/api/domains";
export const PRISM_FAILURES_KEY = "/api/prism/failures";

export const domains = {
  get: () => api.get<Domains>(DOMAINS_KEY),
  check: () => api.post<Domains>("/api/domains/check"),
  add: (body: { name: string; dns_provider_id?: number | null; gateway_id?: number | null; direct?: boolean }) =>
    api.post<Domains>(DOMAINS_KEY, body),
  /** `null` clears a field; leaving it out keeps it. */
  update: (id: number, body: { dns_provider_id?: number | null; gateway_id?: number | null; direct?: boolean }) =>
    api.patch<Domains>(`/api/domains/${id}`, body),
  remove: (id: number) => api.del<Domains>(`/api/domains/${id}`),
  addAddress: (domainId: number, username: string, target: string, kinds?: { lnurl: boolean; bip353: boolean }) =>
    api.post<Domains>(`/api/domains/${domainId}/addresses`, { username, target, ...kinds }),
  removeAddress: (id: number) => api.del<Domains>(`/api/addresses/${id}`),
  /** BIP-353 only (`lnurl: false`), or LNURL too on a served domain. */
  updateAddress: (id: number, body: { domain_id?: number; username?: string; lnurl?: boolean; bip353?: boolean; target?: string }) =>
    api.patch<Domains>(`/api/addresses/${id}`, body),
  /** Replaces an address's prism splits. Percentages are basis points; the rest stays.
   * Sending the id of a stored split edits it in place: its note and the sats that failed
   * to arrive stay with it, also when its destination changes. */
  setPrism: (
    id: number,
    splits: { id?: number; destination: string; percent_bps: number; note?: string | null }[],
  ) => api.put<Domains>(`/api/addresses/${id}/prism`, { splits }),
  /** Pays failed splits again now. */
  retryPrism: (id: number) => api.post<Domains>(`/api/addresses/${id}/prism/retry`),
  /** What an address's splits did, newest first. `before`: page before that payout id. */
  payoutHistory: (id: number, before?: number | null) =>
    api.get<PrismPayoutPage>(
      `/api/addresses/${id}/prism/payouts?limit=20${before ? `&before=${before}` : ""}`,
    ),
  /** Clears what a split failed to send; nothing is paid. */
  resetPrismFailed: (id: number, splitId: number) =>
    api.post<Domains>(`/api/addresses/${id}/prism/reset`, { split_id: splitId }),
  addProvider: (body: { kind: DnsProviderKind; token: string; label?: string }) => api.post<Domains>("/api/dns-providers", body),
  updateProvider: (id: number, body: { label?: string; token?: string }) =>
    api.patch<Domains>(`/api/dns-providers/${id}`, body),
  removeProvider: (id: number) => api.del<Domains>(`/api/dns-providers/${id}`),
  desecCaptcha: () => api.post<{ id: string; challenge: string }>("/api/dns-providers/desec/captcha"),
  desecSignup: (captchaId: string, solution: string) =>
    api.post<Domains>("/api/dns-providers/desec/signup", { captcha_id: captchaId, solution }),
  desecCredentials: () => api.get<{ email: string; password: string | null }>("/api/dns-providers/desec/credentials"),
  njallaSignup: () => api.post<Domains>("/api/dns-providers/njalla/signup"),
  njallaSearch: (providerId: number, q: string) =>
    api.get<{ domains: NjallaName[] }>(`/api/njalla/search?provider_id=${providerId}&q=${encodeURIComponent(q)}`),
  njallaBuy: (body: { provider_id: number; domain: string; years: number; gateway_id: number | null; direct: boolean }) =>
    api.post<Domains>("/api/njalla/buy", body),
  njallaBuyCancel: () => api.del<Domains>("/api/njalla/buy"),
  njallaCredentials: () =>
    api.get<{ email: string; password: string | null }>("/api/dns-providers/njalla/credentials"),
  addGateway: (
    body:
      | { provider: "manual"; ipv4: string; mode: "dedicated" | "shared"; label?: string }
      | {
          provider: "lnvps";
          max_sats?: number;
          cap?: { currency: string; amount: number };
          auto_renew: boolean;
          renew_max_sats?: number;
          label?: string;
        },
  ) => api.post<Domains>("/api/gateways", body),
  updateGateway: (
    id: number,
    body: {
      label?: string;
      ipv4?: string;
      mode?: string;
      auto_renew?: boolean;
      renew_max_sats?: number;
      cap?: { currency: string; amount: number } | null;
    },
  ) => api.patch<Domains>(`/api/gateways/${id}`, body),
  removeGateway: (id: number) => api.del<Domains>(`/api/gateways/${id}`),
  setupScript: (id: number) => fetchText(`/api/gateways/${id}/setup`),
  lnvpsQuote: () => api.get<LnvpsQuote>("/api/gateways/lnvps/quote"),
  lnvpsEmail: (email: string) => api.put<Domains>("/api/gateways/lnvps/email", { email }),
};

// ---------------------------------------------------------------- services (SPEC §12.3–§12.6)

export interface PublicName {
  status: {
    errors: string[];
    synced_at: number | null;
  };
  /** Present on GET: twelve.cash works only on mainnet. */
  twelve_cash_available?: boolean;
  twelve_cash: TwelveCashEntry[];
  /** Present on GET: clank.money works only on mainnet. */
  clank_available?: boolean;
  clank: ClankEntry[];
  /** Present on GET: ZEUS Pay works only on mainnet. */
  zeus_available?: boolean;
  /** Present on GET: LNURL address services (Alby) work only on mainnet. */
  alby_available?: boolean;
  /** Main's address on an NWC LNURL service (Alby), set up on its website. */
  /** Addresses on an NWC LNURL service (Alby), set up on its website: one per Alby
   * account, each paying into Main or an account. */
  alby: AlbyEntry[];
  /** Answers to the "add an address" questions. */
  prefs: AddressPrefs;
  /** Main's ZEUS Pay LNURL address (one per node). */
  zeus: ZeusEntry | null;
}

export interface AddressPrefs {
  wallets: "any" | "modern" | "both" | null;
  domain: "yes" | "buy" | "no" | null;
  site: "home" | "server" | null;
}

export interface AlbyEntry {
  /** "main" or the account id. */
  target: string;
  address: string | null;
  connection_id: number;
  verified: boolean;
  created_at: number;
  error: string | null;
}

export interface ZeusEntry {
  /** "main" or the account id it pays into. */
  target: string;
  address: string;
  handle: string;
  domain: string;
  connection_id: number;
  /** Its LNURL endpoint answered. */
  verified: boolean;
  created_at: number;
  /** ZEUS Pay+ (choose the name and domain). */
  plus_active: boolean;
  plus_expires_at: number | null;
  plus_fee_sats: number | null;
  auto_renew: boolean;
  renew_max_sats: number | null;
  plus_error: string | null;
}

export interface ClankEntry {
  /** "main" or the account id. */
  target: string;
  address: string;
  offer_id: string;
  state: "paying" | "paid" | "registered";
  price_sats: number;
  verified: boolean;
  active: boolean;
  /** Last registration error while not registered. */
  error: string | null;
  created_at: number;
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
  /** A chosen name Allod is buying, until twelve.cash publishes it. */
  purchase?: { state: "paying" | "paid"; price_sats: number; error: string | null } | null;
}

export const PUBLIC_NAME_KEY = "/api/public-name";

export const publicName = {
  refresh: () => api.post<PublicName>("/api/public-name/refresh"),
  twelveAdd: (target: string, address?: string) =>
    api.post<PublicName>("/api/public-name/twelve-cash", { target, address }),
  twelveBuy: (body: { target: string; username: string; max_price_sats: number; pay_from?: string }) =>
    api.post<PublicName>("/api/public-name/twelve-cash/buy", body),
  twelveOffer: (target: string, address: string) =>
    api.get<{ offer: string }>(
      `/api/public-name/twelve-cash/offer?target=${encodeURIComponent(target)}&address=${encodeURIComponent(address)}`,
    ),
  clankAvailable: (username: string) =>
    api.get<{ valid: boolean; available: boolean | null }>(
      `/api/public-name/clank/available?username=${encodeURIComponent(username)}`,
    ),
  clankBuy: (body: { target: string; username: string; max_price_sats: number; pay_from?: string }) =>
    api.post<PublicName>("/api/public-name/clank", body),
  /** The payment proof for a name whose invoice was paid from another wallet. */
  clankProof: (body: { target: string; preimage: string }) =>
    api.post<PublicName>("/api/public-name/clank/proof", body),
  zeusAdd: (target: string, handle?: string, domain?: string) =>
    api.post<PublicName>("/api/public-name/zeus", { handle, domain, target }),
  zeusTarget: (target: string) => api.put<PublicName>("/api/public-name/zeus/target", { target }),
  zeusRename: (handle: string, domain: string) => api.put<PublicName>("/api/public-name/zeus", { handle, domain }),
  zeusBuyPlus: (maxPriceSats: number) => api.post<PublicName>("/api/public-name/zeus/plus", { max_price_sats: maxPriceSats }),
  zeusAutoRenew: (autoRenew: boolean, maxPriceSats?: number) =>
    api.put<PublicName>("/api/public-name/zeus/plus", { auto_renew: autoRenew, max_price_sats: maxPriceSats }),
  zeusRefresh: () => api.post<PublicName>("/api/public-name/zeus/refresh"),
  zeusRemove: (force = false) => api.del<PublicName>(`/api/public-name/zeus${force ? "?force=true" : ""}`),
  setPrefs: (p: Partial<AddressPrefs>) => api.put<PublicName>("/api/public-name/prefs", p),
  resetPrefs: () => api.del<PublicName>("/api/public-name/prefs"),
  /** A new receive-only connection for `target`, or a new key for `connectionId`. */
  albyConnection: (target: string, connectionId?: number) =>
    api.post<{ uri: string; connection_id: number }>("/api/public-name/alby/connection", {
      target,
      connection_id: connectionId,
    }),
  albyLink: (connectionId: number, address: string) =>
    api.post<PublicName>("/api/public-name/alby", { connection_id: connectionId, address }),
  albyRemove: (connectionId: number) => api.del<PublicName>(`/api/public-name/alby/${connectionId}`),
  checkNames: () => api.post<PublicName>("/api/public-name/names/check"),
  clankForget: (target: string) => api.del<PublicName>(`/api/public-name/clank/${encodeURIComponent(target)}`),
  twelveForget: (target: string) => api.del<PublicName>(`/api/public-name/twelve-cash/${encodeURIComponent(target)}`),
};
