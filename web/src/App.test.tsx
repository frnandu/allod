import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { SWRConfig } from "swr";
import { finalizeEvent, generateSecretKey, getPublicKey, nip19, verifyEvent, type NostrEvent } from "nostr-tools";
import { routes } from "./App";
import { _test, nip98Header } from "./lib/nip98";
import { _resetProgress } from "./lib/eta";
import { clearSession, setSession } from "./lib/session";
import { versionLabel } from "./screens/settings/SettingsLayout";

const sk = generateSecretKey();
const pk = getPublicKey(sk);
const PHRASE = Array.from({ length: 12 }, (_, i) => `word${i + 1}`).join(" ");

const info = (setup_completed: boolean) => ({
  version: "0.1.0",
  setup_completed,
  running: true,
  backend_type: "ldk-server",
  network: "regtest",
  deployment: { mode: "managed", platform: "docker" },
});

function mockApi(handlers: Record<string, (init?: RequestInit) => unknown>) {
  const fetchMock = vi.fn(async (url: string, init?: RequestInit) => {
    const path = new URL(url, "http://localhost").pathname;
    const h = handlers[path];
    if (!h && path === "/api/setup/claim") return new Response(null, { status: 204 });
    if (!h) return new Response(JSON.stringify({ error: { code: "NOT_FOUND", message: path } }), { status: 404 });
    const value = h(init);
    return value instanceof Response ? value : new Response(JSON.stringify(value), { status: 200 });
  });
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function renderAt(path: string) {
  render(
    <SWRConfig value={{ provider: () => new Map(), dedupingInterval: 0 }}>
      <RouterProvider router={createMemoryRouter(routes, { initialEntries: [path] })} />
    </SWRConfig>,
  );
}

/** The Node page's cards, open (they start closed for a first-time viewer). */
const NODE_CARDS = ["node.allod", "node.lightning", "node.bitcoin"];

beforeEach(() => {
  clearSession();
  localStorage.clear();
  for (const id of NODE_CARDS) localStorage.setItem(`allod.card.${id}`, "open");
  window.history.replaceState(null, "", "/");
  window.nostr = {
    getPublicKey: async () => pk,
    signEvent: async (t) => finalizeEvent(t, sk),
  };
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  delete window.nostr;
});

describe("gate", () => {
  it("shows the setup wizard before setup", async () => {
    mockApi({ "/api/info": () => info(false) });
    renderAt("/");
    expect(await screen.findByText("Set up your node")).toBeTruthy();
    expect(screen.getByText("Allod trusts the machine it runs on")).toBeTruthy();
  });

  it("requires host setup link and exchanges fragment without leaving it in URL", async () => {
    mockApi({
      "/api/info": () => info(false),
      "/api/setup/claim": () =>
        new Response(JSON.stringify({ error: { code: "UNAUTHORIZED", message: "setup link required" } }), {
          status: 401,
        }),
    });
    renderAt("/");
    expect(await screen.findByText("Open your private setup link")).toBeTruthy();
    expect(screen.getByText(/allod setup-link --base-url/)).toBeTruthy();

    cleanup();
    let claimed = "";
    window.location.hash = `setup=${"ab".repeat(32)}`;
    mockApi({
      "/api/info": () => info(false),
      "/api/setup/claim": (init) => {
        claimed = String(init?.body ?? "");
        return null;
      },
    });
    renderAt("/");
    expect(await screen.findByText("Set up your node")).toBeTruthy();
    expect(JSON.parse(claimed)).toEqual({ token: "ab".repeat(32) });
    expect(window.location.hash).toBe("");
  });

  it("asks to sign in once set up", async () => {
    mockApi({ "/api/info": () => info(true) });
    renderAt("/");
    expect(await screen.findByRole("heading", { name: "Sign in" })).toBeTruthy();
    expect(screen.getByRole("link", { name: "Sidecar" }).getAttribute("href")).toBe("https://sidecar.top/");
    expect(screen.getByRole("link", { name: "Peridot" }).getAttribute("href")).toBe(
      "https://github.com/nogringo/peridot",
    );
    expect(screen.queryByRole("link", { name: "nsec.app" })).toBeNull();
  });

  it("signs in directly after a signer is chosen", async () => {
    let challenge = "";
    let loginBody = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/auth/challenge": () => ({ challenge: "challenge-1" }),
      "/api/auth/login": (init) => {
        challenge = String((init?.headers as Record<string, string>)?.Authorization ?? "");
        loginBody = String(init?.body);
        return { token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk };
      },
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/setup/progress": () => ({ setup_completed: true, mode: "managed", seed_handoff: "done", credentials: { state: "done" } }),
      "/api/balances": () => ({
        lightning: { total_msat: 0, spendable_msat: 0, receivable_msat: 0 },
        onchain: { total_sat: 0, spendable_sat: 0, reserved_sat: 0, pending_from_closures_sat: 0 },
        accounts_total_msat: 0,
        main_spendable_msat: 0,
        in_flight_msat: 0,
      }),
      "/api/transactions": () => ({ items: [] }),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/auth/logout": () => null,
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Use extension"));
    expect(await screen.findByRole("heading", { name: "Main" })).toBeTruthy();
    expect(JSON.parse(loginBody)).toEqual({ challenge: "challenge-1" });
    expect(challenge.startsWith("Nostr ")).toBe(true);
    expect(screen.queryByText("Sign in")).toBeNull();

    // Signing out must stick: the restored extension signer must not log straight back in.
    fireEvent.click(screen.getByRole("button", { name: "Sign out" }));
    expect(await screen.findByText("Use extension")).toBeTruthy();
    expect(screen.queryByRole("heading", { name: "Main" })).toBeNull();
    fireEvent.click(screen.getByText("Use extension"));
    expect(await screen.findByRole("heading", { name: "Main" })).toBeTruthy();
  });

  it("shows the node once signed in", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({
        status: "ok",
        db: { ok: true },
        ldk_server: { ok: true },
        node: { pubkey: "02ab".repeat(16), block_height: 321, block_hash: "00" },
      }),
      "/api/setup/progress": () => ({ setup_completed: true, mode: "managed", seed_handoff: "done", credentials: { state: "done" } }),
      "/api/balances": () => ({
        lightning: { total_msat: 5_000_000, spendable_msat: 4_000_000, receivable_msat: 1_000_000 },
        onchain: { total_sat: 0, spendable_sat: 0, reserved_sat: 0, pending_from_closures_sat: 0 },
        accounts_total_msat: 0,
        main_spendable_msat: 4_321_000,
        in_flight_msat: 0,
      }),
      "/api/transactions": () => ({ items: [] }),
      "/api/settings": () => ({ fiat_currency: null }),
      "/api/profiles": () => [{ pubkey: pk, name: "Satoshi", picture: "https://example.com/s.png", targets: [] }],
    });
    renderAt("/");
    // The owner's Nostr name and picture, not the npub.
    expect(await screen.findByText("Satoshi")).toBeTruthy();
    expect(document.querySelector('img[src="https://example.com/s.png"]')).toBeTruthy();
    expect(await screen.findByText("Node running")).toBeTruthy();
    expect(await screen.findByText(/4,321/)).toBeTruthy();
    expect(screen.getByRole("link", { name: /Send/ })).toBeTruthy();
    expect(screen.getAllByText("Accounts").length).toBeGreaterThan(0);
  });
});

