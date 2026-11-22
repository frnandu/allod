import { Badge } from "./ui/badge";
import { Disclosure } from "./ui/extras";
import type { GossipSidecarStatus, GossipStatus } from "../lib/api";
import { ago } from "../lib/format";

const labels = {
  waiting: "Waiting",
  learning: "Learning",
  active: "Up to date",
  degraded: "Problem",
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

function Row({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex justify-between gap-3 text-xs">
      <span className="text-muted">{label}</span>
      <span className="text-right font-mono">{value}</span>
    </div>
  );
}

/** What is happening right now, in one line, when it is not simply up to date. */
function progress(status: GossipStatus): string | null {
  const s = status.sidecar;
  if (status.source !== "sidecar" || !s || s.ready) return null;
  if (s.state === "verifying" && s.verify?.waiting_for_chain) return "Waiting for the Bitcoin node to finish syncing before checking channels.";
  if (s.state === "verifying") return "Checking that the channels it learned really exist on-chain.";
  return `Learning the network from peers: ${s.graph_channels.toLocaleString()} channels so far.`;
}

function SidecarDetails({ sidecar }: { sidecar: GossipSidecarStatus }) {
  const verify = sidecar.verify;
  const snapshot = sidecar.snapshot;
  return (
    <>
      <p className="text-xs text-muted">
        A helper next to the node collects the map from other nodes, checks each channel against the Bitcoin chain, and
        hands the node a fresh copy every hour.
      </p>
      <div className="space-y-1">
        <Row label="Channels collected (before checks)" value={sidecar.graph_channels.toLocaleString()} />
        <Row label="Peers it learns from" value={`${sidecar.peers_connected} of ${sidecar.peers_configured}`} />
        {sidecar.initial_sync_secs != null && <Row label="First sync took" value={duration(sidecar.initial_sync_secs)} />}
      </div>
      {verify?.enabled ? (
        <p className="text-xs text-muted">
          {verify.scanning
            ? `Checking channels against the chain${verify.scan_progress != null ? ` (${Math.round(verify.scan_progress)}%)` : ""}…`
            : verify.last_scan_finished_at
              ? `Channels checked ${ago(verify.last_scan_finished_at)}${verify.last_scan_height ? ` at block ${verify.last_scan_height.toLocaleString()}` : ""}${verify.last_scan_secs != null ? ` in ${duration(verify.last_scan_secs)}` : ""}.`
              : "Channels not checked yet."}{" "}
          {verify.unspent_channels.toLocaleString()} open ({btc(verify.unspent_capacity_sats)}), {verify.missing_channels.toLocaleString()} closed or never
          funded, {verify.pending_channels.toLocaleString()} waiting for the next check.
        </p>
      ) : (
        <p className="text-xs text-muted">Channel checks are off: the node gets channels unchecked and without their size.</p>
      )}
      {snapshot?.last_served_at ? (
        <p className="text-xs text-muted">
          Last copy handed to the node {ago(snapshot.last_served_at)}: {(snapshot.channels ?? 0).toLocaleString()} channels,{" "}
          {(snapshot.channels_with_capacity ?? 0).toLocaleString()} with their size
          {snapshot.last_bytes != null && `, ${bytes(snapshot.last_bytes)}`}.
        </p>
      ) : (
        sidecar.ready && <p className="text-xs text-muted">Ready; the node takes its first copy at its next hourly update.</p>
      )}
    </>
  );
}

function P2pDetails({ status }: { status: GossipStatus }) {
  return (
    <>
      <p className="text-xs text-muted">The node collects the map itself from the nodes it is connected to.</p>
      {status.channels_per_hour != null && (
        <p className="text-xs text-muted">
          {status.channels_per_hour > 0 || (status.nodes_per_hour ?? 0) > 0
            ? `Growing by ${Math.round(status.channels_per_hour).toLocaleString()} channels and ${Math.round(status.nodes_per_hour ?? 0).toLocaleString()} nodes an hour.`
            : "Not growing right now."}
        </p>
      )}
      <p className="text-xs text-muted">There is no known total to compare with: “up to date” means the map stopped growing.</p>
    </>
  );
}

/**
 * The node's map of the Lightning network (gossip), which it finds payment routes in.
 * The count is what the node itself uses; the sidecar's own (unchecked) count is a detail.
 */
export function NetworkMap({ status }: { status: GossipStatus }) {
  const tone = status.state === "degraded" ? "danger" : status.state === "active" ? "ok" : "accent";
  const error = status.sidecar_error ? `The map helper does not answer: ${status.sidecar_error}` : (status.sidecar?.verify?.last_error ?? status.sidecar?.last_error);
  const now = progress(status);
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h4 className="font-medium">Network map</h4>
        <Badge tone={tone}>{labels[status.state]}</Badge>
      </div>
      <p className="text-sm">
        Knows <span className="font-mono">{status.channels.toLocaleString()}</span> channels between{" "}
        <span className="font-mono">{status.nodes.toLocaleString()}</span> nodes
        <span className="text-muted">, to find routes for payments.</span>
      </p>
      {status.state === "learning" && (
        <div className="h-1.5 overflow-hidden rounded-full bg-surface-2" aria-label="Learning the network map">
          <div className="h-full w-1/2 animate-pulse rounded-full bg-accent" />
        </div>
      )}
      {now && <p className="text-xs text-muted">{now}</p>}
      {error && <p className="text-xs text-danger">{error}</p>}
      <Disclosure>
        {status.source === "sidecar" && status.sidecar ? <SidecarDetails sidecar={status.sidecar} /> : <P2pDetails status={status} />}
      </Disclosure>
    </div>
  );
}
