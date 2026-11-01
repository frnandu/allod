import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import type { GossipSidecarStatus, GossipStatus } from "../lib/api";
import { ago } from "../lib/format";

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

function duration(secs: number) {
  if (secs < 60) return `${secs} s`;
  if (secs < 3600) return `${Math.floor(secs / 60)} min ${secs % 60} s`;
  return `${Math.floor(secs / 3600)} h ${Math.floor((secs % 3600) / 60)} min`;
}

function btc(sats: number) {
  return `${(sats / 1e8).toLocaleString(undefined, { maximumFractionDigits: 0 })} BTC`;
}

function Stat({ label, value }: { label: string; value: string | number }) {
  return (
    <div>
      <p className="text-muted">{label}</p>
      <p className="font-mono text-lg">{typeof value === "number" ? value.toLocaleString() : value}</p>
    </div>
  );
}

function SidecarDetails({ status, sidecar }: { status: GossipStatus; sidecar?: GossipSidecarStatus | null }) {
  const verify = sidecar?.verify;
  const snapshot = sidecar?.snapshot;
  return (
    <>
      <div className="grid grid-cols-2 gap-4 text-sm sm:grid-cols-4">
        <Stat label="Channels in node" value={status.channels} />
        <Stat label="Nodes in node" value={status.nodes} />
        <Stat label="Sidecar channels" value={sidecar?.graph_channels ?? "—"} />
        <Stat
          label="Gossip peers"
          value={sidecar ? `${sidecar.peers_connected}/${sidecar.peers_configured}` : "—"}
        />
      </div>
      {sidecar && !sidecar.ready && (
        <p className="text-sm text-muted">
          {sidecar.state === "verifying"
            ? "Graph synced; checking channel funding outputs before serving the first snapshot."
            : `Syncing gossip from peers: ${sidecar.graph_channels.toLocaleString()} channels so far.`}
        </p>
      )}
      {sidecar?.initial_sync_secs != null && (
        <p className="text-sm text-muted">Sidecar graph synced in {duration(sidecar.initial_sync_secs)} after start.</p>
      )}
      {verify?.enabled ? (
        <p className="text-sm text-muted">
          {verify.scanning
            ? `Scanning bitcoind's UTXO set${verify.scan_progress != null ? ` (${Math.round(verify.scan_progress)}%)` : ""}…`
            : verify.last_scan_finished_at
              ? `Funding checked ${ago(verify.last_scan_finished_at)}${verify.last_scan_height ? ` at block ${verify.last_scan_height.toLocaleString()}` : ""}${verify.last_scan_secs != null ? ` in ${duration(verify.last_scan_secs)}` : ""}.`
              : "Funding not checked yet."}{" "}
          {verify.unspent_channels.toLocaleString()} channels unspent ({btc(verify.unspent_capacity_sats)}),{" "}
          {verify.missing_channels.toLocaleString()} closed or unfunded, {verify.pending_channels.toLocaleString()} waiting for the next scan.
        </p>
      ) : (
        sidecar && (
          <p className="text-sm text-muted">
            Funding checks are off: channels are served unverified and without capacity.
          </p>
        )
      )}
      {snapshot?.last_served_at ? (
        <p className="text-xs text-muted">
          Last snapshot sent to the node {ago(snapshot.last_served_at)}: {(snapshot.channels ?? 0).toLocaleString()} channels,{" "}
          {(snapshot.channels_with_capacity ?? 0).toLocaleString()} with capacity
          {snapshot.last_bytes != null && `, ${bytes(snapshot.last_bytes)}`}. The node fetches one per hour.
        </p>
      ) : (
        sidecar?.ready && <p className="text-xs text-muted">Ready; the node fetches its first snapshot on its next hourly sync.</p>
      )}
      {(sidecar?.last_error || verify?.last_error || status.sidecar_error) && (
        <p className="text-sm text-danger">
          {status.sidecar_error
            ? `Gossip sidecar unavailable: ${status.sidecar_error}`
            : (verify?.last_error ?? sidecar?.last_error)}
        </p>
      )}
    </>
  );
}

function P2pDetails({ status }: { status: GossipStatus }) {
  return (
    <>
      <div className="grid grid-cols-2 gap-4 text-sm">
        <Stat label="Known channels" value={status.channels} />
        <Stat label="Known nodes" value={status.nodes} />
      </div>
      {status.channels_per_hour != null && (
        <p className="text-sm text-muted">
          {status.channels_per_hour > 0 || (status.nodes_per_hour ?? 0) > 0
            ? `Growing +${Math.round(status.channels_per_hour).toLocaleString()} channels/h and +${Math.round(status.nodes_per_hour ?? 0).toLocaleString()} nodes/h.`
            : "No new channels in the last minute: the graph is not growing right now."}
        </p>
      )}
      <p className="text-xs text-muted">
        P2P gossip has no fixed total. Active means the graph stopped growing; it does not claim 100% coverage.
      </p>
    </>
  );
}

export function GossipStatusCard({ status }: { status: GossipStatus }) {
  const tone = status.state === "degraded" ? "danger" : status.state === "active" ? "ok" : "accent";
  const sidecar = status.source === "sidecar";
  return (
    <Card className="lg:col-span-2">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div>
          <CardTitle>Network graph</CardTitle>
          <p className="pt-1 text-xs text-muted">
            {sidecar ? "Local gossip sidecar · P2P gossip, served to the node as snapshots" : "P2P gossip · no third-party snapshot"}
          </p>
        </div>
        <Badge tone={tone}>{labels[status.state]}</Badge>
      </CardHeader>
      <CardContent className="space-y-4">
        {status.state === "learning" && (
          <div className="h-1.5 overflow-hidden rounded-full bg-surface-2" aria-label="Learning network graph">
            <div className="h-full w-1/2 animate-pulse rounded-full bg-accent" />
          </div>
        )}
        {sidecar ? <SidecarDetails status={status} sidecar={status.sidecar} /> : <P2pDetails status={status} />}
      </CardContent>
    </Card>
  );
}
