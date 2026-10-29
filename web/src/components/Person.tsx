import { nip19 } from "nostr-tools";
import useSWR from "swr";
import { fetcher, type Counterparty, type Profile } from "../lib/api";
import { shorten } from "../lib/utils";
import { Sigil } from "./Sigil";

/** The Nostr profile of a pubkey (cached by the server and by SWR). */
export function useProfile(pubkey?: string): Profile | undefined {
  const { data } = useSWR<Profile[]>(pubkey ? `/api/profiles?pubkeys=${pubkey}` : null, fetcher, {
    revalidateOnFocus: false,
    dedupingInterval: 600_000,
  });
  return data?.[0];
}

export function npub(pubkey: string): string {
  try {
    return nip19.npubEncode(pubkey);
  } catch {
    return pubkey;
  }
}

/** Best name for a counterparty: what the payment says, the Nostr profile, the address. */
export function personLabel(cp?: Counterparty, profile?: Profile): string | null {
  if (!cp) return null;
  return cp.name ?? profile?.name ?? cp.identifier ?? (cp.pubkey ? shorten(npub(cp.pubkey), 10, 4) : null);
}

export function Avatar({ pubkey, picture, size = 36 }: { pubkey?: string; picture?: string; size?: number }) {
  if (picture) {
    return (
      <img
        src={picture}
        alt=""
        width={size}
        height={size}
        loading="lazy"
        referrerPolicy="no-referrer"
        className="shrink-0 rounded-full object-cover"
        style={{ width: size, height: size }}
      />
    );
  }
  return <Sigil seed={pubkey ?? "anon"} size={size} />;
}