describe("fee estimates", () => {
  const node = {
    pubkey: "02ab".repeat(16),
    alias: "allod",
    network: "bitcoin",
    block_height: 973_789,
    block_hash: "00",
    version: "0.1.0",
    lightning_synced_at: 1,
    listening_addresses: [],
    announcement_addresses: [],
    node_uris: [],
    capabilities: {},
  };
  const target = (blocks: number, sat_per_vb: number | null) => ({ blocks, sat_per_vb });

  it("shows the rates and warns while targets are missing", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": () => ({
        source: "bitcoind",
        available: false,
        targets: [target(2, 1.5), target(6, null), target(12, null), target(144, null), target(1008, null)],
        missing: [6, 12, 144, 1008],
        error: null,
      }),
    });
    renderAt("/node");
    expect(await screen.findByText("Fee estimates")).toBeTruthy();
    expect(await screen.findByText("1.5")).toBeTruthy();
    expect(screen.getByText("Fee estimates are still missing")).toBeTruthy();
    expect(screen.getAllByText("no estimate")).toHaveLength(4);
  });

  it("says nothing about fees without a bundled bitcoind", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": () => ({ source: null, available: false, targets: [], missing: [] }),
      "/api/node/bitcoind": () => ({ source: null }),
    });
    renderAt("/node");
    expect(await screen.findByText("allod")).toBeTruthy();
    expect(screen.queryByText("Fee estimates")).toBeNull();
    expect(screen.queryByText("Bitcoin node")).toBeNull();
  });

  const bitcoind = (extra: Record<string, unknown> = {}) => ({
    source: "bitcoind",
    updated_at: Math.floor(Date.now() / 1000),
    version: 310100,
    subversion: "/Satoshi:31.1.0/",
    chain: "main",
    blocks: 935_400,
    headers: 935_400,
    best_block_time: Math.floor(Date.now() / 1000) - 300,
    verification_progress: 0.99999,
    initial_block_download: false,
    size_on_disk: 12 * 1024 ** 3,
    pruned: true,
    prune_height: 930_000,
    uptime: 7200,
    peers: { total: 10, inbound: 0, outbound: 10, network_active: true, reachable: ["ipv4"] },
    mempool: { loaded: true, transactions: 1200, vbytes: 500_000, usage: 3 * 1024 ** 2, max: 300 * 1024 ** 2, min_fee_sat_vb: 1, total_fee_sat: 1000 },
    relay_fee_sat_vb: 1,
    background_validation: null,
    warnings: [],
    errors: {},
    ...extra,
  });
  const allFees = () => ({
    source: "bitcoind",
    available: true,
    targets: [target(2, 12), target(6, 8), target(12, 5), target(144, 2), target(1008, 1)],
    missing: [],
    error: null,
  });

  it("shows Allod's own layer on top of the node page", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true, detail: "sqlite" }, ldk_server: { ok: true } }),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () => ({ source: null }),
      "/api/apps": () => [
        { id: 1, name: "Damus", last_used_at: now - 600, expires_at: null },
        { id: 2, name: "Old", last_used_at: null, expires_at: now - 10 },
      ],
      "/api/accounts/summary": () => ({ count: 3, total_balance_msat: 152_000_000 }),
      "/api/ledger/check": () => ({ ok: true, problems: [] }),
      "/api/domains": () => ({ domains: [{ id: 1, name: "example.org", live: true, addresses: [{}, {}] }] }),
      "/api/nip05": () => ({ names: [{}] }),
      // Third-party names count too, not only addresses on own domains.
      "/api/public-name": () => ({
        status: { errors: [], synced_at: now },
        twelve_cash: [],
        clank: [],
        alby: [],
        zeus: { address: "me@zeuspay.com", target: "main", verified: false },
      }),
      "/api/prism/failures": () => ({ failed_count: 2, failed_msat: 21_000_000, addresses: [] }),
      "/api/backup": () => ({ verified: false }),
      "/api/allod/status": () => ({
        relays: {
          configured: ["wss://relay.one", "wss://relay.two"],
          states: {
            updated_at: now,
            relays: [
              { url: "wss://relay.one", connected: true, status: "connected", connected_at: now - 3600 },
              { url: "wss://relay.two", connected: false, status: "disconnected", connected_at: null },
            ],
          },
        },
        activity: {
          since: now - 86400,
          received: { count: 3, msat: 21_000_000, fee_msat: 0 },
          sent: { count: 2, msat: 5_000_000, fee_msat: 4_000 },
          failed_sends: 1,
        },
        held_invoices: { count: 1, msat: 30_000_000, next_deadline: 977_100 },
        push: { devices: 3, failing: 0, last_success_at: now - 7200, via_connections: 1 },
        database: { backend: "sqlite", size_bytes: 12_900_000, litestream: true },
      }),
    });
    renderAt("/node");
    expect(await screen.findByText("Allod")).toBeTruthy();
    expect(await screen.findByText("1/2 connected")).toBeTruthy();
    expect(screen.getByText("down: relay.two")).toBeTruthy();
    expect(screen.getByText("5 payments")).toBeTruthy();
    expect(screen.getByText(/1 failed/)).toBeTruthy(); // next to what went out
    expect(screen.getByText("3 devices")).toBeTruthy();
    expect(screen.getByText("last sent 2 h ago · 1 via a connection")).toBeTruthy();
    expect(screen.getByText("12.3 MiB")).toBeTruthy();
    expect(screen.getByText("SQLite · replicated by Litestream")).toBeTruthy();
    expect(screen.getByText("1 hold invoice waiting to be settled")).toBeTruthy();
    expect(screen.getByText(/before block 977,100/)).toBeTruthy();
    expect(await screen.findByText("1 used today · 1 expired")).toBeTruthy();
    expect(await screen.findByText("Balanced")).toBeTruthy();
    expect(screen.getByText("Allod 0.1.0 · Docker")).toBeTruthy();
    expect(await screen.findByText(/live · 1 Nostr name$/)).toBeTruthy();
    // Two on example.org and one on ZEUS Pay.
    const addresses = screen.getByText("Lightning addresses").closest("a")!;
    expect(within(addresses).getByText("3")).toBeTruthy();
    expect(await screen.findByText("2 prism payouts failed")).toBeTruthy();
    expect(await screen.findByText("Recovery phrase not checked")).toBeTruthy();
    // The Lightning node below it: its alias and ID in a row, no avatar.
    expect(screen.getByText("Lightning node")).toBeTruthy();
    expect(screen.getByText("Node ID")).toBeTruthy();
  });

  it("shows Bitcoin Core's status and background validation after a fast sync", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({
          background_validation: {
            active: true,
            complete: false,
            blocks: 599_915,
            target_height: 935_000,
            transactions: 466_098_142,
            target_transactions: 1_305_397_408,
            progress: 466_098_142 / 1_305_397_408,
            progress_basis: "transactions",
            tx_per_sec: 2_400,
            eta_secs: 5 * 3600 + 20 * 60,
          },
          warnings: ["Clock skew detected"],
        }),
    });
    renderAt("/node");
    expect(await screen.findByText("Bitcoin node")).toBeTruthy();
    expect(await screen.findByText("Background validation")).toBeTruthy();
    // By transactions (35.7%), not by blocks (64.2%).
    expect(screen.getByText("35.7%")).toBeTruthy();
    expect(screen.getByText("block 599,915 / 935,000")).toBeTruthy();
    expect(screen.getByText("466.1 M / 1,305.4 M transactions")).toBeTruthy();
    expect(screen.getByText("about 5 h 20 min left")).toBeTruthy();
    expect(screen.getByText(/2,400 transactions\/s/)).toBeTruthy();
    expect(screen.getByText("Validating history")).toBeTruthy();
    expect(screen.getByText("Bitcoin Core 31.1 · mainnet · pruned")).toBeTruthy();
    expect(screen.getByText("Clock skew detected")).toBeTruthy();
    expect(await screen.findByText("Fee estimates")).toBeTruthy();
    expect(screen.getByText("Mempool & blocks")).toBeTruthy();
    expect(screen.getByText("1,200 tx")).toBeTruthy();
    expect(screen.getByText("1,000 sat")).toBeTruthy();
    // The fee boxes start at the mempool's entry floor.
    expect(await screen.findByText("No priority")).toBeTruthy();
    expect(screen.getByText("High priority")).toBeTruthy();
    expect(screen.getByText("12")).toBeTruthy();
  });

  it("draws projected mempool blocks next to the mined ones", async () => {
    const now = Math.floor(Date.now() / 1000);
    setSession({ token: "t", expires_at: now + 3600, pubkey: pk });
    const mined = (height: number, ago: number) => ({
      height, hash: `h${height}`, time: now - ago, txs: 4120, size: 1_763_679, vsize: 997_944,
      fee_sat: 1_009_430, min_rate: 0, median_rate: 1, max_rate: 20, percentiles: [0, 0, 1, 1, 1],
    });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({
          mempool_blocks: [
            { vsize: 998_000, fee_sat: 868_344, min_rate: 0.12, median_rate: 1.21, max_rate: 30.2, count: 1 },
            { vsize: 999_000, fee_sat: 120_213, min_rate: 0.12, median_rate: 0.12, max_rate: 0.13, count: 1 },
            { vsize: 2_950_000, fee_sat: 315_000, min_rate: 0.1, median_rate: 0.1, max_rate: 0.11, count: 3 },
          ],
          recent_blocks: [mined(976_961, 300), mined(976_960, 900)],
        }),
    });
    renderAt("/node");
    expect(await screen.findByLabelText("Mempool blocks")).toBeTruthy();
    expect(screen.getByText("next block")).toBeTruthy();
    expect(screen.getByText("~1.2")).toBeTruthy();
    expect(screen.getByText("0.12 – 30")).toBeTruthy();
    expect(screen.getByText("+3 blocks")).toBeTruthy();
    expect(screen.getByText("976,961")).toBeTruthy();
    expect(screen.getByText("15 min ago")).toBeTruthy();
    expect(screen.getAllByText("4,120 tx")).toHaveLength(2);
  });

  it("flags a full mempool that evicts its cheapest transactions", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({
          mempool: { loaded: true, transactions: 90_000, vbytes: 40_000_000, usage: 300 * 1024 ** 2, max: 300 * 1024 ** 2, min_fee_sat_vb: 3.2, total_fee_sat: 5_000_000 },
        }),
    });
    renderAt("/node");
    expect(await screen.findByText("full · evicting cheapest")).toBeTruthy();
    expect(screen.getByText("≈ 40 blocks")).toBeTruthy();
    expect(screen.getAllByText("3.2 sat/vB").length).toBeGreaterThan(0);
  });

  it("says when Bitcoin Core is too busy to answer and takes the tip from the Lightning node", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({
          updated_at: now - 600,
          // Ten minutes without an answer; the server took ldk-server's newer tip (no time).
          best_block_time: null,
          blocks: 977_183,
          tip_source: "ldk-server",
        }),
    });
    renderAt("/node");
    expect(await screen.findByText("Bitcoin Core is busy")).toBeTruthy();
    expect(screen.getByText("Busy")).toBeTruthy();
    expect(screen.getByText("977,183")).toBeTruthy();
    expect(screen.getByText("from the Lightning node")).toBeTruthy();
    expect(screen.queryByText("Tip behind")).toBeNull();
  });

  it("shows the live tip from bitcoind's log while its chain figures are old", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      // Twenty minutes without a chain answer, but the log saw a newer block 30 s ago.
      "/api/node/bitcoind": () =>
        bitcoind({
          updated_at: now - 1200,
          blocks: 977_352,
          best_block_hash: null,
          best_block_time: null,
          tip_source: "log",
          tip_live: true,
          tip_seen_at: now - 30,
        }),
    });
    renderAt("/node");
    expect(await screen.findByText("977,352")).toBeTruthy();
    expect(screen.getByText("live from bitcoind's log")).toBeTruthy();
    expect(screen.getByText("just now")).toBeTruthy();
    expect(screen.queryByText("Busy")).toBeNull();
    expect(screen.queryByText("Bitcoin Core is busy")).toBeNull();
    expect(screen.getByText(/figures from 20 min ago/)).toBeTruthy();
  });

  it("keeps Bitcoin Core's badge calm when its figures are a few minutes old but current", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      // Five minutes old, and ldk-server knows no newer block.
      "/api/node/bitcoind": () => bitcoind({ updated_at: now - 300 }),
    });
    renderAt("/node");
    expect(await screen.findByText(/figures from 5 min ago/)).toBeTruthy();
    expect(screen.queryByText("Busy")).toBeNull();
    expect(screen.queryByText("Bitcoin Core is busy")).toBeNull();
  });

  it("shows Bitcoin Core as busy, not down, before its first chain answer", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      // Allod just started while a UTXO scan holds bitcoind's chain lock: only the tip
      // from ldk-server and the mempool so far.
      "/api/node/bitcoind": () => ({
        source: "bitcoind",
        chain_pending: true,
        blocks: 977_198,
        best_block_hash: "00ab",
        best_block_time: null,
        tip_source: "ldk-server",
        mempool: { transactions: 5 },
      }),
    });
    renderAt("/node");
    expect(await screen.findByText("Bitcoin Core is busy")).toBeTruthy();
    expect(screen.getByText(/has not reported its chain figures yet/)).toBeTruthy();
    expect(screen.getByText("Busy")).toBeTruthy();
    expect(screen.getByText("977,198")).toBeTruthy();
    expect(screen.queryByText("Bitcoin Core does not answer")).toBeNull();
    expect(screen.queryByText(/Syncing/)).toBeNull();
  });

  it("takes Bitcoin Core's state from its log before its first chain answer", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      // Background validation holds bitcoind's chain lock right after a restart; its log
      // still gives the tip and the sync state.
      "/api/node/bitcoind": () => ({
        source: "bitcoind",
        chain_pending: true,
        tip_live: true,
        tip_source: "log",
        tip_seen_at: now - 20,
        blocks: 977_512,
        best_block_hash: null,
        best_block_time: now - 600,
        initial_block_download: false,
        verification_progress: 0.99999,
        background_validation: { active: true, complete: false, blocks: 642_000 },
        peers: { total: 8 },
        uptime: 600,
        mempool: { transactions: 5 },
      }),
    });
    renderAt("/node");
    expect(await screen.findByText("Validating history")).toBeTruthy();
    expect(screen.getByText("977,512")).toBeTruthy();
    expect(screen.getByText("live from bitcoind's log")).toBeTruthy();
    expect(screen.getByText("10 min ago")).toBeTruthy();
    expect(screen.getByText(/other figures once bitcoind answers/)).toBeTruthy();
    expect(screen.queryByText("Busy")).toBeNull();
    expect(screen.queryByText("Bitcoin Core is busy")).toBeNull();
  });

  it("folds the Node page's cards to a brief, and remembers each one", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    for (const id of NODE_CARDS) localStorage.removeItem(`allod.card.${id}`);
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({ peers: { total: 0, inbound: 0, outbound: 0, network_active: true, reachable: [] } }),
    });
    renderAt("/node");
    const bitcoin = await screen.findByRole("button", { name: "Bitcoin node" });
    expect(bitcoin.getAttribute("aria-expanded")).toBe("false");
    // Closed: the badge and a few figures, not the details.
    expect(await screen.findByText("No peers")).toBeTruthy();
    expect(screen.getByText("Last block")).toBeTruthy();
    expect(screen.queryByText("Bitcoin Core has no peers")).toBeNull();
    expect(screen.getByRole("button", { name: "Lightning node" }).getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(bitcoin);
    expect(await screen.findByText("Bitcoin Core has no peers")).toBeTruthy();
    expect(localStorage.getItem("allod.card.node.bitcoin")).toBe("open");
    // A reload keeps it open; the others stay closed.
    cleanup();
    renderAt("/node");
    expect(await screen.findByText("Bitcoin Core has no peers")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Allod" }).getAttribute("aria-expanded")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "Bitcoin node" }));
    expect(localStorage.getItem("allod.card.node.bitcoin")).toBe("closed");
  });

  it("sums up the closed cards: the Lightning node's state, Bitcoin's fees, no versions", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    for (const id of NODE_CARDS) localStorage.removeItem(`allod.card.${id}`);
    mockApi({
      "/api/info": () => info(true),
      // Two blocks under bitcoind's tip.
      "/api/node": () => ({ ...node, block_height: 935_398 }),
      "/api/fees": allFees,
      "/api/node/bitcoind": () => bitcoind(),
      "/api/peers": () => [
        { node_id: "02aa", address: "a:9735", is_persisted: true, is_connected: true },
        { node_id: "02bb", address: "b:9735", is_persisted: true, is_connected: false },
      ],
    });
    renderAt("/node");
    expect(await screen.findByText("2 blocks behind")).toBeTruthy();
    expect(screen.queryByText("Syncing")).toBeNull();
    // No channels: 0, not a dash.
    const channels = screen.getAllByText("Channels").find((e) => e.tagName === "DT")!;
    expect(within(channels.parentElement!).getByText("0")).toBeTruthy();
    // Lightning peers: connected / saved, closed and open.
    expect(await screen.findByText("1 / 2")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Lightning node" }));
    expect(await screen.findByText("connected / saved")).toBeTruthy();
    expect(screen.getByRole("link", { name: "1 / 2" }).getAttribute("href")).toBe("/node/peers");
    fireEvent.click(screen.getByRole("button", { name: "Lightning node" }));
    expect(await screen.findByText("Fee rate (sat/vB)")).toBeTruthy();
    // Cheapest first, as in the open card.
    const fees = screen.getByText("Fee rate (sat/vB)").parentElement!.textContent!;
    expect(fees.indexOf("Low")).toBeLessThan(fees.indexOf("Medium"));
    expect(fees.indexOf("Medium")).toBeLessThan(fees.indexOf("High"));
    // Figures link to where they are managed.
    const peers = screen.getAllByText("Peers").find((e) => e.tagName === "DT")!;
    expect(peers.closest("a")?.getAttribute("href")).toBe("/node/peers");
    expect(screen.queryByText(/Bitcoin Core 2\d/)).toBeNull();
    expect(screen.queryByLabelText("Software")).toBeNull();
  });

  it("reconnects an offline peer at its saved address", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const posts: unknown[] = [];
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/peers": (init) => {
        if (init?.method === "POST") {
          posts.push(JSON.parse(String(init.body)));
          return new Response(null, { status: 201 });
        }
        return [
          { node_id: "02aa", address: "a.example:9735", is_persisted: true, is_connected: true },
          { node_id: "02bb", address: "b.example:9735", is_persisted: true, is_connected: false },
        ];
      },
    });
    renderAt("/node/peers");
    // Only the offline one offers it.
    const button = await screen.findByRole("button", { name: "Reconnect" });
    expect(screen.getAllByRole("button", { name: "Reconnect" })).toHaveLength(1);
    fireEvent.click(button);
    await waitFor(() => expect(posts).toEqual([{ pubkey: "02bb", address: "b.example:9735", persist: true }]));
  });

  it("warns when Bitcoin Core has no peers or is outdated", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({ version: 270100, peers: { total: 0, inbound: 0, outbound: 0, network_active: true, reachable: [] } }),
    });
    renderAt("/node");
    expect(await screen.findByText("Bitcoin Core has no peers")).toBeTruthy();
    expect(screen.getByText("No peers")).toBeTruthy();
    expect(screen.getByText("Bitcoin Core 27.1 no longer gets security fixes")).toBeTruthy();
  });

  it("blames a starved bitcoind, not the network, for no peers during background validation", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": allFees,
      "/api/node/bitcoind": () =>
        bitcoind({
          peers: { total: 0, inbound: 0, outbound: 0, network_active: true, reachable: [] },
          background_validation: { active: true, complete: false, blocks: 646_000 },
        }),
    });
    renderAt("/node");
    expect(await screen.findByText("Bitcoin Core has no peers")).toBeTruthy();
    expect(screen.getByText(/peers time out and drop/)).toBeTruthy();
  });
});

