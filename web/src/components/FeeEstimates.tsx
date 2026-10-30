import useSWR from "swr";
import { fetcher, type FeeEstimates } from "../lib/api";
import { cn } from "../lib/utils";
import { Alert } from "./ui/alert";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

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

export function FeeEstimatesCard() {
  const { data: fees } = useFeeEstimates();
  // Light modes have no bitcoind of Allod's own; there is nothing to show.
  if (!fees?.source) return null;
  const rate = (blocks: number) => fees.targets.find((t) => t.blocks === blocks)?.sat_per_vb ?? null;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Fee estimates</CardTitle>
        <p className="text-xs text-muted">Bitcoin Core's estimate of the fee rate to confirm within…</p>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <div className="grid grid-cols-2 gap-x-4 gap-y-3 sm:grid-cols-3">
          {TARGETS.map((t) => {
            const v = rate(t.blocks);
            return (
              <div key={t.blocks}>
                <p className="text-muted">{t.label}</p>
                <p className={cn("font-mono", v == null ? "text-warn" : "text-fg")}>
                  {v == null ? "no estimate" : `${feeRate(v)} sat/vB`}
                </p>
              </div>
            );
          })}
        </div>
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
      </CardContent>
    </Card>
  );
}
