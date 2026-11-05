import useSWR from "swr";
import { fetcher, type HealthResponse } from "./api";

/**
 * Whether the Lightning node answers (`GET /api/health` is "ok"). Wallet routes
 * (accounts, live events) answer 503 until then, so callers wait for it instead of
 * polling into errors while Bitcoin syncs.
 */
export function useNodeReady(): boolean {
  const health = useSWR<HealthResponse>("/api/health", fetcher, { refreshInterval: 15000 });
  return health.data?.status === "ok";
}