describe("nip98", () => {
  it("hashes signed request bodies without Web Crypto", async () => {
    vi.stubGlobal("crypto", undefined);
    expect(await _test.sha256Hex("abc")).toBe("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
  });

  it("signs url, method and body hash", async () => {
    const signer = {
      kind: "extension" as const,
      label: "test",
      getPublicKey: async () => pk,
      signEvent: async (t: Parameters<typeof finalizeEvent>[0]) => finalizeEvent(t, sk),
    };
    const body = JSON.stringify({ challenge: "ab" });
    const header = await nip98Header(signer, "http://localhost/api/auth/login", "post", body);
    const event = JSON.parse(atob(header.replace("Nostr ", ""))) as NostrEvent;
    expect(verifyEvent(event)).toBe(true);
    expect(event.kind).toBe(27235);
    expect(event.tags).toContainEqual(["u", "http://localhost/api/auth/login"]);
    expect(event.tags).toContainEqual(["method", "POST"]);
    expect(event.tags).toContainEqual(["payload", await _test.sha256Hex(body)]);
  });
});

describe("sync progress", () => {
  it("shows a time-left estimate on the first response, from the server's history", async () => {
    _resetProgress();
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    const fetchMock = mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "degraded", db: { ok: true }, ldk_server: { ok: false } }),
      "/api/sync/fast": () => ({ available: false }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "done",
        credentials: { state: "done" },
        bitcoind: {
          blocks: 1060,
          headers: 2060,
          verification_progress: 0.5,
          initial_block_download: true,
          updated_at: now,
          // 0.01 per minute: 0.5 left is about 50 min.
          history: [
            [now - 60, 1000, 0.49],
            [now - 30, 1030, 0.495],
            [now, 1060, 0.5],
          ],
        },
      }),
    });
    renderAt("/");
    expect(await screen.findByText("about 50 min left")).toBeTruthy();
    expect(screen.queryByText("estimating…")).toBeNull();
    expect(screen.getByText(/syncing 50\.0% · 1,060 of 2,060/)).toBeTruthy();
    // Wallet routes answer 503 until the Lightning node runs: not asked meanwhile.
    const paths = fetchMock.mock.calls.map(([url]) => new URL(String(url), "http://localhost").pathname);
    expect(paths).not.toContain("/api/accounts");
    expect(paths).not.toContain("/api/events/ticket");
  });
});

