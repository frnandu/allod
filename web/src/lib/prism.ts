import useSWR from "swr";
import { fetcher, PRISM_FAILURES_KEY, type PrismFailures } from "./api";

/** Failed split payouts across every address, for the warning shown in the menus. */
export function usePrismFailures() {
  const { data } = useSWR<PrismFailures>(PRISM_FAILURES_KEY, fetcher, {
    refreshInterval: 30_000,
  });
  return { failedCount: data?.failed_count ?? 0, failedMsat: data?.failed_msat ?? 0 };
}

/** The accessible label of a failed-payout warning. */
export function prismFailureLabel(count: number): string {
  return `${count} prism ${count === 1 ? "payout" : "payouts"} could not be sent`;
}
