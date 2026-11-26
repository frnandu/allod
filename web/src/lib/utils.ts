import { clsx, type ClassValue } from "clsx";
import { twMerge } from "tailwind-merge";

export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs));
}

export function shorten(value: string, head = 8, tail = 6): string {
  return value.length <= head + tail + 1 ? value : `${value.slice(0, head)}…${value.slice(-tail)}`;
}

/** mempool.space's transaction page on the node's network; none on regtest. */
export function txUrl(network: string | undefined, txid: string): string | null {
  const prefix: Record<string, string> = { bitcoin: "", signet: "signet/", testnet4: "testnet4/", testnet: "testnet/" };
  const p = network ? prefix[network] : undefined;
  return p == null ? null : `https://mempool.space/${p}tx/${txid}`;
}
