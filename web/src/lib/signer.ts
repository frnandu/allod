// Nostr signers for owner login (SPEC §11.1): NIP-07 browser extensions and NIP-46
// remote signers (bunker:// and nostrconnect://).
import type { EventTemplate, NostrEvent } from "nostr-tools";
import { generateSecretKey, getPublicKey } from "nostr-tools";
import { BunkerSigner, createNostrConnectURI, parseBunkerInput, type BunkerPointer } from "nostr-tools/nip46";
import { bytesToHex, hexToBytes } from "nostr-tools/utils";

declare global {
  interface Window {
    nostr?: {
      getPublicKey(): Promise<string>;
      signEvent(event: EventTemplate): Promise<NostrEvent>;
    };
  }
}

export type SignerKind = "extension" | "bunker";

export interface Signer {
  kind: SignerKind;
  label: string;
  getPublicKey(): Promise<string>;
  signEvent(template: EventTemplate): Promise<NostrEvent>;
  close?(): Promise<void>;
}

const STORAGE_KEY = "allod.signer";
/** Public relays used for nostrconnect:// pairing. The remote signer may switch relays. */
export const DEFAULT_CONNECT_RELAYS = ["wss://relay.nsec.app", "wss://relay.damus.io"];

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

function wrapBunker(client: BunkerSigner, label: string): Signer {
  return {
    kind: "bunker",
    label,
    getPublicKey: () => client.getPublicKey(),
    signEvent: (t) => client.signEvent(t),
    close: () => client.close(),
  };
}

function remember(choice: { kind: "extension" } | { kind: "bunker"; bp: BunkerPointer; sk: string }) {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(choice));
  } catch {
    /* storage unavailable: the user picks the signer again next time */
  }
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
  const sk = generateSecretKey();
  const client = BunkerSigner.fromBunker(sk, bp, { onauth: onAuthUrl });
  await client.connect();
  remember({ kind: "bunker", bp, sk: bytesToHex(sk) });
  return wrapBunker(client, "Remote signer");
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
  const secret = bytesToHex(generateSecretKey()).slice(0, 32);
  const uri = createNostrConnectURI({
    clientPubkey: getPublicKey(sk),
    relays,
    secret,
    name: "Allod",
    perms: ["sign_event:27235"],
  });
  const abort = new AbortController();
  const signer = BunkerSigner.fromURI(sk, uri, {}, abort.signal).then((client) => {
    remember({ kind: "bunker", bp: client.bp, sk: bytesToHex(sk) });
    return wrapBunker(client, "Remote signer");
  });
  return { uri, signer, cancel: () => abort.abort() };
}

export function chooseExtension(): Signer {
  const s = extensionSigner();
  remember({ kind: "extension" });
  return s;
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
    const saved = JSON.parse(raw);
    if (saved.kind === "extension") return hasExtension() ? extensionSigner() : null;
    if (saved.kind === "bunker") {
      return wrapBunker(BunkerSigner.fromBunker(hexToBytes(saved.sk), saved.bp), "Remote signer");
    }
  } catch {
    forgetSigner();
  }
  return null;
}
