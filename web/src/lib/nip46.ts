// Minimal NIP-46 client (remote signers: bunker:// and nostrconnect://).
//
// nostr-tools' BunkerSigner only reads NIP-44 replies. Some signers (Amber among them,
// depending on version) answer with NIP-04, and the pairing then hangs silently. This
// client reads both and answers each signer in the scheme it last used.
import type { EventTemplate, NostrEvent } from "nostr-tools";
import * as nip04 from "nostr-tools/nip04";
import * as nip44 from "nostr-tools/nip44";
import { SimplePool } from "nostr-tools/pool";
import { finalizeEvent, getPublicKey, verifyEvent } from "nostr-tools/pure";

const KIND_NOSTR_CONNECT = 24133;
const REQUEST_TIMEOUT_MS = 120_000;

export type Encryption = "nip44" | "nip04";

/** What a later session needs to talk to the same signer again. */
export interface RemoteSession {
  /** The remote signer's pubkey (not necessarily the user's key). */
  remote: string;
  relays: string[];
  enc: Encryption;
}

function detect(content: string): Encryption {
  return content.includes("?iv=") ? "nip04" : "nip44";
}

function decryptAny(sk: Uint8Array, pubkey: string, content: string): { text: string; enc: Encryption } {
  const enc = detect(content);
  const text =
    enc === "nip04" ? nip04.decrypt(sk, pubkey, content) : nip44.decrypt(content, nip44.getConversationKey(sk, pubkey));
  return { text, enc };
}

function encryptWith(enc: Encryption, sk: Uint8Array, pubkey: string, text: string): string {
  return enc === "nip04" ? nip04.encrypt(sk, pubkey, text) : nip44.encrypt(text, nip44.getConversationKey(sk, pubkey));
}

interface Pending {
  resolve: (result: string) => void;
  reject: (e: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

export class Nip46Client {
  readonly session: RemoteSession;
  private readonly sk: Uint8Array;
  private readonly pool: SimplePool;
  private readonly pending = new Map<string, Pending>();
  private sub: { close: () => void } | null = null;
  private serial = 0;
  private userPubkey: string | null = null;
  onAuthUrl?: (url: string) => void;

  constructor(sk: Uint8Array, session: RemoteSession, pool = new SimplePool()) {
    this.sk = sk;
    this.session = { ...session };
    this.pool = pool;
  }

  private ensureSubscribed() {
    if (this.sub) return;
    this.sub = this.pool.subscribe(
      this.session.relays,
      { kinds: [KIND_NOSTR_CONNECT], authors: [this.session.remote], "#p": [getPublicKey(this.sk)], limit: 0 },
      { onevent: (ev) => this.onEvent(ev) },
    );
  }

  private onEvent(ev: NostrEvent) {
    let msg: { id?: string; result?: string; error?: string };
    try {
      const { text, enc } = decryptAny(this.sk, ev.pubkey, ev.content);
      this.session.enc = enc;
      msg = JSON.parse(text);
    } catch (e) {
      console.warn("nip46: unreadable reply from signer", e);
      return;
    }
    const p = msg.id ? this.pending.get(msg.id) : undefined;
    if (!p) return;
    if (msg.result === "auth_url") {
      // The signer wants the user to approve on a web page; the real reply follows.
      if (msg.error) this.onAuthUrl?.(msg.error);
      return;
    }
    clearTimeout(p.timer);
    this.pending.delete(msg.id!);
    if (msg.error) p.reject(new Error(`Signer refused: ${msg.error}`));
    else p.resolve(msg.result ?? "");
  }

  async request(method: string, params: string[]): Promise<string> {
    this.ensureSubscribed();
    const id = `allod-${Date.now().toString(36)}-${++this.serial}`;
    const content = encryptWith(this.session.enc, this.sk, this.session.remote, JSON.stringify({ id, method, params }));
    const event = finalizeEvent(
      { kind: KIND_NOSTR_CONNECT, created_at: Math.floor(Date.now() / 1000), tags: [["p", this.session.remote]], content },
      this.sk,
    );
    const reply = new Promise<string>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error("The signer did not answer. Check that the signer app is open and online."));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, { resolve, reject, timer });
    });
    const sent = await Promise.allSettled(this.pool.publish(this.session.relays, event));
    if (!sent.some((r) => r.status === "fulfilled")) {
      const p = this.pending.get(id);
      if (p) {
        clearTimeout(p.timer);
        this.pending.delete(id);
      }
      throw new Error(`Could not reach the signer's relays (${this.session.relays.join(", ")})`);
    }
    return reply;
  }

  async connect(secret?: string, perms = "sign_event:27235") {
    const result = await this.request("connect", [this.session.remote, secret ?? "", perms]);
    if (secret && result !== "ack" && result !== secret) throw new Error("The signer answered with a wrong secret");
  }

  async getPublicKey(): Promise<string> {
    if (!this.userPubkey) this.userPubkey = await this.request("get_public_key", []);
    return this.userPubkey;
  }

  async signEvent(template: EventTemplate): Promise<NostrEvent> {
    const signed = JSON.parse(await this.request("sign_event", [JSON.stringify(template)])) as NostrEvent;
    if (!verifyEvent(signed)) throw new Error("The signer returned an invalid signature");
    return signed;
  }

  async close() {
    for (const p of this.pending.values()) {
      clearTimeout(p.timer);
      p.reject(new Error("closed"));
    }
    this.pending.clear();
    this.sub?.close();
    this.sub = null;
    this.pool.close(this.session.relays);
  }
}

/**
 * Waits for a signer to answer a nostrconnect:// URI. The signer's first message must
 * carry the URI's secret (NIP-46); that is what proves it is the app the user scanned with.
 */
export function waitForNostrConnect(
  sk: Uint8Array,
  relays: string[],
  secret: string,
  signal: AbortSignal,
  pool = new SimplePool(),
): Promise<Nip46Client> {
  return new Promise((resolve, reject) => {
    let done = false;
    const sub = pool.subscribe(
      relays,
      { kinds: [KIND_NOSTR_CONNECT], "#p": [getPublicKey(sk)], limit: 0 },
      {
        onevent: (ev) => {
          if (done) return;
          try {
            const { text, enc } = decryptAny(sk, ev.pubkey, ev.content);
            const msg = JSON.parse(text);
            if (msg.result !== secret) {
              console.warn("nip46: pairing reply without the expected secret", msg.result);
              return;
            }
            done = true;
            sub.close();
            resolve(new Nip46Client(sk, { remote: ev.pubkey, relays, enc }, pool));
          } catch (e) {
            console.warn("nip46: unreadable pairing reply", e);
          }
        },
        onclose: (reasons) => {
          if (!done) reject(new Error(`Lost the connection to the pairing relays: ${reasons.join(", ")}`));
        },
      },
    );
    signal.addEventListener("abort", () => {
      if (done) return;
      done = true;
      sub.close();
      pool.close(relays);
      reject(new Error("Pairing cancelled"));
    });
  });
}
