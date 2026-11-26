// Sending zaps: NIP-57 (Lightning address / LNURL) and NIP-B1 (BOLT12 offer).
import type { EventTemplate, NostrEvent } from "nostr-tools";
import { api } from "./api";
import { restoreSigner } from "./signer";

/** BOLT12 payer note of a NIP-B1 zap. */
export const BOLT12_ZAP_NOTE = "nostr:nipB1:";

async function sign(template: EventTemplate): Promise<NostrEvent> {
  const signer = restoreSigner();
  if (!signer) throw new Error("Your Nostr signer is not connected or is locked. Sign in again, or zap anonymously.");
  return signer.signEvent(template);
}

/** The signed zap request (or intent) to send along with the payment. */
export async function prepareZap(req: {
  pubkey: string;
  payment: string;
  amount_msat: number;
  comment: string;
  anonymous: boolean;
}): Promise<NostrEvent> {
  const res = await api.post<{ template?: EventTemplate; event?: NostrEvent }>("/api/zaps", req);
  return res.event ?? (await sign(res.template!));
}

export interface ZapReceipt {
  status: "none" | "waiting" | "no_proof" | "sign" | "not_accepted" | "published";
  template?: EventTemplate;
}

/** Publishes the zap of a paid BOLT12 zap, signing it when it is a public one. */
export async function publishBolt12Zap(transaction: string): Promise<ZapReceipt> {
  const path = `/api/zaps/${encodeURIComponent(transaction)}/receipt`;
  const res = await api.post<ZapReceipt>(path, {});
  if (res.status !== "sign") return res;
  return api.post<ZapReceipt>(path, { event: await sign(res.template!) });
}
