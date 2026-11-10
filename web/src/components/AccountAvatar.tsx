import useSWR from "swr";
import { fetcher, type Nip05Name } from "../lib/api";
import { useSession } from "../lib/session";
import { Avatar, useProfile } from "./Person";

/** Account's linked NIP-05 profile picture; Main uses signed-in owner's profile. */
export function AccountAvatar({ accountId, seed, size = 36 }: { accountId: number | null; seed: string; size?: number }) {
  const session = useSession();
  const { data } = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher, { revalidateOnFocus: false });
  const pubkey = accountId === null ? session?.pubkey : data?.names.find((name) => name.account_id === accountId)?.pubkey;
  const profile = useProfile(pubkey);
  return <Avatar pubkey={pubkey ?? seed} picture={profile?.picture} size={size} />;
}
