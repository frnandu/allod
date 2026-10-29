import useSWR from "swr";
import { fetcher } from "../lib/api";
import { fiat, sats } from "../lib/format";
import { cn } from "../lib/utils";

export function useRate() {
  return useSWR<{ currency: string; btc_price?: number }>("/api/fiat/rate", fetcher, {
    refreshInterval: 60_000,
    shouldRetryOnError: false,
  }).data;
}

/** Sats amount with optional fiat value. */
export function Amount({
  msat,
  sign,
  className,
  big,
}: {
  msat: number | null | undefined;
  sign?: "+" | "-";
  className?: string;
  big?: boolean;
}) {
  const rate = useRate();
  const f = fiat(msat, rate);
  return (
    <span className={cn("inline-flex flex-col", className)}>
      <span className={cn("font-mono tabular-nums", big ? "text-4xl font-medium tracking-tight" : "")}>
        {sign}
        {sats(msat)} <span className={cn("font-sans text-muted", big ? "text-base" : "text-xs")}>sats</span>
      </span>
      {f && <span className={cn("text-muted", big ? "text-sm" : "text-xs")}>≈ {f}</span>}
    </span>
  );
}
