import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import type { GossipStatus } from "../lib/api";

const labels = {
  waiting: "Waiting",
  learning: "Learning",
  active: "Active",
  degraded: "Degraded",
} as const;

function bytes(value: number) {
  if (value < 1024 * 1024) return `${Math.round(value / 1024).toLocaleString()} KiB`;
  if (value < 1024 * 1024 * 1024) return `${(value / 1024 / 1024).toFixed(1)} MiB`;
  return `${(value / 1024 / 1024 / 1024).toFixed(1)} GiB`;
}

export function GossipStatusCard({ status }: { status: GossipStatus }) {
  const proxy = status.proxy;
  const tone = status.state === "degraded" ? "danger" : status.state === "active" ? "ok" : "accent";
  return (
    <Card className="lg:col-span-2">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div>
          <CardTitle>Network graph</CardTitle>
          <p className="pt-1 text-xs text-muted">P2P gossip · no third-party snapshot</p>
        </div>
        <Badge tone={tone}>{labels[status.state]}</Badge>
      </CardHeader>
      <CardContent className="space-y-4">
        {status.state === "learning" && (
          <div className="h-1.5 overflow-hidden rounded-full bg-surface-2" aria-label="Learning network graph">
            <div className="h-full w-1/2 animate-pulse rounded-full bg-accent" />
          </div>
        )}
        <div className="grid grid-cols-2 gap-4 text-sm sm:grid-cols-4">
          <div><p className="text-muted">Known channels</p><p className="font-mono text-lg">{status.channels.toLocaleString()}</p></div>
          <div><p className="text-muted">Known nodes</p><p className="font-mono text-lg">{status.nodes.toLocaleString()}</p></div>
          <div><p className="text-muted">Blocks recovered</p><p className="font-mono text-lg">{(proxy?.blocks_recovered ?? 0).toLocaleString()}</p></div>
          <div><p className="text-muted">Bitcoin peers</p><p className="font-mono text-lg">{proxy?.eligible_bitcoin_peers ?? "—"}</p></div>
        </div>
        {proxy && (proxy.blocks_fetching > 0 || proxy.blocks_queued > 0) && (
          <p className="text-sm text-muted">
            {proxy.blocks_fetching.toLocaleString()} funding blocks fetching · {proxy.blocks_queued.toLocaleString()} queued
          </p>
        )}
        {proxy && proxy.bytes_downloaded > 0 && (
          <p className="text-xs text-muted">Downloaded {bytes(proxy.bytes_downloaded)} of historical funding blocks.</p>
        )}
        {(proxy?.last_error || status.proxy_error) && (
          <p className="text-sm text-danger">
            {proxy?.last_error ?? `Recovery proxy unavailable: ${status.proxy_error}`}
          </p>
        )}
        <p className="text-xs text-muted">
          P2P gossip has no fixed total. Active means current validation work drained; it does not claim 100% coverage.
        </p>
      </CardContent>
    </Card>
  );
}
