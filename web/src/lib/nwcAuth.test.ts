import { describe, expect, it } from "vitest";
import { parseAuthRequest, returnUrl, safeIcon, walletAuthPath } from "./nwcAuth";

const key = "ab".repeat(32);
const state = "9f".repeat(16);

describe("NWC-08 requests", () => {
  it("needs the app key and a state the app can check", () => {
    expect(typeof parseAuthRequest(new URLSearchParams(`pubkey=${key}`))).toBe("string");
    expect(typeof parseAuthRequest(new URLSearchParams(`pubkey=${key}&state=XYZ`))).toBe("string");
    expect(typeof parseAuthRequest(new URLSearchParams(`pubkey=${key}&state=${state}&renewal_period=monthly`))).toBe("string");
    const r = parseAuthRequest(new URLSearchParams(`pubkey=${key}&state=${state}&isolated=true&metadata=%7B%22a%22%3A1%7D`));
    expect(r).toMatchObject({ pubkey: key, state, isolated: true, metadata: { a: 1 }, requestMethods: [], relays: [] });
  });

  it("turns a wallet auth link into the confirmation page", () => {
    expect(walletAuthPath("nostr+walletconnect://x")).toBeNull();
    expect(walletAuthPath(`nostr+walletauth://${key}?state=${state}`)).toBeNull();
    const path = walletAuthPath(`nostr+walletauth+allod://${key.toUpperCase()}?relay=wss%3A%2F%2Fr.example&state=${state}&pubkey=evil`);
    expect(path).toBe(`/connections/new?relay=wss%3A%2F%2Fr.example&state=${state}&pubkey=${key}`);
  });

  it("returns to the app only through harmless links", () => {
    const result = { wallet_pubkey: "cc", relay_url: "wss://r.example", state, lud16: undefined };
    expect(returnUrl("javascript:alert(1)", result)).toBeNull();
    expect(returnUrl("data:text/html,x", result)).toBeNull();
    expect(returnUrl("not a link", result)).toBeNull();
    expect(returnUrl("ndk://nwc", result)).toBe(`ndk://nwc?wallet_pubkey=cc&relay_url=wss%3A%2F%2Fr.example&state=${state}`);
    expect(returnUrl("https://app.example/done?x=1", result)).toContain("x=1&wallet_pubkey=cc");
    expect(safeIcon("http://app.example/i.png")).toBeNull();
    expect(safeIcon("https://app.example/i.png")).toBe("https://app.example/i.png");
  });
});
