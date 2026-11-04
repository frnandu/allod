import type { EventTemplate, NostrEvent } from "nostr-tools";
import * as nip04 from "nostr-tools/nip04";
import * as nip44 from "nostr-tools/nip44";
import * as nip49 from "nostr-tools/nip49";
import type { SimplePool } from "nostr-tools/pool";
import { finalizeEvent, generateSecretKey, getPublicKey, verifyEvent } from "nostr-tools/pure";
import { bytesToHex } from "nostr-tools/utils";
import { afterEach, describe, expect, it } from "vitest";
import { waitForNostrConnect, type Encryption } from "./nip46";
import {
  chooseLocalKey,
  deleteLocalKey,
  lockLocalKey,
  newSecretKey,
  parseSecretKey,
  restoreSigner,
  saveLocalKey,
  storedLocalNpub,
  unlockLocalKey,
} from "./signer";

const userSk = generateSecretKey();
const signerSk = generateSecretKey();
const signerPk = getPublicKey(signerSk);

/** In-memory relay with a fake remote signer that answers in `enc`. */
function fakePool(enc: Encryption) {
  const subs: { filter: { "#p"?: string[] }; onevent: (e: NostrEvent) => void }[] = [];
  const deliver = (ev: NostrEvent) => {
    const to = ev.tags.find((t) => t[0] === "p")?.[1];
    for (const s of subs) if (!s.filter["#p"] || s.filter["#p"].includes(to!)) s.onevent(ev);
  };
  const reply = (clientPk: string, body: object) => {
    const text = JSON.stringify(body);
    const content =
      enc === "nip04"
        ? nip04.encrypt(signerSk, clientPk, text)
        : nip44.encrypt(text, nip44.getConversationKey(signerSk, clientPk));
    deliver(finalizeEvent({ kind: 24133, created_at: 0, tags: [["p", clientPk]], content }, signerSk));
  };
  const pool = {
    subscribe(_relays: string[], filter: { "#p"?: string[] }, h: { onevent: (e: NostrEvent) => void }) {
      const s = { filter, onevent: h.onevent };
      subs.push(s);
      return { close: () => subs.splice(subs.indexOf(s), 1) };
    },
    publish(_relays: string[], ev: NostrEvent) {
      // The signer: decrypt the request (either scheme) and answer.
      const text = ev.content.includes("?iv=")
        ? nip04.decrypt(signerSk, ev.pubkey, ev.content)
        : nip44.decrypt(ev.content, nip44.getConversationKey(signerSk, ev.pubkey));
      const req = JSON.parse(text);
      queueMicrotask(() => {
        if (req.method === "get_public_key") reply(ev.pubkey, { id: req.id, result: getPublicKey(userSk) });
        if (req.method === "sign_event") {
          const signed = finalizeEvent(JSON.parse(req.params[0]) as EventTemplate, userSk);
          reply(ev.pubkey, { id: req.id, result: JSON.stringify(signed) });
        }
      });
      return [Promise.resolve("ok")];
    },
    close() {},
  };
  return { pool: pool as unknown as SimplePool, reply };
}

const template: EventTemplate = { kind: 27235, created_at: 1, tags: [], content: "" };

describe("Nip46Client", () => {
  for (const enc of ["nip44", "nip04"] as const) {
    it(`pairs and signs when the signer answers with ${enc}`, async () => {
      const { pool, reply } = fakePool(enc);
      const clientSk = generateSecretKey();
      const abort = new AbortController();
      const pairing = waitForNostrConnect(clientSk, [], "s3cret", abort.signal, pool);
      reply(getPublicKey(clientSk), { id: "x", result: "wrong" });
      reply(getPublicKey(clientSk), { id: "x", result: "s3cret" });
      const client = await pairing;
      expect(client.session.remote).toBe(signerPk);
      expect(client.session.enc).toBe(enc);
      expect(await client.getPublicKey()).toBe(getPublicKey(userSk));
      const signed = await client.signEvent(template);
      expect(verifyEvent(signed)).toBe(true);
    });
  }
});

describe("waitForNostrConnect", () => {
  it("rejects when cancelled", async () => {
    const abort = new AbortController();
    const p = waitForNostrConnect(generateSecretKey(), [], "s", abort.signal);
    abort.abort();
    await expect(p).rejects.toThrow("Pairing cancelled");
  });
});

describe("browser key", () => {
  afterEach(() => localStorage.clear());

  it("stores the key encrypted and needs the password after locking", async () => {
    const { sk, npub, nsec } = newSecretKey();
    expect(parseSecretKey(nsec)).toEqual(sk);
    expect(() => saveLocalKey(sk, "short")).toThrow("at least 8");
    saveLocalKey(sk, "correct horse");
    expect(storedLocalNpub()).toBe(npub);
    // Only the ncryptsec and the pubkey are stored, never the secret key itself.
    const raw = localStorage.getItem("allod.localkey")!;
    expect(raw).toContain("ncryptsec1");
    expect(raw).not.toContain(bytesToHex(sk));
    expect(raw).not.toContain(nsec);

    const signer = chooseLocalKey();
    expect(await signer.getPublicKey()).toBe(getPublicKey(sk));
    expect(verifyEvent(await signer.signEvent(template))).toBe(true);
    expect(restoreSigner()?.kind).toBe("local");

    lockLocalKey();
    expect(restoreSigner()).toBeNull();
    expect(() => chooseLocalKey()).toThrow("Unlock");
    expect(() => unlockLocalKey("wrong password")).toThrow("Wrong password");
    unlockLocalKey("correct horse");
    expect(await chooseLocalKey().getPublicKey()).toBe(getPublicKey(sk));

    deleteLocalKey();
    expect(storedLocalNpub()).toBeNull();
    expect(localStorage.getItem("allod.signer")).toBeNull();
  });

  it("imports an ncryptsec with its password", () => {
    const { sk } = newSecretKey();
    const enc = nip49.encrypt(sk, "pw-of-the-file", 8);
    expect(parseSecretKey(enc, "pw-of-the-file")).toEqual(sk);
    expect(() => parseSecretKey(enc, "nope")).toThrow("Wrong password");
  });

  it("rejects input that is not an nsec", () => {
    expect(() => parseSecretKey("npub1xyz")).toThrow("Not a valid nsec");
  });
});
