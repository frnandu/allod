import type { Page } from "@playwright/test";
import { finalizeEvent, getPublicKey, type EventTemplate } from "nostr-tools";
import { hexToBytes } from "nostr-tools/utils";

// Fixed regtest owner key from deploy/regtest.env (public test key).
export const OWNER_SECRET_HEX =
  process.env.DEV_OWNER_SECRET_HEX ?? "9a2fb0b5e2cca568a75ae871559e357aed59005cf30ff7053f01b10709c04f0a";

/** Injects a NIP-07 `window.nostr` backed by `secretHex` into the page. */
export async function installSigner(page: Page, secretHex = OWNER_SECRET_HEX) {
  const sk = hexToBytes(secretHex);
  const pubkey = getPublicKey(sk);
  await page.exposeFunction("__allodTestSign", (template: EventTemplate) => finalizeEvent(template, sk));
  await page.addInitScript((pk: string) => {
    const w = window as unknown as {
      nostr: unknown;
      __allodTestSign: (t: unknown) => Promise<unknown>;
    };
    w.nostr = {
      getPublicKey: async () => pk,
      signEvent: (t: unknown) => w.__allodTestSign(t),
    };
  }, pubkey);
  return pubkey;
}