describe("setup wizard", () => {
  it("creates a node and requires a backup check", async () => {
    let setupAuth = "";
    let fastSyncBody = "";
    let verifyBody = "";
    mockApi({
      "/api/backup/verify": (init) => {
        verifyBody = String(init?.body);
        return null;
      },
      "/api/sync/fast": (init) => {
        if (init?.method === "POST") {
          fastSyncBody = String(init.body);
          return { requested_url: "x" };
        }
        return { available: true, suggestion: { height: 935000, url: "https://mirror.example/utxo-935000.dat", source: "mirror.example" } };
      },
      "/api/info": () => info(false),
      "/api/fiat/currencies": () => ({ source: "yadio", host: "api.yadio.io", currencies: ["ARS", "EUR", "USD"] }),
      "/api/settings": () => ({ fiat_currency: "ARS", fiat_source: "yadio", fiat_source_host: "api.yadio.io" }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "waiting_for_ldk_server",
        credentials: { state: "waiting" },
        bitcoind: { initial_block_download: true, blocks: 10, headers: 900000, verification_progress: 0.01 },
      }),
      "/api/setup": (init) => {
        setupAuth = String((init?.headers as Record<string, string>)?.Authorization ?? "");
        return { token: "tok", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk, recovery_phrase: PHRASE };
      },
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Get started"));
    fireEvent.click(await screen.findByText("Use extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));

    expect(await screen.findByText("Write down your recovery phrase")).toBeTruthy();
    expect(setupAuth.startsWith("Nostr ")).toBe(true);
    expect(screen.getByText("word12")).toBeTruthy();

    const cont = screen.getByText("Continue").closest("button")!;
    expect(cont.disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox"));
    fireEvent.click(cont);

    expect(await screen.findByText("Check your backup")).toBeTruthy();
    const confirm = screen.getByText("Confirm backup").closest("button")!;
    expect(confirm.disabled).toBe(true);
    for (const label of screen.getAllByText(/^Word #\d+$/)) {
      const n = Number(label.textContent!.replace("Word #", ""));
      fireEvent.change(document.getElementById(`word-${n - 1}`)!, { target: { value: `word${n}` } });
    }
    await waitFor(() => expect(confirm.disabled).toBe(false));
    // Without ALLOD_SKIP_BACKUP_CHECK there is no way around the check.
    expect(screen.queryByText("Skip check")).toBeNull();
    fireEvent.click(confirm);
    // Optional fast sync: it is the default and shows the suggested snapshot, which
    // can be changed.
    expect(await screen.findByText("Initial sync")).toBeTruthy();
    expect(screen.getByText("Fast sync from a snapshot (default)")).toBeTruthy();
    const url = screen.getByLabelText("Snapshot URL") as HTMLInputElement;
    expect(url.value).toBe("https://mirror.example/utxo-935000.dat");
    fireEvent.change(url, { target: { value: "https://other.example/utxo-935000.dat" } });
    fireEvent.click(screen.getByText("Continue"));
    expect(await screen.findByText("Your node is set up")).toBeTruthy();
    expect(JSON.parse(fastSyncBody)).toEqual({ url: "https://other.example/utxo-935000.dat" });
    // The server checked the typed words.
    const words = JSON.parse(verifyBody).words as Record<string, string>;
    expect(Object.keys(words)).toHaveLength(3);
    for (const [i, w] of Object.entries(words)) expect(w).toBe(`word${Number(i) + 1}`);
  });

  it("skips the backup check only when the node allows it", async () => {
    let verified = false;
    mockApi({
      "/api/backup/verify": () => {
        verified = true;
        return null;
      },
      "/api/sync/fast": () => ({ available: false }),
      "/api/info": () => ({ ...info(false), network: "bitcoin", deployment: { mode: "managed", platform: "docker", skip_backup_check: true } }),
      "/api/setup/progress": () => ({ setup_completed: true, mode: "managed", seed_handoff: "done", credentials: { state: "done" } }),
      "/api/setup": () => ({ token: "tok", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk, recovery_phrase: PHRASE }),
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Get started"));
    fireEvent.click(await screen.findByText("Use extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByText("Continue"));
    fireEvent.click(await screen.findByText("Skip check"));
    expect(await screen.findByText("Your node is set up")).toBeTruthy();
    expect(verified).toBe(false);
  });

  it("still offers the sync choice after skipping the backup check", async () => {
    mockApi({
      "/api/sync/fast": () => ({ available: true, suggestion: { height: 935000, url: "https://mirror.example/utxo-935000.dat", source: "mirror.example" } }),
      "/api/info": () => ({ ...info(false), network: "bitcoin", deployment: { mode: "managed", platform: "startos", skip_backup_check: true } }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "waiting_for_ldk_server",
        credentials: { state: "waiting" },
        bitcoind: { initial_block_download: true, blocks: 10, headers: 900000, verification_progress: 0.01 },
      }),
      "/api/setup": () => ({ token: "tok", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk, recovery_phrase: PHRASE }),
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Get started"));
    fireEvent.click(await screen.findByText("Use extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByText("Continue"));
    fireEvent.click(await screen.findByText("Skip check"));
    expect(await screen.findByText("Initial sync")).toBeTruthy();
    fireEvent.click(screen.getByText("Continue"));
    expect(await screen.findByText("Your node is set up")).toBeTruthy();
  });

  it("asks about the initial sync even when the fast-sync check fails", async () => {
    mockApi({
      "/api/sync/fast": () =>
        new Response(JSON.stringify({ error: { code: "INTERNAL", message: "boom" } }), { status: 500 }),
      "/api/info": () => ({ ...info(false), network: "bitcoin", deployment: { mode: "managed", platform: "docker", skip_backup_check: true } }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "waiting_for_ldk_server",
        credentials: { state: "waiting" },
        bitcoind: { initial_block_download: true, blocks: 10, headers: 900000, verification_progress: 0.01 },
      }),
      "/api/setup": () => ({ token: "tok", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk, recovery_phrase: PHRASE }),
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Get started"));
    fireEvent.click(await screen.findByText("Use extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByText("Continue"));
    fireEvent.click(await screen.findByText("Skip check"));
    // A failed check is not an answer: the question stays, with a way to retry or go on.
    expect(await screen.findByText("Could not check whether fast sync is available")).toBeTruthy();
    expect(screen.queryByText("Your node is set up")).toBeNull();
    fireEvent.click(screen.getByText("Try again"));
    expect(await screen.findByText("Could not check whether fast sync is available")).toBeTruthy();
    fireEvent.click(screen.getByText("Continue"));
    expect(await screen.findByText("Your node is set up")).toBeTruthy();
  });

  it("changes the display currency in Settings (USD by default)", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let saved = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/fiat/currencies": () => ({ source: "yadio", host: "api.yadio.io", currencies: ["ARS", "EUR", "USD"] }),
      "/api/settings": (init) => {
        if (init?.method === "PATCH") {
          saved = String(init.body);
          return { fiat_currency: "ARS", fiat_source: "yadio", fiat_source_host: "api.yadio.io" };
        }
        return { fiat_currency: "USD", fiat_source: "yadio", fiat_source_host: "api.yadio.io" };
      },
    });
    renderAt("/settings");
    expect(await screen.findByRole("heading", { name: "Display currency" })).toBeTruthy();
    expect(screen.getByRole("navigation", { name: "Settings sections" })).toBeTruthy();
    // The running version sits next to the Settings title.
    expect(await screen.findByText("Allod v0.1.0")).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("option", { name: /US Dollar/ }).getAttribute("aria-selected")).toBe("true"));
    // The saved currency is stated and listed first, not only highlighted somewhere in the list.
    expect(screen.getByText("US Dollar (USD)")).toBeTruthy();
    expect(screen.getAllByRole("option")[0]!.textContent).toMatch(/US Dollar/);
    // Currencies come from the node (server-side rates), with names from Intl.
    fireEvent.change(screen.getByLabelText("Currency"), { target: { value: "peso" } });
    const peso = await screen.findByRole("option", { name: /Argentine Peso/ });
    expect(screen.queryByRole("option", { name: /Euro/ })).toBeNull();
    fireEvent.click(peso);
    fireEvent.click(screen.getByText("Save"));
    expect(await screen.findByText("Saved.")).toBeTruthy();
    expect(JSON.parse(saved)).toEqual({ fiat_currency: "ARS", fiat_source: "yadio" });
  });

  it("switches the chain source in Settings > Bitcoin", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const bodies: unknown[] = [];
    const presets = [
      { id: "alby-esplora", name: "Alby", kind: "esplora", url: "https://electrs.getalbypro.com" },
      { id: "mempool-esplora", name: "mempool.space", kind: "esplora", url: "https://mempool.space/api" },
    ];
    const base = { default_mode: "bundled", network: "bitcoin", editable: true, presets };
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/settings/chain-source": (init) => {
        if (init?.method === "PUT") {
          const body = JSON.parse(String(init.body));
          bodies.push(body);
          const view = { ...body };
          delete view.rpc_password;
          return { ...base, active: { mode: "bundled", kind: "bitcoind" }, requested: view, restart_pending: true };
        }
        return { ...base, active: { mode: "bundled", kind: "bitcoind" }, requested: null, restart_pending: false };
      },
    });
    renderAt("/settings/bitcoin");
    expect(await screen.findByRole("heading", { name: "Chain source" })).toBeTruthy();
    expect(screen.getByText(/Bitcoin Core · this stack/)).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Bitcoin Core in this stack/ }).getAttribute("aria-checked")).toBe("true");
    const use = screen.getByText("Use this source") as HTMLButtonElement;
    expect(use.disabled).toBe(true);

    // From the bundled node to Alby's Esplora.
    fireEvent.click(screen.getByRole("radio", { name: /Alby/ }));
    fireEvent.click(use);
    expect(await screen.findByText("Restarting the Lightning node")).toBeTruthy();
    expect(bodies[0]).toEqual({ kind: "esplora", url: "https://electrs.getalbypro.com" });

    // An external Bitcoin Core needs address, user and password.
    fireEvent.click(screen.getByRole("radio", { name: /Your own Bitcoin Core node/ }));
    fireEvent.change(screen.getByLabelText("RPC address"), { target: { value: "10.0.0.5:8332" } });
    fireEvent.change(screen.getByLabelText("RPC user"), { target: { value: "ldk" } });
    expect(use.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("RPC password"), { target: { value: "pw" } });
    fireEvent.click(use);
    await waitFor(() => expect(bodies).toHaveLength(2));
    expect(bodies[1]).toEqual({ kind: "bitcoind", rpc_address: "10.0.0.5:8332", rpc_user: "ldk", rpc_password: "pw" });
  });

  describe("Lightning connectivity", () => {
    const PUB = "02ab".repeat(16) + "02";
    const ONION = "pg6mmjiyjmcrsslvykfwnntlaru7p5svn6y2ymmju6nubxndf4pscryd.onion:9735";
    const gateway = { id: 3, label: "Dublin", provider: "manual", ipv4: "203.0.113.10", mode: "dedicated", state: "active" };
    const reachable = (address: string) => ({ state: "reachable", detail: `peers can connect at ${address}`, checked_at: 1, address });
    const unset = {
      available: true,
      tor: false,
      clearnet: "none",
      gateway_id: null,
      port: null,
      host: null,
      host_port: null,
      default_host_port: 9735,
      addresses: [],
      address: null,
      onion_address: null,
      node_port: 9735,
      managed: true,
      tunnel: true,
      tor_available: true,
      tor_checked: true,
      reach: { clearnet: null, tor: null },
      announced: null,
      node: { pubkey: PUB, alias: "allod", announcement_addresses: [] },
      gateways: [gateway],
    };
    const both = {
      ...unset,
      tor: true,
      clearnet: "gateway",
      gateway_id: 3,
      port: 19735,
      addresses: ["203.0.113.10:19735", ONION],
      address: "203.0.113.10:19735",
      onion_address: ONION,
      announced: true,
      reach: { clearnet: reachable("203.0.113.10:19735"), tor: reachable(ONION) },
      node: { ...unset.node, announcement_addresses: ["203.0.113.10:19735", ONION] },
    };
    const session = () => setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });

    it("starts private, and opens the gateway's settings inside its choice", async () => {
      session();
      mockApi({
        "/api/info": () => info(true),
        "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
        "/api/settings/lightning": () => ({ ...unset, gateways: [] }),
      });
      renderAt("/settings/lightning");
      expect(await screen.findByRole("tab", { name: "Connectivity" })).toBeTruthy();
      const tor = screen.getByRole("switch", { name: "Reachable over Tor" });
      expect(tor.getAttribute("aria-checked")).toBe("false");
      expect((screen.getByLabelText("Not over the internet") as HTMLInputElement).checked).toBe(true);
      expect(screen.getByText("Current")).toBeTruthy();
      expect(screen.getAllByText("Private").length).toBeGreaterThan(0);
      expect(screen.queryByText(/stack/i)).toBeNull();
      expect(screen.queryByText("No gateway yet")).toBeNull();
      fireEvent.click(screen.getByLabelText("Through a gateway"));
      const card = screen.getByLabelText("Through a gateway").closest("div")!;
      expect(within(card).getByText("No gateway yet")).toBeTruthy();
      expect(within(card).getByRole("link", { name: /Add a gateway in Settings/ }).getAttribute("href")).toBe("/settings/gateways");
      expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
      fireEvent.click(screen.getByRole("button", { name: "Discard" }));
      expect((screen.getByLabelText("Not over the internet") as HTMLInputElement).checked).toBe(true);
    });

    it("greys out what this device cannot do", async () => {
      session();
      mockApi({
        "/api/info": () => info(true),
        "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
        "/api/settings/lightning": () => ({ ...unset, tunnel: false, tor_available: false }),
      });
      renderAt("/settings/lightning");
      expect(((await screen.findByRole("switch", { name: "Reachable over Tor" })) as HTMLButtonElement).disabled).toBe(true);
      expect((screen.getByLabelText("Through a gateway") as HTMLInputElement).disabled).toBe(true);
      expect((screen.getByLabelText("At my home IP") as HTMLInputElement).disabled).toBe(false);
      expect(screen.getAllByText("Not available on this device yet.")).toHaveLength(2);
    });

    it("sends Lightning through a gateway and Tor together", async () => {
      session();
      const bodies: unknown[] = [];
      mockApi({
        "/api/info": () => info(true),
        "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
        "/api/settings/lightning": (init) => {
          if (init?.method === "PUT") {
            bodies.push(JSON.parse(String(init.body)));
            return both;
          }
          return unset;
        },
      });
      renderAt("/settings/lightning");
      fireEvent.click(await screen.findByLabelText("Through a gateway"));
      fireEvent.change(screen.getByLabelText("Port"), { target: { value: "19735" } });
      fireEvent.click(screen.getByRole("switch", { name: "Reachable over Tor" }));
      expect(screen.getByText("After saving")).toBeTruthy();
      expect(screen.getByText(/Saving restarts the Lightning node/)).toBeTruthy();
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
      expect(await screen.findByText(`${PUB}@203.0.113.10:19735`)).toBeTruthy();
      expect(bodies[0]).toEqual({ clearnet: "gateway", tor: true, gateway_id: 3, port: 19735 });
      expect(screen.getByText(`${PUB}@${ONION}`)).toBeTruthy();
      expect(screen.getByText("Saved")).toBeTruthy();
      expect(screen.getAllByText("Tor and a gateway").length).toBeGreaterThan(0);
      expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(true);
    });

    it("turns on Tor only, then adds the home IP", async () => {
      session();
      const bodies: unknown[] = [];
      mockApi({
        "/api/info": () => info(true),
        "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
        "/api/settings/lightning": (init) => {
          if (init?.method === "PUT") {
            const b = JSON.parse(String(init.body));
            bodies.push(b);
            return b.clearnet === "host"
              ? { ...unset, tor: true, onion_address: ONION, clearnet: "host", host: b.host, host_port: b.host_port, addresses: [`${b.host}:${b.host_port}`, ONION], address: `${b.host}:${b.host_port}` }
              : { ...unset, tor: true, addresses: [ONION], onion_address: ONION };
          }
          return unset;
        },
        "/api/settings/lightning/detect-ip": () => ({ ip: "198.51.100.7", source: "Cloudflare (1.1.1.1)" }),
      });
      renderAt("/settings/lightning");
      expect(((await screen.findByRole("button", { name: "Save" })) as HTMLButtonElement).disabled).toBe(true);
      fireEvent.click(screen.getByRole("switch", { name: "Reachable over Tor" }));
      expect(screen.getAllByText("Tor only").length).toBeGreaterThan(0);
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
      await waitFor(() => expect(bodies).toHaveLength(1));
      expect(bodies[0]).toEqual({ clearnet: "none", tor: true });
      expect((await screen.findAllByText(ONION)).length).toBeGreaterThan(0);
      fireEvent.click(screen.getByLabelText("At my home IP"));
      expect(screen.getByText(/stays on record/)).toBeTruthy();
      // Prefilled with the IP this machine's connections come from.
      await waitFor(() => expect((screen.getByLabelText("Public address") as HTMLInputElement).value).toBe("198.51.100.7"));
      expect(screen.getByText(/by asking Cloudflare \(1\.1\.1\.1\)/)).toBeTruthy();
      expect(screen.getByRole("button", { name: "Detect again" })).toBeTruthy();
      expect(screen.getByText(/Forward it to port 9735/)).toBeTruthy();
      fireEvent.click(screen.getByRole("button", { name: "Save" }));
      await waitFor(() => expect(bodies).toHaveLength(2));
      expect(bodies[1]).toEqual({ clearnet: "host", tor: true, host: "198.51.100.7", host_port: 9735 });
    });

    it("shows on the node page whether and how the node is reachable", async () => {
      session();
      let state: unknown = unset;
      let uris: string[] = [];
      mockApi({
        "/api/info": () => info(true),
        "/api/node": () => ({
          pubkey: PUB,
          alias: "allod",
          network: "bitcoin",
          block_height: 1,
          block_hash: "00",
          version: "0.1.0 (590cdd9923f566a9b8dbe1cd1f34f2b0476cb2ba)",
          components: {
            ldk_node: { version: "0.8.0+git", rev: "179b4b263762155a7ff515ddad7e757a80807fd5" },
            lightning: { version: "0.3.0-rc3", rev: "d7a980641c6325ddda5072a0f470ef8392676cf5" },
          },
          listening_addresses: [],
          announcement_addresses: [],
          node_uris: uris,
          capabilities: {},
        }),
        "/api/fees": () => ({ source: null, available: false, targets: [], missing: [] }),
        "/api/node/bitcoind": () => ({ source: null }),
        "/api/settings/lightning": () => state,
      });
      renderAt("/node");
      expect(await screen.findByText("Inbound connections")).toBeTruthy();
      expect(screen.getByText("Private")).toBeTruthy();
      // What the node runs, each linked to its commit.
      expect(screen.getByText("LDK Server 0.1.0 (590cdd9)").getAttribute("href")).toBe(
        "https://github.com/lightningdevkit/ldk-server/commit/590cdd9923f566a9b8dbe1cd1f34f2b0476cb2ba",
      );
      expect(screen.getByText("LDK Node 0.8.0+git (179b4b2)")).toBeTruthy();
      expect(screen.getByText("LDK 0.3.0-rc3 (d7a9806)").getAttribute("href")).toContain("/rust-lightning/commit/d7a98064");
      const link = screen.getByRole("link", { name: /Make your node reachable/ });
      expect(link.getAttribute("href")).toBe("/settings/lightning");
      cleanup();
      uris = [`${PUB}@203.0.113.10:19735`, `${PUB}@${ONION}`];
      state = {
        ...both,
        reach: { clearnet: { ...both.reach.clearnet, state: "unreachable", detail: "no answer from 203.0.113.10:19735" }, tor: both.reach.tor },
      };
      renderAt("/node");
      expect(await screen.findByText("Tor and a gateway")).toBeTruthy();
      expect(screen.getByText("no answer from 203.0.113.10:19735")).toBeTruthy();
      expect(screen.getAllByText("Gateway Dublin")).toHaveLength(2); // at a glance, and in the fold
      // Each path at a glance; its address (ready to copy) and check folded away.
      expect(screen.getAllByText("Tor")).toHaveLength(2);
      const fold = screen.getByText("Addresses and checks").closest("details")!;
      expect(fold.open).toBe(false);
      expect(within(fold).getByText(`${PUB}@${ONION}`)).toBeTruthy();
      expect(within(fold).getByText("no answer from 203.0.113.10:19735")).toBeTruthy();
      expect(screen.queryByText(/peers can connect at/)).toBeNull();
      expect(screen.getByRole("link", { name: /Connectivity settings/ })).toBeTruthy();
    });
  });

  it("joins a Wi-Fi network in Settings > Network on the appliance", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const joins: unknown[] = [];
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/system/network": () => ({
        available: true, hostname: "allod", country: "DE", hotspot: false, wifi_device: "wlan0", wifi: null,
        devices: [{ device: "eth0", type: "ethernet", state: "connected", connection: "Wired", ipv4: ["192.168.1.20/24"] }],
        saved: [],
      }),
      "/api/system/network/scan": () => ({
        networks: [
          { ssid: "HomeNet", signal: 80, security: "WPA2", in_use: false },
          { ssid: "Cafe", signal: 30, security: "", in_use: false },
        ],
      }),
      "/api/system/network/wifi": (init) => {
        joins.push(JSON.parse(String(init?.body)));
        return { ok: true };
      },
    });
    renderAt("/settings/network");
    expect(await screen.findByRole("link", { name: /Network/ })).toBeTruthy();
    expect(await screen.findByText("192.168.1.20/24")).toBeTruthy();
    fireEvent.click(screen.getByText("Find networks"));
    expect(await screen.findByText("HomeNet")).toBeTruthy();
    fireEvent.click(within(screen.getByText("HomeNet").closest("li") as HTMLElement).getByText("Join"));
    const dialog = await screen.findByRole("dialog", { name: "Join HomeNet" });
    fireEvent.change(within(dialog).getByLabelText(/Password/), { target: { value: "correct horse" } });
    fireEvent.click(within(dialog).getByRole("button", { name: /Join/ }));
    await waitFor(() => expect(joins).toEqual([{ ssid: "HomeNet", password: "correct horse", hidden: false }]));
  });

  it("hides Settings > Network outside the appliance", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/system/network": () => ({ available: false }),
    });
    renderAt("/settings");
    expect(await screen.findByRole("link", { name: /Bitcoin/ })).toBeTruthy();
    expect(screen.queryByRole("link", { name: /Network/ })).toBeNull();
  });

  it("manages notification devices in Settings > Notifications", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const calls: { method: string; path: string; body: unknown }[] = [];
    const device = {
      id: 7, label: "Firefox on Linux", app_id: null, app_name: null, endpoint_host: "updates.push.services.mozilla.com",
      events: ["payment_received", "payment_sent", "hold_invoice_accepted"], hide_amounts: false,
      created_at: 1, last_success_at: null, last_error: null, failures: 0,
    };
    const record = (path: string) => (init?: RequestInit) => {
      calls.push({ method: init?.method ?? "GET", path, body: init?.body ? JSON.parse(String(init.body)) : null });
      return path.endsWith("/test") ? { delivered: true, status: 201 } : device;
    };
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/push": () => ({
        vapid_public_key: "BAAA",
        events: device.events,
        subscriptions: [
          device,
          { ...device, id: 8, label: "", app_id: 3, app_name: "Zeus", endpoint_host: "ntfy.sh" },
        ],
      }),
      "/api/push/subscriptions/7": record("/api/push/subscriptions/7"),
      "/api/push/subscriptions/7/test": record("/api/push/subscriptions/7/test"),
    });
    renderAt("/settings/notifications");
    expect(await screen.findByText("Firefox on Linux")).toBeTruthy();
    expect(screen.getByText(/updates.push.services.mozilla.com/)).toBeTruthy();
    // A device a wallet app registered through its connection.
    expect(screen.getByText("Via connection: Zeus")).toBeTruthy();
    // jsdom has no push support: say so instead of offering the switch.
    expect(screen.queryByText("Turn on notifications")).toBeNull();

    const row = within(screen.getByLabelText("Firefox on Linux"));
    fireEvent.click(row.getByLabelText("Payments sent"));
    await waitFor(() => expect(calls).toHaveLength(1));
    expect(calls[0]).toEqual({
      method: "PATCH",
      path: "/api/push/subscriptions/7",
      body: { events: ["payment_received", "hold_invoice_accepted"] },
    });
    fireEvent.click(row.getByLabelText("Hide amounts and descriptions"));
    await waitFor(() => expect(calls).toHaveLength(2));
    expect(calls[1]?.body).toEqual({ hide_amounts: true });

    fireEvent.click(row.getByText("Send a test"));
    expect(await screen.findByText(/Test sent/)).toBeTruthy();
    fireEvent.click(row.getByText("Remove"));
    await waitFor(() => expect(calls.some((c) => c.method === "DELETE")).toBe(true));
  });

  it("turns on notifications for this browser", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const posts: unknown[] = [];
    let subs: unknown[] = [];
    const sub = {
      options: { applicationServerKey: null },
      toJSON: () => ({ endpoint: "https://fcm.googleapis.com/fcm/send/xyz", keys: { p256dh: "BPK", auth: "AUTH" } }),
      unsubscribe: vi.fn(async () => true),
    };
    const subscribe = vi.fn(async () => sub);
    const reg = { pushManager: { getSubscription: async () => null, subscribe } };
    vi.stubGlobal("isSecureContext", true);
    vi.stubGlobal("PushManager", class {});
    vi.stubGlobal("Notification", { permission: "default", requestPermission: async () => "granted" });
    Object.defineProperty(navigator, "serviceWorker", {
      configurable: true,
      value: { register: vi.fn(async () => reg), ready: Promise.resolve(reg), getRegistration: async () => undefined },
    });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/push": () => ({ vapid_public_key: "BAAA", events: [], subscriptions: subs }),
      "/api/push/subscriptions": (init) => {
        posts.push(JSON.parse(String(init?.body)));
        const view = { id: 3, label: "Chrome on Linux", endpoint_host: "fcm.googleapis.com", events: [], hide_amounts: false, created_at: 1, last_success_at: null, last_error: null, failures: 0 };
        subs = [view];
        return view;
      },
    });
    try {
      renderAt("/settings/notifications");
      fireEvent.click(await screen.findByText("Turn on notifications"));
      expect(await screen.findByText("This browser receives notifications.")).toBeTruthy();
      expect(subscribe).toHaveBeenCalledWith({ userVisibleOnly: true, applicationServerKey: expect.any(Uint8Array) });
      expect(posts[0]).toMatchObject({
        endpoint: "https://fcm.googleapis.com/fcm/send/xyz",
        keys: { p256dh: "BPK", auth: "AUTH" },
      });
      // The card title, and the badge on the registered device.
      expect(screen.getAllByText("This device")).toHaveLength(2);
    } finally {
      delete (navigator as { serviceWorker?: unknown }).serviceWorker;
    }
  });

  it("suggests the built-in node when the external Bitcoin node stops answering", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let started = false;
    const settings = () => ({
      default_mode: "external-bitcoind",
      network: "bitcoin",
      editable: true,
      presets: [],
      active: { mode: "external-bitcoind", kind: "bitcoind", rpc_address: started ? "allod_bitcoind_1:8332" : "10.21.21.8:8332" },
      requested: started ? { kind: "bitcoind", rpc_address: "allod_bitcoind_1:8332", rpc_user: "ldk", standby: true } : null,
      restart_pending: false,
      standby: started
        ? { enabled: true, in_use: true, outage_since: null, outage_error: null, suggested: false }
        : { enabled: false, in_use: false, outage_since: 1_800_000_000, outage_error: "connection refused", suggested: true },
    });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/settings/chain-source": () => settings(),
      "/api/settings/chain-source/standby": (init) => {
        expect(init?.method).toBe("POST");
        started = true;
        return settings();
      },
    });
    renderAt("/settings/bitcoin");
    expect(await screen.findByText("Your Bitcoin node is not answering")).toBeTruthy();
    expect(screen.getByText(/Last error: connection refused/)).toBeTruthy();
    expect(screen.getByRole("radio", { name: /Allod's built-in node/ })).toBeTruthy();
    fireEvent.click(screen.getByText("Start built-in node"));
    await waitFor(() => expect(screen.queryByText("Your Bitcoin node is not answering")).toBeNull());
    expect(started).toBe(true);
  });

  it("shows the chain source read-only when Allod does not manage ldk-server", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/settings/chain-source": () => ({
        default_mode: null, network: "bitcoin", editable: false, active: null, requested: null,
        restart_pending: false, presets: [],
      }),
    });
    renderAt("/settings/bitcoin");
    expect(await screen.findByText(/Allod does not manage this Lightning node/)).toBeTruthy();
    expect(screen.queryByText("Use this source")).toBeNull();
  });

  it("offers fast sync on Home while Bitcoin is still syncing", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let fast: Record<string, unknown> = {
      available: true,
      requested_url: null,
      status: null,
      suggestion: { height: 935000, url: "https://mirror.example/utxo-935000.dat", source: "mirror.example" },
    };
    let posted: unknown = null;
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "degraded", db: { ok: true }, ldk_server: { ok: false } }),
      "/api/settings": () => ({ fiat_currency: "none", fiat_source: "yadio", fiat_source_host: "api.yadio.io" }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "done",
        credentials: { state: "done" },
        bitcoind: { initial_block_download: true, blocks: 120, headers: 940000, verification_progress: 0.0001 },
      }),
      "/api/sync/fast": (init) => {
        if (init?.method === "POST") {
          posted = JSON.parse(String(init.body));
          fast = { ...fast, requested_url: (posted as { url: string }).url, status: { state: "downloading", bytes: 0, total: 9_390_000_000 } };
        }
        return fast;
      },
    });
    renderAt("/");
    fireEvent.click(await screen.findByText("Use fast sync"));
    expect(screen.getByText("Why fast sync is safe")).toBeTruthy();
    expect((screen.getByLabelText("Snapshot URL") as HTMLInputElement).value).toBe("https://mirror.example/utxo-935000.dat");
    fireEvent.click(screen.getByText("Start fast sync"));
    await waitFor(() => expect(screen.queryByText("Use fast sync")).toBeNull());
    expect(posted).toEqual({ url: "https://mirror.example/utxo-935000.dat" });
  });

  it("shows only the snapshot's progress until it is loaded", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let fast: Record<string, unknown> = {
      available: true,
      requested_url: "https://mirror.example/utxo-935000.dat",
      status: { state: "downloading", bytes: 939_000_000, total: 9_390_000_000 },
    };
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "degraded", db: { ok: true }, ldk_server: { ok: false } }),
      "/api/settings": () => ({ fiat_currency: "none", fiat_source: "yadio", fiat_source_host: "api.yadio.io" }),
      "/api/setup/progress": () => ({
        setup_completed: true,
        mode: "managed",
        seed_handoff: "waiting_for_ldk_server",
        credentials: { state: "waiting" },
        bitcoind: { initial_block_download: true, blocks: 120, headers: 940000, verification_progress: 0.0001 },
      }),
      "/api/sync/fast": () => fast,
    });
    renderAt("/");
    expect(await screen.findByText(/downloading 10%/)).toBeTruthy();
    expect(screen.queryByText(/syncing/)).toBeNull();
    expect(screen.getAllByRole("progressbar")[0]!.getAttribute("aria-valuenow")).toBe("10");
    cleanup();

    fast = {
      available: true,
      requested_url: "https://mirror.example/utxo-935000.dat",
      status: { state: "loading", base_height: 935000, phase: "coins", pct: 37.14, coins: 61000000 },
    };
    renderAt("/");
    expect(await screen.findByText(/loading snapshot at 935,000: 37% · 61M coins/)).toBeTruthy();
    expect(screen.queryByText(/syncing/)).toBeNull();
    cleanup();

    fast = { available: true, requested_url: null, status: { state: "loaded", base_height: 935000 } };
    renderAt("/");
    expect(await screen.findByText(/from snapshot at 935,000/)).toBeTruthy();
  });

  it("finds a contact while typing and prefers their BOLT12 offer", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const decoded: string[] = [];
    let paid = "";
    const carol = "cc".repeat(32);
    mockApi({
      "/api/info": () => info(true),
      "/api/settings": () => ({ fiat_currency: "none", fiat_source: "yadio", fiat_source_host: "api.yadio.io" }),
      "/api/contacts": () => ({ discovering: false, contacts: [
        {
          pubkey: carol,
          name: "Carol",
          username: "carol",
          lud16: "carol@pay.example",
          offer: "lno1carol",
          pay_target: "lno1carol",
          targets: [
            { kind: "bolt12", value: "lno1carol", source: "nip-b1" },
            { kind: "lightning_address", value: "carol@pay.example", source: "profile" },
          ],
        },
        { pubkey: "dd".repeat(32), name: "Dave" },
      ] }),
      "/api/payments/decode": (init) => {
        const payment = JSON.parse(String(init?.body)).payment as string;
        decoded.push(payment);
        return payment.startsWith("lno")
          ? { kind: "bolt12", amount_msat: null, description: "tips", network_ok: true, is_expired: false, self_payment: false }
          : { kind: "lnurl", amount_msat: null, description: "Carol", min_msat: 1000, max_msat: 1000000, comment_allowed: 0, network_ok: true, is_expired: false, self_payment: false };
      },
      "/api/payments": (init) => {
        paid = String(init?.body);
        return { public_id: "t1", direction: "outgoing", state: "SETTLED", amount_msat: 5000000, fee_msat: 0, labels: [], description: "", created_at: 1, updated_at: 1 };
      },
    });
    renderAt("/send");
    const input = (await screen.findByPlaceholderText(/contact's name/)) as HTMLTextAreaElement;
    fireEvent.change(input, { target: { value: "car" } });
    expect(await screen.findByRole("option", { name: /Carol/ })).toBeTruthy();
    fireEvent.change(input, { target: { value: "dav" } });
    // Listed, but not payable: shown with the reason and cannot be chosen.
    const dave = await screen.findByRole("option", { name: /Dave/ });
    expect(dave.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText("no Lightning address or offer in their profile")).toBeTruthy();
    fireEvent.change(input, { target: { value: "car" } });
    await screen.findByRole("option", { name: /Carol/ });
    fireEvent.keyDown(input, { key: "Enter" });

    // The offer is chosen first; the address is one click away.
    await waitFor(() => expect(decoded).toEqual(["lno1carol"]));
    expect(input.value).toBe("lno1carol");
    fireEvent.click(screen.getByText("carol@pay.example"));
    await waitFor(() => expect(decoded).toEqual(["lno1carol", "carol@pay.example"]));
    fireEvent.change(await screen.findByLabelText(/Amount/), { target: { value: "5000" } });
    fireEvent.click(screen.getByRole("button", { name: /Pay/ }));
    await waitFor(() => expect(paid).not.toBe(""));
    const body = JSON.parse(paid);
    expect(body.payment).toBe("carol@pay.example");
    expect(body.recipient).toEqual({ pubkey: carol, name: "Carol" });
  });
});

