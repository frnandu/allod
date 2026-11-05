import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { SWRConfig } from "swr";
import { finalizeEvent, generateSecretKey, getPublicKey, verifyEvent, type NostrEvent } from "nostr-tools";
import { routes } from "./App";
import { _test, nip98Header } from "./lib/nip98";
import { _resetProgress } from "./lib/eta";
import { clearSession, setSession } from "./lib/session";

const sk = generateSecretKey();
const pk = getPublicKey(sk);
const PHRASE = Array.from({ length: 24 }, (_, i) => `word${i + 1}`).join(" ");

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
    if (!h) return new Response(JSON.stringify({ error: { code: "NOT_FOUND", message: path } }), { status: 404 });
    return new Response(JSON.stringify(h(init)), { status: 200 });
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

beforeEach(() => {
  clearSession();
  localStorage.clear();
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

  it("asks to sign in once set up", async () => {
    mockApi({ "/api/info": () => info(true) });
    renderAt("/");
    expect(await screen.findByRole("heading", { name: "Sign in" })).toBeTruthy();
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
    expect(await screen.findByText("1.5 sat/vB")).toBeTruthy();
    expect(screen.getByText("Fee estimates are still missing")).toBeTruthy();
    expect(screen.getAllByText("no estimate")).toHaveLength(4);
  });

  it("says nothing about fees without a bundled bitcoind", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    mockApi({
      "/api/info": () => info(true),
      "/api/node": () => node,
      "/api/fees": () => ({ source: null, available: false, targets: [], missing: [] }),
    });
    renderAt("/node");
    expect(await screen.findByText("allod")).toBeTruthy();
    expect(screen.queryByText("Fee estimates")).toBeNull();
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
    fireEvent.click(await screen.findByText("Browser extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));

    expect(await screen.findByText("Write down your recovery phrase")).toBeTruthy();
    expect(setupAuth.startsWith("Nostr ")).toBe(true);
    expect(screen.getByText("word24")).toBeTruthy();

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
    // Optional fast sync: full validation is the default; choosing fast fills the
    // suggested snapshot, which can be changed.
    expect(await screen.findByText("Initial sync")).toBeTruthy();
    fireEvent.click(screen.getByText("Fast sync from a snapshot"));
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
    fireEvent.click(await screen.findByText("Browser extension"));
    fireEvent.click(await screen.findByText("Continue"));
    fireEvent.click(await screen.findByText("Create my node"));
    fireEvent.click(await screen.findByRole("checkbox"));
    fireEvent.click(screen.getByText("Continue"));
    fireEvent.click(await screen.findByText("Skip check"));
    expect(await screen.findByText("Your node is set up")).toBeTruthy();
    expect(verified).toBe(false);
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

  it("pays from the account chosen in the switcher", async () => {
    setSession({ token: "t", expires_at: Math.floor(Date.now() / 1000) + 3600, pubkey: pk });
    localStorage.setItem("allod.account", "5");
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
    await waitFor(() => expect(select.value).toBe("5"));
    fireEvent.change(screen.getByPlaceholderText(/lnbc/), { target: { value: "lnbcrt10u1test" } });
    fireEvent.click(screen.getByRole("button", { name: /Review/ }));
    fireEvent.click(await screen.findByRole("button", { name: /^Pay$/ }));
    await waitFor(() => expect(paid).not.toBe(""));
    expect(JSON.parse(paid).from_app_id).toBe(5);
  });
});
