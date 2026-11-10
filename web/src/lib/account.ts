// Accounts available in wallet forms. "main" is the Main account.
import useSWR from "swr";
import { fetcher, type Account } from "./api";
import { useNodeReady } from "./health";

export type AccountId = "main" | number;

/** Accounts that are open (closed ones holding late payments stay manageable on Accounts). */
export function useAccounts() {
  // Accounts live in the wallet, which starts with the Lightning node (503 before).
  const ready = useNodeReady();
  const r = useSWR<{ items: Account[]; total: number }>(ready ? "/api/accounts" : null, fetcher, { refreshInterval: 30000 });
  return { ...r, ready, open: r.data?.items.filter((a) => a.closed_at == null) ?? [] };
}

/** `app_id` for API bodies: undefined for Main. */
export function appIdOf(id: AccountId): number | undefined {
  return id === "main" ? undefined : id;
}