describe("connections", () => {
  it("grants the advanced superuser permission with its risk warning", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let created = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/apps": (init) => {
        created = String(init?.body);
        return {
          app: { id: 9, name: "Alby Go", scopes: ["superuser"] },
          pairing_uri: "nostr+walletconnect://aa?relay=wss%3A%2F%2Frelay.example&secret=ff",
        };
      },
    });
    renderAt("/connections/new");
    fireEvent.change(await screen.findByPlaceholderText(/e.g. Damus/), { target: { value: "Alby Go" } });
    // Off by default: nothing warns until the Advanced option is chosen.
    expect(screen.queryByText("Powerful connection permission")).toBeNull();
    fireEvent.click(screen.getByRole("checkbox", { name: /Allow creating app connections/ }));
    expect(await screen.findByText("Powerful connection permission")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Create connection/ }));
    await waitFor(() => expect(created).not.toBe(""));
    // The default preset plus superuser, and no silent extras.
    expect(JSON.parse(created).scopes).toEqual([
      "pay_invoice",
      "get_balance",
      "make_invoice",
      "lookup_invoice",
      "list_transactions",
      "notifications",
      "superuser",
    ]);
  });

  it("confirms an app's own connection request and tells the app (NWC-08)", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let sent = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/apps/authorize": (init) => {
        sent = String(init?.body);
        return { app: { id: 12, name: "Example App", scopes: [] }, wallet_pubkey: "cc".repeat(32), relay_url: "wss://relay.example", state: "9f".repeat(16) };
      },
    });
    // The app's own link, pasted on the New connection page.
    renderAt("/connections/new");
    const link =
      `nostr+walletauth://${"ab".repeat(32)}?relay=wss%3A%2F%2Fapp.relay.example&state=${"9f".repeat(16)}` +
      "&name=Example%20App&request_methods=pay_invoice%20get_balance&optional_request_methods=make_invoice%20list_transactions" +
      "&max_amount=1000000&renewal_period=monthly&notification_types=payment_received";
    fireEvent.change(await screen.findByPlaceholderText("nostr+walletauth://…"), { target: { value: link } });
    fireEvent.click(screen.getByRole("button", { name: "Review request" }));

    // The confirmation page: who asks, for what, with which limit.
    expect(await screen.findByText("Example App")).toBeTruthy();
    expect(screen.getByText("Pay invoices")).toBeTruthy();
    expect(screen.getByText("1,000 sats, renewed monthly")).toBeTruthy();
    expect(screen.getByText("wss://app.relay.example")).toBeTruthy();
    // An optional method can be left out.
    fireEvent.click(screen.getByRole("checkbox", { name: "Read history" }));
    const told: unknown[] = [];
    const listener = (e: Event) => told.push((e as CustomEvent).detail);
    window.addEventListener("nwc:success", listener);
    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    expect(await screen.findByText("“Example App” is connected")).toBeTruthy();
    window.removeEventListener("nwc:success", listener);
    const body = JSON.parse(sent);
    expect(body).toMatchObject({
      pubkey: "ab".repeat(32),
      state: "9f".repeat(16),
      max_amount: 1000000,
      renewal_period: "monthly",
      request_methods: ["pay_invoice", "get_balance"],
      granted_optional_methods: ["make_invoice"],
      notification_types: ["payment_received"],
      relays: ["wss://app.relay.example"],
    });
    expect(told).toEqual([{ relayUrl: "wss://relay.example", walletPubkey: "cc".repeat(32), state: "9f".repeat(16) }]);
  });

  it("refuses a connection request it cannot honour (NWC-08)", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({ "/api/info": () => info(true) });
    // A required method Allod does not offer: only declining is possible.
    renderAt(`/connections/new?pubkey=${"ab".repeat(32)}&state=${"9f".repeat(16)}&request_methods=pay_keysend`);
    expect(await screen.findByText("Allod cannot give what this app needs")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Approve" }) as HTMLButtonElement).disabled).toBe(true);
    let declined = false;
    const listener = () => (declined = true);
    window.addEventListener("nwc:declined", listener);
    fireEvent.click(screen.getByRole("button", { name: "Decline" }));
    window.removeEventListener("nwc:declined", listener);
    expect(declined).toBe(true);
  });

  it("shows a granted superuser scope on the connection page and can revoke it when editing", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const app = {
      id: 7,
      name: "Alby Go",
      description: "",
      kind: "connection",
      app_pubkey: "aa".repeat(32),
      wallet_pubkey: "bb".repeat(32),
      isolated: false,
      max_amount_msat: 0,
      budget_renewal: "monthly",
      expires_at: null,
      metadata: {},
      relay_urls: null,
      created_at: 1,
      last_used_at: null,
      scopes: ["superuser", "get_balance"],
      budget: null,
      balance_msat: null,
      requests: [],
    };
    let patched = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/apps/7": (init) => {
        if (init?.method === "PATCH") patched = String(init?.body);
        return app;
      },
    });
    renderAt("/connections/7");
    // While the permission is in effect, the page states the risk and flags the scope.
    expect(await screen.findByText("Powerful connection permission")).toBeTruthy();
    expect(screen.getByText("Create app connections")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Edit" }));
    const toggle = await screen.findByRole("checkbox", { name: /Allow creating app connections/ });
    expect((toggle as HTMLInputElement).checked).toBe(true);
    fireEvent.click(toggle);
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    await waitFor(() => expect(patched).not.toBe(""));
    expect(JSON.parse(patched).scopes).toEqual(["get_balance"]);
  });
});

