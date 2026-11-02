// The account chosen in the top bar's switcher (SPEC §13.2). Send pays from it, Receive
// receives into it and Activity shows it. "main" is the Main account.
import { useSyncExternalStore } from "react";
import useSWR from "swr";
import { fetcher, type Account } from "./api";

const KEY = "allod.account";
const listeners = new Set<() => void>();

export type AccountId = "main" | number;

function read(): AccountId {
  try {
    const v = localStorage.getItem(KEY);
    if (v && /^\d+$/.test(v)) return Number(v);
  } catch {
    // storage unavailable: Main
  }
  return "main";
}

export function selectAccount(id: AccountId) {
  try {
    localStorage.setItem(KEY, String(id));
  } catch {
    // keep it for this page only
  }
  listeners.forEach((l) => l());
}

function subscribe(l: () => void) {
  listeners.add(l);
  return () => listeners.delete(l);
}

/** Accounts that are open (closed ones holding late payments stay manageable on Accounts). */
export function useAccounts() {
  const r = useSWR<{ items: Account[]; total: number }>("/api/accounts", fetcher, { refreshInterval: 30000 });
  return { ...r, open: r.data?.items.filter((a) => a.closed_at == null) ?? [] };
}

/** The selected account, falling back to Main when it no longer exists. */
export function useSelectedAccount(): { id: AccountId; account: Account | null } {
  const id = useSyncExternalStore(subscribe, read, () => "main" as AccountId);
  const { open } = useAccounts();
  if (id === "main") return { id, account: null };
  const account = open.find((a) => a.id === id) ?? null;
  return account ? { id, account } : { id: "main", account: null };
}

/** `app_id` for API bodies: undefined for Main. */
export function appIdOf(id: AccountId): number | undefined {
  return id === "main" ? undefined : id;
}
