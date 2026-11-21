import useSWR from "swr";
import { fetcher, type FeeEstimates } from "../lib/api";
import { cn } from "../lib/utils";
import { Alert } from "./ui/alert";

/** Blocks are ~10 minutes; these are the targets ldk-node asks bitcoind for. */
const TARGETS: { blocks: number; label: string }[] = [
  { blocks: 2, label: "~20 min" },
  { blocks: 6, label: "~1 hour" },
  { blocks: 12, label: "~2 hours" },
  { blocks: 144, label: "~1 day" },
  { blocks: 1008, label: "~1 week" },
];

export function useFeeEstimates() {
  // Estimates move block by block, not request by request: a slow poll is enough.
  return useSWR<FeeEstimates>("/api/fees", fetcher, { refreshInterval: 60000 });
}

/** sat/vB at the precision the estimator actually resolves. */
export function feeRate(satPerVb: number): string {
  if (satPerVb >= 10) return satPerVb.toFixed(0);
  return satPerVb >= 1 ? satPerVb.toFixed(1) : satPerVb.toFixed(2);
}

/** One line for status lists, or nothing when there is no bitcoind to ask. */
export function feeEstimateSummary(fees?: FeeEstimates): { done: boolean; value: string; warn?: boolean } | undefined {
  if (!fees?.source) return undefined;
  if (fees.error) return { done: false, value: "Bitcoin node unreachable", warn: true };
  if (fees.missing.length > 0) {
    return { done: false, value: `no estimate for ${fees.missing.join(", ")} blocks`, warn: true };
  }
  return { done: true, value: `${fees.targets.length} targets available` };
}

/** One rung of the fee ladder: the bar's length is the rate against the highest rung. */
function Rung({ label, sub, rate, max, floor }: { label: string; sub?: string; rate: number | null; max: number; floor?: boolean }) {
  const width = rate == null || max <= 0 ? 0 : Math.max(3, (rate / max) * 100);
  return (
    <li className="grid grid-cols-[5.5rem_minmax(0,1fr)_6.5rem] items-center gap-3">
      <div className="leading-tight">
        <p className={cn("text-sm", floor ? "text-muted" : "text-fg")}>{label}</p>
        {sub && <p className="text-[11px] text-muted">{sub}</p>}
      </div>
      <div className="h-2.5 overflow-hidden rounded-full bg-surface-2">
        {rate == null ? (
          <div className="h-full w-full rounded-full border border-dashed border-warn/60" />
        ) : (
          <div
            className={cn(
              "h-full rounded-full transition-[width] duration-500",
              floor ? "bg-muted/40" : "bg-linear-to-r from-accent/50 to-accent",
            )}
            style={{ width: `${width}%` }}
          />
        )}
      </div>
      <p className={cn("text-right font-mono text-sm tabular-nums", rate == null ? "text-warn" : floor ? "text-muted" : "text-fg")}>
        {rate == null ? "no estimate" : (
          <>
            {feeRate(rate)} <span className="text-[11px] text-muted">sat/vB</span>
          </>
        )}
      </p>
    </li>
  );
}

/** The fee ladder (fastest first) for the Bitcoin node card, with the mempool's entry
 * floor as the bottom rung when known: below it a transaction is not even relayed. */
export function FeeEstimatesSection({ fees, floor }: { fees: FeeEstimates; floor?: number | null }) {
  const rate = (blocks: number) => fees.targets.find((t) => t.blocks === blocks)?.sat_per_vb ?? null;
  const rates = TARGETS.map((t) => rate(t.blocks));
  const max = Math.max(0, floor ?? 0, ...rates.map((r) => r ?? 0));
  return (
    <div className="space-y-3">
      <div>
        <h3 className="text-xs font-medium uppercase tracking-wider text-muted">Fee estimates</h3>
        <p className="text-xs text-muted">Bitcoin Core's fee rate to confirm within…</p>
      </div>
      <ul className="space-y-2.5">
        {TARGETS.map((t, i) => (
          <Rung key={t.blocks} label={t.label} sub={`${t.blocks} blocks`} rate={rates[i] ?? null} max={max} />
        ))}
        {floor != null && <Rung label="Floor" sub="mempool min." rate={floor} max={max} floor />}
      </ul>
      {fees.error ? (
        <Alert tone="danger" title="The Bitcoin node cannot be asked for fee rates">
          {fees.error}
        </Alert>
      ) : fees.missing.length > 0 ? (
        <Alert tone="warn" title="Fee estimates are still missing">
          Bitcoin Core learns them from transactions it sees enter its mempool and confirm, so a
          node that synced from a snapshot starts without any. On mainnet ldk-server waits for
          every target above before it starts: prices estimated from nothing would misprice
          force-closes and sweeps.
        </Alert>
      ) : null}
    </div>
  );
}
