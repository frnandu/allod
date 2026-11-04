// Nostr signers for owner login (SPEC §11.1): NIP-07 browser extensions, NIP-46 remote
// signers (bunker:// and nostrconnect://), and a key kept in this browser.
import type { EventTemplate, NostrEvent } from "nostr-tools";
import { nip19 } from "nostr-tools";
import { parseBunkerInput } from "nostr-tools/nip46";
import * as nip49 from "nostr-tools/nip49";
import { finalizeEvent, generateSecretKey, getPublicKey } from "nostr-tools/pure";
import { bytesToHex, hexToBytes } from "nostr-tools/utils";
import { Nip46Client, waitForNostrConnect, type RemoteSession } from "./nip46";

declare global {
  interface Window {
    nostr?: {
      getPublicKey(): Promise<string>;
      signEvent(event: EventTemplate): Promise<NostrEvent>;
    };
  }
}

export type SignerKind = "extension" | "bunker" | "local";

export interface Signer {
  kind: SignerKind;
  label: string;
  getPublicKey(): Promise<string>;
  signEvent(template: EventTemplate): Promise<NostrEvent>;
  close?(): Promise<void>;
}

const STORAGE_KEY = "allod.signer";
/** The browser-held secret key. Kept apart from the signer choice, so switching signer keeps it. */
const LOCAL_KEY = "allod.localkey";
/**
 * Public relays used for nostrconnect:// pairing. Two, to keep the QR code easy to scan;
 * the signer needs to reach one of them. (relay.nsec.app was dropped: it timed out.)
 */
export const DEFAULT_CONNECT_RELAYS = ["wss://relay.damus.io", "wss://relay.primal.net"];

type Saved = { kind: "extension" } | { kind: "local" } | ({ kind: "bunker"; sk: string } & RemoteSession);

export function hasExtension(): boolean {
  return typeof window !== "undefined" && !!window.nostr;
}

export function extensionSigner(): Signer {
  const ext = window.nostr;
  if (!ext) throw new Error("No Nostr browser extension found");
  return {
    kind: "extension",
    label: "Browser extension",
    getPublicKey: () => ext.getPublicKey(),
    signEvent: (t) => ext.signEvent(t),
  };
}

function wrapRemote(client: Nip46Client): Signer {
  return {
    kind: "bunker",
    label: "Remote signer",
    getPublicKey: () => client.getPublicKey(),
    signEvent: (t) => client.signEvent(t),
    close: () => client.close(),
  };
}

function remember(choice: Saved) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(choice));
  } catch {
    /* storage unavailable: the user picks the signer again next time */
  }
}

function rememberRemote(sk: Uint8Array, client: Nip46Client) {
  remember({ kind: "bunker", sk: bytesToHex(sk), ...client.session });
}

export function forgetSigner() {
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    /* ignore */
  }
}

/** Connects to a remote signer from a bunker:// URL or NIP-05 address. */
export async function bunkerSigner(input: string, onAuthUrl?: (url: string) => void): Promise<Signer> {
  const bp = await parseBunkerInput(input.trim());
  if (!bp) throw new Error("Not a bunker:// URL or a NIP-05 address with a bunker");
  if (bp.relays.length === 0) throw new Error("The bunker link names no relay");
  const sk = generateSecretKey();
  const client = new Nip46Client(sk, { remote: bp.pubkey, relays: bp.relays, enc: "nip44" });
  client.onAuthUrl = onAuthUrl;
  await client.connect(bp.secret ?? undefined);
  rememberRemote(sk, client);
  return wrapRemote(client);
}

/**
 * Starts a nostrconnect:// pairing: show `uri` (as text or QR) to the user, who opens
 * it in their signer app. `signer` resolves once the app has connected.
 */
export function startNostrConnect(relays = DEFAULT_CONNECT_RELAYS): {
  uri: string;
  signer: Promise<Signer>;
  cancel: () => void;
} {
  const sk = generateSecretKey();
  const secret = bytesToHex(generateSecretKey()).slice(0, 16);
  const q = new URLSearchParams();
  for (const r of relays) q.append("relay", r);
  q.set("secret", secret);
  q.set("perms", "sign_event:27235");
  q.set("name", "Allod");
  const uri = `nostrconnect://${getPublicKey(sk)}?${q.toString()}`;
  const abort = new AbortController();
  const signer = waitForNostrConnect(sk, relays, secret, abort.signal).then((client) => {
    rememberRemote(sk, client);
    return wrapRemote(client);
  });
  return { uri, signer, cancel: () => abort.abort() };
}

export function chooseExtension(): Signer {
  const s = extensionSigner();
  remember({ kind: "extension" });
  return s;
}

