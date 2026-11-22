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

/** Priority names for the targets, as block explorers label their fee boxes. */
const PRIORITY: Record<number, string> = {
  2: "High priority",
  6: "Medium priority",
  12: "Low priority",
  144: "Economy",
  1008: "Cheapest",
};

/** One fee box: the priority, the rate, and how long it takes to confirm. The fastest
 * boxes carry the strongest accent; the floor is the cheapest a transaction can pay and
 * still be relayed. */
function FeeTile({ label, sub, rate, tone }: { label: string; sub: string; rate: number | null; tone: number | "floor" }) {
  const floor = tone === "floor";
  return (
    <li
      className={cn(
        "relative overflow-hidden rounded-lg border px-3 pb-2.5 pt-3 text-center",
        floor ? "border-dashed border-line" : "border-line bg-surface",
        rate == null && "border-dashed border-warn/50",
      )}
    >
      {!floor && rate != null && (
        <span aria-hidden className="absolute inset-x-0 top-0 h-1 bg-accent" style={{ opacity: tone }} />
      )}
      <p className={cn("text-xs font-medium", floor ? "text-muted" : "text-fg")}>{label}</p>
      <p className="pt-1.5 leading-none">
        {rate == null ? (
          <span className="text-sm text-warn">no estimate</span>
        ) : (
          <>
            <span className={cn("font-mono text-xl tabular-nums", floor && "text-muted")}>{feeRate(rate)}</span>{" "}
            <span className="text-[11px] text-muted">sat/vB</span>
          </>
        )}
      </p>
      <p className="pt-1.5 text-[11px] text-muted">{sub}</p>
    </li>
  );
}

/** Bitcoin Core's fee estimates as fee boxes, cheapest on the left and fastest on the
 * right, starting from the mempool's entry floor when known (below it a transaction is
 * not even relayed). */
export function FeeEstimatesSection({ fees, floor }: { fees: FeeEstimates; floor?: number | null }) {
  const rate = (blocks: number) => fees.targets.find((t) => t.blocks === blocks)?.sat_per_vb ?? null;
  const cheapFirst = [...TARGETS].reverse();
  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-end justify-between gap-x-4 gap-y-1">
        <div>
          <h3 className="text-xs font-medium uppercase tracking-wider text-muted">Fee estimates</h3>
          <p className="text-xs text-muted">Bitcoin Core's fee rate to confirm within the time shown</p>
        </div>
        <p aria-hidden className="flex items-center gap-1.5 text-[11px] text-muted">
          cheaper
          <span className="h-1 w-16 rounded-full bg-linear-to-r from-accent/10 to-accent" />
          faster
        </p>
      </div>
      <ul className="grid grid-cols-2 gap-2 sm:grid-cols-3 xl:grid-cols-6">
        {floor != null && <FeeTile label="No priority" sub="mempool minimum" rate={floor} tone="floor" />}
        {cheapFirst.map((t, i) => (
          <FeeTile
            key={t.blocks}
            label={PRIORITY[t.blocks] ?? t.label}
            sub={`${t.label} · ${t.blocks} blocks`}
            rate={rate(t.blocks)}
            tone={0.15 + (i / (cheapFirst.length - 1)) * 0.85}
          />
        ))}
      </ul>
      {fees.error ? (
        <Alert tone="danger" title="The Bitcoin node cannot be asked for fee rates">
          {fees.error}
        </Alert>
      ) : fees.missing.length > 0 ? (
        <Alert tone="warn" title="Fee estimates are still missing">
          Bitcoin Core learns them from transactions it sees enter its mempool and confirm, so a
          node that synced from a snapshot starts without any. On mainnet LDK Server waits for
          every target above before it starts: prices estimated from nothing would misprice
          force-closes and sweeps.
        </Alert>
      ) : null}
    </div>
  );
}