describe("connection push devices", () => {
  it("lists the devices a connection registered and removes one or all", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const app = {
      id: 7, name: "Allod PoC", description: "", kind: "connection", app_pubkey: "aa".repeat(32),
      wallet_pubkey: "bb".repeat(32), isolated: false, max_amount_msat: 0, budget_renewal: "monthly",
      expires_at: null, metadata: {}, relay_urls: null, created_at: 1, last_used_at: null,
      scopes: ["notifications", "get_balance"], budget: null, balance_msat: null, requests: [],
    };
    const sub = (id: number, app_id: number | null, label: string, host: string) => ({
      id, label, app_id, app_name: app_id ? "Allod PoC" : null, endpoint_host: host,
      events: ["payment_received"], hide_amounts: false, created_at: 1, last_success_at: null,
      last_error: null, failures: 0,
    });
    let subs = [
      sub(1, null, "Firefox on Linux", "updates.push.services.mozilla.com"),
      sub(5, 7, "Pixel 9 · Sunup", "updates.push.services.mozilla.com"),
      sub(6, 7, "", "fcm.googleapis.com"),
      sub(8, 9, "Other app", "ntfy.sh"),
    ];
    const deleted: string[] = [];
    const del = (id: number) => (init?: RequestInit) => {
      if (init?.method === "DELETE") {
        deleted.push(String(id));
        subs = subs.filter((s) => s.id !== id);
      }
      return null;
    };
    mockApi({
      "/api/info": () => info(true),
      "/api/apps/7": () => app,
      "/api/push": () => ({ vapid_public_key: "B", events: [], subscriptions: subs }),
      "/api/push/subscriptions/5": del(5),
      "/api/push/subscriptions/6": del(6),
      "/api/push/subscriptions/10": del(10),
    });
    renderAt("/connections/7");
    expect(await screen.findByText("Pixel 9 · Sunup")).toBeTruthy();
    // Unnamed devices fall back to a placeholder; other owners' devices are not listed.
    expect(screen.getByText("Unnamed device")).toBeTruthy();
    expect(screen.queryByText("Firefox on Linux")).toBeNull();
    expect(screen.queryByText("Other app")).toBeNull();

    fireEvent.click(within(screen.getByLabelText("Pixel 9 · Sunup")).getByText("Remove"));
    await waitFor(() => expect(deleted).toEqual(["5"]));
    await waitFor(() => expect(screen.queryByText("Pixel 9 · Sunup")).toBeNull());

    subs = [...subs, sub(10, 7, "Tablet · ntfy", "ntfy.sh")];
    cleanup();
    renderAt("/connections/7");
    fireEvent.click(await screen.findByText("Remove all"));
    await waitFor(() => expect(deleted.length).toBe(3));
    expect(deleted.slice(1).sort()).toEqual(["10", "6"].sort());
  });
});

describe("receive", () => {
  const node = (jit_receive: boolean) => ({
    pubkey: "02ab".repeat(16),
    alias: "n",
    network: "regtest",
    block_height: 1,
    block_hash: "00",
    version: "v",
    listening_addresses: [],
    announcement_addresses: [],
    node_uris: [],
    capabilities: { bolt12: true, force_close: true, splicing: true, hold_invoices: true, jit_receive },
  });
  const tx = { public_id: "t9", state: "PENDING", amount_msat: 9_000_000, payment_request: "lnbcrt9", payment_hash: "ff".repeat(32) };

  it("offers a just-in-time channel only when an LSP is configured", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node(false),
    });
    renderAt("/receive");
    await screen.findByPlaceholderText(/21,000/);
    expect(screen.queryByRole("checkbox", { name: /Just-in-time channel/ })).toBeNull();
    cleanup();

    let created = "";
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node(true),
      "/api/invoices": (init) => {
        created = String(init?.body);
        return tx;
      },
      "/api/transactions/t9": () => tx,
    });
    renderAt("/receive");
    const jit = await screen.findByRole("checkbox", { name: /Just-in-time channel/ });
    // JIT invoices need an amount; the UI says so instead of calling the node.
    fireEvent.click(jit);
    fireEvent.click(screen.getByRole("button", { name: /Create invoice/ }));
    expect(await screen.findByText("Just-in-time invoices need an amount.")).toBeTruthy();
    fireEvent.change(screen.getByLabelText(/Amount/), { target: { value: "9000" } });
    fireEvent.click(screen.getByRole("button", { name: /Create invoice/ }));
    await waitFor(() => expect(created).not.toBe(""));
    expect(JSON.parse(created)).toMatchObject({ jit: true, amount_msat: 9_000_000 });
  });

  it("warns until the backup is checked, more so when receiving on-chain or just-in-time", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/backup": () => ({ verified: false }),
      "/api/node": () => node(true),
    });
    renderAt("/receive");
    expect(await screen.findByText("Back up your recovery phrase")).toBeTruthy();
    const jit = await screen.findByRole("checkbox", { name: /Just-in-time channel/ });
    expect(screen.queryByText("Your recovery phrase is not backed up")).toBeNull();
    fireEvent.click(jit);
    expect(await screen.findByText("Your recovery phrase is not backed up")).toBeTruthy();
    fireEvent.click(jit);
    fireEvent.click(screen.getByText("On-chain"));
    expect(await screen.findByText("Your recovery phrase is not backed up")).toBeTruthy();
    cleanup();

    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/backup": () => ({ verified: true }),
      "/api/node": () => node(true),
    });
    renderAt("/receive");
    fireEvent.click(await screen.findByText("On-chain"));
    await screen.findByText("Get a new address");
    expect(screen.queryByText("Back up your recovery phrase")).toBeNull();
    expect(screen.queryByText("Your recovery phrase is not backed up")).toBeNull();
  });
});

