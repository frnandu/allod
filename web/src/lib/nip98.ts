// NIP-98 HTTP auth headers (SPEC §11.1).
import { sha256 } from "@noble/hashes/sha2.js";
import type { Signer } from "./signer";

async function sha256Hex(text: string): Promise<string> {
  const digest = sha256(new TextEncoder().encode(text));
  return Array.from(new Uint8Array(digest), (b) => b.toString(16).padStart(2, "0")).join("");
}

function base64Utf8(text: string): string {
  const bytes = new TextEncoder().encode(text);
  let binary = "";
  bytes.forEach((b) => (binary += String.fromCharCode(b)));
  return btoa(binary);
}

/** Builds `Authorization: Nostr <base64(event)>` for one request. */
export async function nip98Header(
  signer: Signer,
  url: string,
  method: string,
  body = "",
): Promise<string> {
  const tags = [
    ["u", url],
    ["method", method.toUpperCase()],
  ];
  if (body) tags.push(["payload", await sha256Hex(body)]);
  const event = await signer.signEvent({
    kind: 27235,
    created_at: Math.floor(Date.now() / 1000),
    tags,
    content: "",
  });
  return `Nostr ${base64Utf8(JSON.stringify(event))}`;
}

export const _test = { sha256Hex, base64Utf8 };
