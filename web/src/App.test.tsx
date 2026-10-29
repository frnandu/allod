import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { createMemoryRouter, RouterProvider } from "react-router";
import { SWRConfig } from "swr";
import { finalizeEvent, generateSecretKey, getPublicKey, verifyEvent, type NostrEvent } from "nostr-tools";
import { routes } from "./App";
import { _test, nip98Header } from "./lib/nip98";
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

describe("nip98", () => {
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

describe("setup wizard", () => {
  it("creates a node and requires a backup check", async () => {
    let setupAuth = "";
    let fastSyncBody = "";
    mockApi({
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
    expect(await screen.findByText("Display currency")).toBeTruthy();
    await waitFor(() => expect(screen.getByRole("option", { name: /US Dollar/ }).getAttribute("aria-selected")).toBe("true"));
    // Currencies come from the node (server-side rates), with names from Intl.
    fireEvent.change(screen.getByLabelText("Currency"), { target: { value: "peso" } });
    const peso = await screen.findByRole("option", { name: /Argentine Peso/ });
    expect(screen.queryByRole("option", { name: /Euro/ })).toBeNull();
    fireEvent.click(peso);
    fireEvent.click(screen.getByText("Save"));
    expect(await screen.findByText("Saved.")).toBeTruthy();
    expect(JSON.parse(saved)).toEqual({ fiat_currency: "ARS", fiat_source: "yadio" });
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
