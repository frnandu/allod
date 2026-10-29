// Live updates over SSE (SPEC §11.3): a ticket from POST /api/events/ticket, then
// EventSource on /api/events. Every event revalidates the wallet data in SWR.
import { useEffect } from "react";
import { mutate } from "swr";
import { api } from "./api";
import { useSession } from "./session";

const WALLET_KEYS = (key: unknown) =>
  typeof key === "string" &&
  (key.startsWith("/api/transactions") || key.startsWith("/api/balances") || key.startsWith("/api/offers") || key.startsWith("/api/channels"));

export function useLiveEvents() {
  const session = useSession();
  useEffect(() => {
    if (!session || typeof EventSource === "undefined") return;
    let source: EventSource | null = null;
    let stopped = false;
    let retry: ReturnType<typeof setTimeout> | undefined;

    const connect = async () => {
      try {
        const { ticket } = await api.post<{ ticket: string }>("/api/events/ticket");
        if (stopped) return;
        source = new EventSource(`/api/events?ticket=${ticket}`);
        const refresh = () => void mutate(WALLET_KEYS);
        source.addEventListener("transaction.updated", refresh);
        source.addEventListener("balance.changed", refresh);
        source.onerror = () => {
          source?.close();
          if (!stopped) retry = setTimeout(connect, 5000);
        };
      } catch {
        if (!stopped) retry = setTimeout(connect, 10000);
      }
    };
    void connect();
    return () => {
      stopped = true;
      clearTimeout(retry);
      source?.close();
    };
  }, [session]);
}