describe("accounts", () => {
  const account = {
    id: 5,
    name: "Kids",
    description: "",
    kind: "account",
    app_pubkey: "cc".repeat(32),
    wallet_pubkey: "dd".repeat(32),
    isolated: true,
    max_amount_msat: 0,
    budget_renewal: "never",
    expires_at: null,
    metadata: { allod_kind: "account" },
    relay_urls: null,
    created_at: 1,
    last_used_at: null,
    scopes: ["pay_invoice", "get_balance"],
    budget: null,
    balance_msat: 70_000_000,
    lud16_username: null,
    closed_at: null,
    requests: [],
  };
  const balances = {
    lightning: { total_msat: 100_000_000, spendable_msat: 50_000_000, receivable_msat: 0 },
    onchain: { total_sat: 0, spendable_sat: 0, reserved_sat: 0, pending_from_closures_sat: 0 },
    accounts_total_msat: 70_000_000,
    main_spendable_msat: 30_000_000,
    in_flight_msat: 0,
  };

  it("lists Main and the accounts, and warns when accounts exceed what the node can spend", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/accounts/summary": () => ({ count: 1, total_balance_msat: 70_000_000 }),
      "/api/balances": () => balances,
    });
    renderAt("/accounts");
    expect(await screen.findByText("Accounts are under-collateralised")).toBeTruthy();
    expect(screen.getAllByText("Main").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Kids").length).toBeGreaterThan(0);
    expect(screen.getAllByText("70,000").length).toBeGreaterThan(0);
  });

  it("shows linked NIP-05, Lightning address and profile avatar on account", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const person = "ab".repeat(32);
    const fetchMock = mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/accounts/summary": () => ({ count: 1, total_balance_msat: 70_000_000 }),
      "/api/balances": () => balances,
      "/api/nip05": () => ({ names: [{ id: 1, account_id: 5, domain_id: 2, domain: "example.org", username: "kid", pubkey: person, npub: "npub1kid" }] }),
      "/api/domains": () => ({ domains: [{ id: 2, addresses: [{ target: "5", address: "kid@example.org" }] }] }),
    });
    const original = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (url.startsWith("/api/profiles?")) {
        const pubkey = new URL(url, "http://localhost").searchParams.get("pubkeys");
        return new Response(JSON.stringify(pubkey === person ? [{ pubkey: person, picture: "https://example.org/kid.png" }] : []));
      }
      return original(url, init);
    });
    renderAt("/accounts");
    const identity = await screen.findByText(/kid@example.org/);
    expect(screen.getAllByText(/kid@example.org/)).toHaveLength(1);
    expect(identity.textContent).toContain("⚡");
    expect(document.querySelector('img[src="/nostr.png"]')).not.toBeNull();
    await waitFor(() => expect(document.querySelector('img[src="https://example.org/kid.png"]')).not.toBeNull());
  });

  it("shows every Main Lightning address without a count suffix", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/accounts/summary": () => ({ count: 1, total_balance_msat: 70_000_000 }),
      "/api/balances": () => balances,
      "/api/domains": () => ({ domains: [{ addresses: [
        { target: "main", address: "one@example.org" },
        { target: "main", address: "two@example.org" },
        { target: "main", address: "three@example.org" },
      ] }] }),
    });
    renderAt("/accounts");
    expect(await screen.findByText("⚡ one@example.org")).toBeTruthy();
    expect(screen.getByText("⚡ two@example.org")).toBeTruthy();
    expect(screen.getByText("⚡ three@example.org")).toBeTruthy();
    expect(screen.queryByText(/\+2/)).toBeNull();
  });

  it("offers owned and purchased domains in the Lightning address dialog", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => ({ status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {} }),
      "/api/domains": () => ({ domains: [], gateways: [], dns_providers: [], njalla_purchase: null }),
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Add a Lightning address" }));
    expect(screen.getByRole("link", { name: "twelve.cash" }).getAttribute("href")).toBe("https://twelve.cash");
    expect(screen.getByRole("link", { name: "clank.money" }).getAttribute("href")).toBe("https://clank.money");
    expect(screen.getByRole("link", { name: "ZEUS Pay" }).getAttribute("href")).toBe("https://zeusln.com");
    expect(screen.getByRole("link", { name: "Alby" }).getAttribute("href")).toBe("https://getalby.com/");
    expect(screen.queryByText("Visit")).toBeNull();
    expect(screen.getByRole("button", { name: "Add a domain you own" })).toBeTruthy();
    // The first screen asks BIP-353 or LNURL directly: no wallet step in between.
    fireEvent.click(screen.getByRole("button", { name: "Choose BIP-353" }));
    expect(await screen.findByText("Choose a BIP-353 registry")).toBeTruthy();
    expect(screen.queryByText("Which wallets should pay it?")).toBeNull();
    expect(screen.getByText("twelve.cash · free random name")).toBeTruthy();
    expect(screen.queryByText("ZEUS Pay · free")).toBeNull();
    fireEvent.click(screen.getByText("Back"));
    fireEvent.click(screen.getByRole("button", { name: "Choose LNURL" }));
    expect(await screen.findByText("Choose an LNURL service")).toBeTruthy();
    expect(screen.queryByText("Which wallets should pay it?")).toBeNull();
    expect(screen.getByText("ZEUS Pay · free")).toBeTruthy();
    expect(screen.queryByText("twelve.cash · free random name")).toBeNull();
    fireEvent.click(screen.getByText("Back"));
    fireEvent.click(screen.getByRole("button", { name: "Buy a domain" }));
    expect(await screen.findByText("Buy a domain: connect Njalla")).toBeTruthy();
  });

  it("splits an own address's payments with a prism and keeps the rest", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let put = "";
    const emptyPrism = {
      splits: [],
      split_bps: 0,
      stats: {
        forwarded_msat: 0,
        failed_msat: 0,
        pending_msat: 0,
        pending_count: 0,
        failed_count: 0,
        skipped_msat: 0,
        payments: 0,
      },
    };
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => ({ status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {} }),
      "/api/domains": () => ({
        domains: [
          {
            id: 2,
            name: "example.org",
            served: true,
            addresses: [
              {
                id: 8,
                domain_id: 2,
                username: "pay",
                address: "pay@example.org",
                target: "main",
                lnurl_enabled: true,
                bip353: true,
                prism: emptyPrism,
              },
            ],
          },
        ],
      }),
      "/api/addresses/8/prism": (init) => {
        put = String(init?.body);
        return { domains: [{ id: 2, name: "example.org", served: true, addresses: [] }], gateways: [], dns_providers: [] };
      },
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Open pay@example.org" }));
    fireEvent.click(screen.getByRole("tab", { name: /Prism/ }));
    // Background reading about prisms is one click away.
    expect(screen.getByRole("link", { name: /What is a prism\?/ }).getAttribute("href")).toBe(
      "https://dergigi.com/2023/03/12/lightning-prisms/",
    );
    expect(screen.getByText(/Nothing is split: every payment stays in this wallet/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Add a split" }));
    fireEvent.change(screen.getByLabelText("Destination of split 1"), { target: { value: "₿alice@example.org" } });
    fireEvent.change(screen.getByLabelText("Percentage of split 1"), { target: { value: "60" } });
    // A note is for the owner alone: it says whose wallet this is. It stays folded away
    // until asked for, so it does not take space from the split itself.
    fireEvent.click(screen.getByRole("button", { name: "Add note for split 1" }));
    fireEvent.change(screen.getByLabelText("Note for split 1"), { target: { value: "Alice's node" } });
    expect(screen.getByText("BIP-353")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Add a split" }));
    fireEvent.change(screen.getByLabelText("Destination of split 2"), { target: { value: "bob@example.org" } });
    fireEvent.change(screen.getByLabelText("Percentage of split 2"), { target: { value: "50" } });
    // A plain name may be either: it is not called LNURL unless it says so.
    expect(screen.getByText("BIP-353 or LNURL")).toBeTruthy();

    // More than 100% is refused before anything is saved.
    expect(screen.getAllByText("The splits add up to more than 100%.").length).toBeGreaterThan(0);
    const save = screen.getByRole("button", { name: "Save splits" }) as HTMLButtonElement;
    expect(save.disabled).toBe(true);

    fireEvent.change(screen.getByLabelText("Percentage of split 2"), { target: { value: "25" } });
    expect(screen.getByText("85% forwarded in 2 splits")).toBeTruthy();
    expect(screen.getByText("15% stays in this wallet")).toBeTruthy();
    expect(save.disabled).toBe(false);
    fireEvent.click(save);
    await waitFor(() => expect(put).not.toBe(""));
    expect(JSON.parse(put)).toEqual({
      splits: [
        { destination: "₿alice@example.org", percent_bps: 6000, note: "Alice's node" },
        { destination: "bob@example.org", percent_bps: 2500, note: "" },
      ],
    });
  });

  it("shows what a prism could not send and retries the failed payouts", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let retried = false;
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/prism/failures": () => ({
        failed_count: 2,
        failed_msat: 25_000,
        addresses: [{ address_id: 8, address: "pay@example.org", failed_count: 2, failed_msat: 25_000 }],
      }),
      "/api/public-name": () => ({ status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {} }),
      "/api/domains": () => ({
        domains: [
          {
            id: 2,
            name: "example.org",
            served: true,
            addresses: [
              {
                id: 8,
                domain_id: 2,
                username: "pay",
                address: "pay@example.org",
                target: "main",
                lnurl_enabled: true,
                bip353: true,
                prism: {
                  splits: [
                    {
                      id: 1,
                      destination: "bob@example.org",
                      note: "Rent to Bob",
                      kind: "bip353",
                      position: 0,
                      percent_bps: 5000,
                      forwarded_msat: 40_000,
                      failed_msat: 25_000,
                      pending_msat: 0,
                      pending_count: 0,
                      failed_count: 2,
                      skipped_msat: 0,
                      last_error: "no route to the destination",
                    },
                  ],
                  split_bps: 5000,
                  stats: {
                    forwarded_msat: 40_000,
                    failed_msat: 25_000,
                    pending_msat: 0,
                    pending_count: 0,
                    failed_count: 2,
                    skipped_msat: 0,
                    payments: 4,
                  },
                },
              },
            ],
          },
        ],
      }),
      "/api/addresses/8/prism/retry": () => {
        retried = true;
        return {
          domains: [
            {
              id: 2,
              name: "example.org",
              served: true,
              addresses: [
                {
                  id: 8,
                  domain_id: 2,
                  username: "pay",
                  address: "pay@example.org",
                  target: "main",
                  lnurl_enabled: true,
                  bip353: true,
                  prism: {
                    splits: [],
                    split_bps: 0,
                    stats: {
                      forwarded_msat: 0,
                      failed_msat: 0,
                      pending_msat: 0,
                      pending_count: 0,
                      failed_count: 0,
                      skipped_msat: 0,
                      payments: 0,
                    },
                  },
                },
              ],
            },
          ],
          gateways: [],
          dns_providers: [],
        };
      },
    });
    renderAt("/settings/lightning-addresses");
    // The list itself shows what the prism forwards, and what it could not send.
    expect(await screen.findByText("50%")).toBeTruthy();
    expect(screen.getByText("25 sats not sent")).toBeTruthy();
    // The warning follows into the settings sections and the main menu (desktop and mobile).
    expect((await screen.findAllByLabelText("2 prism payouts could not be sent")).length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText("25 sats failed to send")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open pay@example.org" }));
    fireEvent.click(screen.getByRole("tab", { name: /Prism/ }));
    // What the server found for the plain name when it saved it.
    expect(screen.getByText("BIP-353")).toBeTruthy();
    // The owner's own note comes back with the split, folded into one discreet line.
    expect(screen.getByText("Rent to Bob")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Edit note for split 1" })).toBeTruthy();
    // The amount that never arrived stays visible, per split and in total.
    expect(screen.getByText("25 sats not sent (2)")).toBeTruthy();
    expect(screen.getByText(/could not be sent, in 2 payouts/)).toBeTruthy();
    expect(screen.getByText("Last error: no route to the destination")).toBeTruthy();
    expect(screen.getByText("40 sats forwarded")).toBeTruthy();
    expect(screen.getByText(/40 sats forwarded so far, over 4 payments/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: /Retry now/ }));
    await waitFor(() => expect(retried).toBe(true));
    expect(await screen.findByText("Sending the failed splits again.")).toBeTruthy();
  });

  it("shows a prism's payout history and clears what a split could not send", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const now = Math.floor(Date.now() / 1000);
    let cleared: string | null = null;
    let historyCalls = 0;
    const address = (failed: number) => ({
      id: 8,
      domain_id: 2,
      username: "pay",
      address: "pay@example.org",
      target: "main",
      lnurl_enabled: true,
      bip353: true,
      prism: {
        splits: [
          {
            id: 1,
            destination: "bob@example.org",
            note: "Rent to Bob",
            kind: "bip353",
            position: 0,
            percent_bps: 5000,
            forwarded_msat: 40_000,
            failed_msat: failed ? 25_000 : 0,
            pending_msat: 0,
            pending_count: 0,
            failed_count: failed,
            skipped_msat: 0,
          },
        ],
        split_bps: 5000,
        stats: {
          forwarded_msat: 40_000,
          failed_msat: failed ? 25_000 : 0,
          pending_msat: 0,
          pending_count: 0,
          failed_count: failed,
          skipped_msat: 0,
          payments: 4,
        },
      },
    });
    const domains = (failed: number) => ({
      domains: [{ id: 2, name: "example.org", served: true, addresses: [address(failed)] }],
      gateways: [],
      dns_providers: [],
    });
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/prism/failures": () => ({
        failed_count: 2,
        failed_msat: 25_000,
        addresses: [{ address_id: 8, address: "pay@example.org", failed_count: 2, failed_msat: 25_000 }],
      }),
      "/api/public-name": () => ({ status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {} }),
      "/api/domains": () => domains(2),
      "/api/addresses/8/prism/payouts": () => {
        historyCalls += 1;
        if (historyCalls === 1) {
          return {
            payouts: [
              {
                id: 3,
                destination: "bob@example.org",
                note: "Rent to Bob",
                kind: "bip353",
                amount_msat: 25_000,
                fee_msat: 0,
                percent_bps: 5000,
                state: "FAILED",
                attempts: 8,
                last_error: "no route to the destination",
                created_at: now - 120,
                updated_at: now - 60,
                paid_at: null,
              },
            ],
            next_before_id: 3,
          };
        }
        return {
          payouts: [
            {
              id: 2,
              destination: "bob@example.org",
              note: "Rent to Bob",
              kind: "bip353",
              amount_msat: 40_000,
              fee_msat: 1_000,
              percent_bps: 5000,
              state: "PAID",
              attempts: 1,
              last_error: null,
              created_at: now - 3600,
              updated_at: now - 3500,
              paid_at: now - 3500,
            },
          ],
          next_before_id: null,
        };
      },
      "/api/addresses/8/prism/reset": (init) => {
        cleared = String(init?.body);
        return domains(0);
      },
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Open pay@example.org" }));
    fireEvent.click(screen.getByRole("tab", { name: /Prism/ }));

    // The history is one fold away and loads when opened.
    fireEvent.click(screen.getByRole("button", { name: "Payout history" }));
    expect(await screen.findByText("8 tries")).toBeTruthy();
    expect(screen.getByText(/no route to the destination/)).toBeTruthy();
    expect(screen.getByText(/Rent to Bob · bob@example.org/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show older" }));
    expect(await screen.findByText("1 try")).toBeTruthy();
    expect(screen.getByText("fee 1")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Show older" })).toBeNull();

    // Clearing the failed total asks once, then sends nothing: it marks it canceled.
    fireEvent.click(screen.getByRole("button", { name: "Clear failed" }));
    expect(screen.getByText("Clear 25 sats?")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Yes, clear" }));
    await waitFor(() => expect(cleared).not.toBeNull());
    expect(JSON.parse(cleared!)).toEqual({ split_id: 1 });
    expect(
      await screen.findByText("The failed amount of this split is cleared; nothing was sent."),
    ).toBeTruthy();
  });

  it("shows the offer behind a registry address in its dialog", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => ({
        status: { errors: [], synced_at: null },
        twelve_cash: [{ target: "main", address: "calm.fox@twelve.cash", offer_id: "o2", custom: false, verified: true, active: true, created_at: 1 }],
        clank: [],
        alby: [],
        zeus: null,
        prefs: {},
      }),
      "/api/domains": () => ({ domains: [], gateways: [], dns_providers: [], njalla_purchase: null }),
      "/api/offers": () => [
        { offer_id: "o1", offer: "lno1other", app_id: null, amount_msat: null, description: null, state: "active", expires_at: null, created_at: 1 },
        { offer_id: "o2", offer: "lno1twelvecashoffer", app_id: null, amount_msat: null, description: null, state: "active", expires_at: null, created_at: 1 },
      ],
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Open calm.fox@twelve.cash" }));
    expect(await screen.findByText("lno1twelvecashoffer")).toBeTruthy();
    expect(screen.getByRole("button", { name: /Copy offer/ })).toBeTruthy();
    expect(screen.queryByText("lno1other")).toBeNull();
  });

  it("shows the invoice when the wallet cannot pay for a name", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const invoice = "lnbc50u1p3exampleinvoice";
    const data = { status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {}, twelve_cash_available: true, clank_available: true };
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => data,
      "/api/public-name/twelve-cash/buy": () =>
        new Response(
          JSON.stringify({ error: { code: "CONFLICT", message: "paying for the name failed: insufficient balance", invoice } }),
          { status: 409 },
        ),
      "/api/public-name/refresh": () => data,
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Add a Lightning address" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose BIP-353" }));
    fireEvent.click(await screen.findByText(/twelve\.cash · your name/));
    fireEvent.change(screen.getByPlaceholderText("satoshi"), { target: { value: "satoshi" } });
    fireEvent.click(screen.getByText(/^Buy for /));
    fireEvent.click(await screen.findByText(/^Pay 5,000 sats$/));
    expect(await screen.findByText("Pay from another wallet")).toBeTruthy();
    expect(screen.getByText(invoice)).toBeTruthy();
    fireEvent.click(screen.getByText("I paid it — check now"));
    await waitFor(() => expect(screen.queryByText("Pay from another wallet")).toBeNull());
  });

  it("takes the clank.money payment proof after paying from another wallet", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const invoice = "lnbc9990n1pexampleinvoice";
    const data = { status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {}, twelve_cash_available: true, clank_available: true };
    let proof = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => data,
      "/api/public-name/clank": () =>
        new Response(
          JSON.stringify({ error: { code: "CONFLICT", message: "paying for the name failed: insufficient balance", invoice } }),
          { status: 409 },
        ),
      "/api/public-name/clank/proof": (init) => {
        proof = String(init?.body);
        return data;
      },
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Add a Lightning address" }));
    fireEvent.click(screen.getByRole("button", { name: "Choose BIP-353" }));
    fireEvent.click(await screen.findByText(/clank\.money · your name/));
    fireEvent.change(screen.getByPlaceholderText("mynode"), { target: { value: "mynode" } });
    fireEvent.click(screen.getByText(/^Buy for up to/));
    expect(await screen.findByText("Pay from another wallet")).toBeTruthy();
    expect(screen.getByText(invoice)).toBeTruthy();
    const preimage = "ab".repeat(32);
    fireEvent.change(screen.getByPlaceholderText("64 hex characters"), { target: { value: preimage } });
    fireEvent.click(screen.getByText("Finish registration"));
    await waitFor(() => expect(JSON.parse(proof)).toEqual({ target: "main", preimage }));
  });

  it("shows the BIP-353 DNS records tab only on a BIP-353 address", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const txtName = "pay.user._bitcoin-payment.example.org.";
    const address = (id: number, username: string, bip353: boolean) => ({
      id,
      domain_id: 2,
      username,
      address: `${username}@example.org`,
      target: "main",
      lnurl_enabled: true,
      bip353,
      verified: bip353,
      record_name: bip353 ? txtName : null,
    });
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [], total: 0 }),
      "/api/public-name": () => ({ status: { errors: [], synced_at: null }, twelve_cash: [], clank: [], alby: [], zeus: null, prefs: {} }),
      "/api/domains": () => ({
        domains: [
          {
            id: 2,
            name: "example.org",
            served: true,
            dns_provider_id: null,
            records: [
              { type: "A", name: "example.org.", value: "203.0.113.7", ok: true },
              { type: "TXT", name: txtName, value: "bitcoin:?lno=lno1pay", ok: true },
            ],
            addresses: [address(8, "pay", true), address(9, "tips", false)],
          },
        ],
      }),
    });
    renderAt("/settings/lightning-addresses");
    fireEvent.click(await screen.findByRole("button", { name: "Open pay@example.org" }));
    fireEvent.click(screen.getByRole("tab", { name: "BIP-353 DNS records" }));
    // Only the address's own TXT record: the A record belongs to the domain.
    expect(screen.getByText(txtName)).toBeTruthy();
    expect(screen.queryByText("203.0.113.7")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));

    // An LNURL-only address has no DNS record of its own.
    fireEvent.click(await screen.findByRole("button", { name: "Open tips@example.org" }));
    expect(screen.getByRole("tab", { name: /Prism/ })).toBeTruthy();
    expect(screen.queryByRole("tab", { name: /DNS records/ })).toBeNull();
  });

  it("creates account with new NIP-05 name and Lightning address", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    const person = "ab".repeat(32);
    let nipBody = "";
    let addressBody = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/domains": () => ({ domains: [{ id: 2, name: "example.org", served: true, addresses: [] }] }),
      "/api/nip05": (init) => {
        if (init?.method === "POST") nipBody = String(init.body);
        return { names: [] };
      },
      "/api/accounts": () => ({ account: { ...account, id: 5, name: "Kids" }, pairing_uri: "nostr+walletconnect://test", bip353: null }),
      "/api/domains/2/addresses": (init) => {
        addressBody = String(init?.body);
        return { domains: [{ id: 2, addresses: [{ username: "kid", address: "kid@example.org", txt: "bitcoin:?lno=test", record_name: "kid.user._bitcoin-payment.example.org." }] }] };
      },
    });
    renderAt("/accounts/new");
    fireEvent.change(await screen.findByPlaceholderText("e.g. Kids, Phone, Shop"), { target: { value: "Kids" } });
    fireEvent.change(screen.getByLabelText("Nostr name (NIP-05) choice"), { target: { value: "new" } });
    fireEvent.change(screen.getByPlaceholderText("npub1…"), { target: { value: nip19.npubEncode(person) } });
    fireEvent.change(screen.getByPlaceholderText("alice"), { target: { value: "kid" } });
    fireEvent.click(screen.getByLabelText("Add Lightning address"));
    fireEvent.change(screen.getAllByPlaceholderText("alice")[1]!, { target: { value: "kid" } });
    fireEvent.click(screen.getByRole("button", { name: "Create account" }));
    await waitFor(() => expect(nipBody).not.toBe(""));
    await waitFor(() => expect(addressBody).not.toBe(""));
    expect(JSON.parse(nipBody).account_id).toBe(5);
    expect(JSON.parse(addressBody).target).toBe("5");
    expect(await screen.findByText("Nostr name (NIP-05):")).toBeTruthy();
    expect(screen.getByText("Lightning address:")).toBeTruthy();
  });

  it("shows identity pills on account header and can unlink them", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let nipPatch = "";
    let addressPatch = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/accounts/5": () => account,
      "/api/accounts/5/statement": () => ({ items: [], total: 0, balance_msat: 70_000_000 }),
      "/api/balances": () => balances,
      "/api/nip05": () => ({ names: [{ id: 4, account_id: 5, domain_id: 2, domain: "example.org", username: "kid", pubkey: "ab".repeat(32), npub: nip19.npubEncode("ab".repeat(32)) }] }),
      "/api/nip05/4": (init) => { nipPatch = String(init?.body); return { names: [] }; },
      "/api/domains": () => ({ domains: [{ id: 2, name: "example.org", served: true, addresses: [{ id: 8, domain_id: 2, username: "pay", address: "pay@example.org", target: "5", lnurl_enabled: true, bip353: true }] }] }),
      "/api/addresses/8": (init) => { addressPatch = String(init?.body); return { domains: [] }; },
    });
    renderAt("/accounts/5");
    expect(await screen.findByRole("button", { name: "Manage Lightning address pay@example.org" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "+ Nostr name (NIP-05)" })).toBeNull();
    expect(screen.queryByRole("button", { name: "+ Lightning address" })).toBeNull();
    fireEvent.click(await screen.findByRole("button", { name: "Manage NIP-05 kid@example.org" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove or unlink" }));
    fireEvent.click(screen.getByRole("button", { name: "Keep Nostr name (NIP-05), unlink account" }));
    await waitFor(() => expect(JSON.parse(nipPatch).account_id).toBeNull());
    fireEvent.click(screen.getByRole("button", { name: "Manage Lightning address pay@example.org" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove or unlink" }));
    fireEvent.click(screen.getByRole("button", { name: "Keep address, move payments to Main" }));
    await waitFor(() => expect(JSON.parse(addressPatch).target).toBe("main"));
  });

  it("closes a funded account only after typing its name and signing the sweep", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    localStorage.setItem("allod.signer", JSON.stringify({ kind: "extension" }));
    let deleted: { url: string; headers: Record<string, string> } | null = null;
    const fetchMock = mockApi({
      "/api/info": () => info(true),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/accounts/5": () => account,
      "/api/accounts/5/statement": () => ({ items: [], total: 0, balance_msat: 70_000_000 }),
      "/api/balances": () => balances,
    });
    const original = fetchMock.getMockImplementation()!;
    fetchMock.mockImplementation(async (url: string, init?: RequestInit) => {
      if (init?.method === "DELETE") {
        deleted = { url, headers: init.headers as Record<string, string> };
        return new Response(JSON.stringify({ account: { ...account, closed_at: 2 }, sweep: null }), { status: 200 });
      }
      return original(url, init);
    });
    renderAt("/accounts/5");
    fireEvent.click(await screen.findByRole("button", { name: "Close account" }));
    const confirm = screen.getByRole("button", { name: /Sign, sweep to Main and close/ }) as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Type Kids to confirm"), { target: { value: "Kids" } });
    expect(confirm.disabled).toBe(false);
    fireEvent.click(confirm);
    await waitFor(() => expect(deleted).not.toBeNull());
    const d = deleted!;
    expect(d.url).toBe("/api/accounts/5?sweep=true");
    expect(d.headers["X-Allod-Session"]).toBe("t");
    const event = JSON.parse(atob((d.headers.Authorization ?? "").replace("Nostr ", ""))) as NostrEvent;
    expect(verifyEvent(event)).toBe(true);
    expect(event.tags).toContainEqual(["u", `${window.location.origin}/api/accounts/5?sweep=true`]);
    expect(event.tags).toContainEqual(["method", "DELETE"]);
  });

  it("pays from the account chosen in Send", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    let paid = "";
    mockApi({
      "/api/info": () => info(true),
      "/api/health": () => ({ status: "ok", db: { ok: true }, ldk_server: { ok: true } }),
      "/api/accounts": () => ({ items: [account], total: 1 }),
      "/api/balances": () => balances,
      "/api/contacts": () => ({ contacts: [], discovering: false }),
      "/api/payments/decode": () => ({
        kind: "bolt11",
        amount_msat: 1_000_000,
        description: "coffee",
        payee: null,
        expires_at: null,
        is_expired: false,
        network_ok: true,
        self_payment: false,
        address: null,
      }),
      "/api/payments": (init) => {
        paid = String(init?.body);
        return { public_id: "x", state: "SETTLED", amount_msat: 1_000_000, fee_msat: 0, settled_at: 1 };
      },
    });
    renderAt("/send");
    const select = (await screen.findByLabelText("Pay from")) as HTMLSelectElement;
    expect(select.value).toBe("main");
    fireEvent.change(select, { target: { value: "5" } });
    fireEvent.change(screen.getByPlaceholderText(/lnbc/), { target: { value: "lnbcrt10u1test" } });
    fireEvent.click(screen.getByRole("button", { name: /Review/ }));
    fireEvent.click(await screen.findByRole("button", { name: /^Pay$/ }));
    await waitFor(() => expect(paid).not.toBe(""));
    expect(JSON.parse(paid).from_app_id).toBe(5);
  });
});

describe("versionLabel", () => {
  it("prefixes release versions with v and keeps nightly names", () => {
    expect(versionLabel("0.2.3")).toBe("v0.2.3");
    expect(versionLabel("nightly-20261116-1600")).toBe("nightly-20261116-1600");
  });
});