// --- Key kept in this browser ---------------------------------------------------------
//
// Stored only as a NIP-49 ncryptsec (scrypt + XChaCha20-Poly1305) under a password the
// user picks. The decrypted key lives in memory: a reload, a reboot or signing out asks
// for the password again.

interface StoredKey {
  ncryptsec: string;
  pubkey: string;
}

export const MIN_PASSWORD_LENGTH = 8;
let unlocked: Uint8Array | null = null;

function readStoredKey(): StoredKey | null {
  try {
    const v = JSON.parse(localStorage.getItem(LOCAL_KEY) ?? "null");
    return v && typeof v.ncryptsec === "string" && /^[0-9a-f]{64}$/.test(v.pubkey) ? v : null;
  } catch {
    return null;
  }
}

/** The npub of the key stored in this browser, if any. */
export function storedLocalNpub(): string | null {
  const k = readStoredKey();
  return k ? nip19.npubEncode(k.pubkey) : null;
}

/** Parses an nsec (or 64-char hex) secret key, or an ncryptsec with its password. */
export function parseSecretKey(input: string, password = ""): Uint8Array {
  const s = input.trim();
  if (s.startsWith("ncryptsec1")) {
    try {
      return nip49.decrypt(s, password);
    } catch {
      throw new Error("Wrong password for this ncryptsec");
    }
  }
  if (/^[0-9a-fA-F]{64}$/.test(s)) return hexToBytes(s.toLowerCase());
  try {
    const d = nip19.decode(s);
    if (d.type === "nsec") return d.data;
  } catch {
    /* fall through */
  }
  throw new Error("Not a valid nsec");
}

export function newSecretKey(): { sk: Uint8Array; nsec: string; npub: string } {
  const sk = generateSecretKey();
  return { sk, nsec: nip19.nsecEncode(sk), npub: nip19.npubEncode(getPublicKey(sk)) };
}

/** Encrypts `sk` with `password` and stores it in this browser's localStorage. */
export function saveLocalKey(sk: Uint8Array, password: string) {
  if (password.length < MIN_PASSWORD_LENGTH) throw new Error(`Use a password of at least ${MIN_PASSWORD_LENGTH} characters`);
  const stored: StoredKey = { ncryptsec: nip49.encrypt(sk, password), pubkey: getPublicKey(sk) };
  try {
    localStorage.setItem(LOCAL_KEY, JSON.stringify(stored));
  } catch {
    throw new Error("This browser does not allow storing the key (private window?)");
  }
  unlocked = sk;
}

/** Decrypts the stored key into memory. */
export function unlockLocalKey(password: string) {
  const k = readStoredKey();
  if (!k) throw new Error("No key is stored in this browser");
  try {
    unlocked = nip49.decrypt(k.ncryptsec, password);
  } catch {
    throw new Error("Wrong password");
  }
}

/** Forgets the decrypted key; the stored, encrypted copy stays. */
export function lockLocalKey() {
  unlocked = null;
}

export function deleteLocalKey() {
  unlocked = null;
  try {
    localStorage.removeItem(LOCAL_KEY);
    if (JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null")?.kind === "local") forgetSigner();
  } catch {
    /* ignore */
  }
}

function localSigner(sk: Uint8Array): Signer {
  const pubkey = getPublicKey(sk);
  return {
    kind: "local",
    label: "Key in this browser",
    getPublicKey: async () => pubkey,
    signEvent: async (t) => finalizeEvent(t, sk),
  };
}

/** Signs with the key stored in this browser; unlock it first. */
export function chooseLocalKey(): Signer {
  if (!unlocked) throw new Error("Unlock the key with its password first");
  remember({ kind: "local" });
  return localSigner(unlocked);
}

/** Restores the signer chosen last time, if any. */
export function restoreSigner(): Signer | null {
  let raw: string | null;
  try {
    raw = localStorage.getItem(STORAGE_KEY);
  } catch {
    return null;
  }
  if (!raw) return null;
  try {
    const saved = JSON.parse(raw) as Saved;
    if (saved.kind === "extension") return hasExtension() ? extensionSigner() : null;
    // Locked (after a reload or reboot): the user enters the password again.
    if (saved.kind === "local") return unlocked ? localSigner(unlocked) : null;
    if (saved.kind === "bunker" && saved.remote) {
      const { sk, ...session } = saved;
      return wrapRemote(new Nip46Client(hexToBytes(sk), session));
    }
    // Older saved form (nostr-tools BunkerSigner pointer): pair again.
    forgetSigner();
  } catch {
    forgetSigner();
  }
  return null;
}
