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

// --- The drawing --------------------------------------------------------------------------
// A small, fixed, made-up graph: it pictures the network and the gossip moving through it,
// it is not the node's real graph (that has tens of thousands of channels). Laid out once,
// from a fixed seed, so it never jumps between renders.

const W = 800;
const H = 200;

interface GraphNode {
  x: number;
  y: number;
  r: number;
}
/** A channel, with both ends' positions; `ours` when one end is this node. */
interface GraphEdge {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
  ours: boolean;
}

/** This node: always in the middle. */
const ME: GraphNode = { x: W / 2, y: H / 2, r: 7 };

function seeded(seed: number) {
  return () => {
    seed = (seed + 0x6d2b79f5) | 0;
    let t = Math.imul(seed ^ (seed >>> 15), 1 | seed);
    t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
    return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
  };
}

function layout(): { others: GraphNode[]; edges: GraphEdge[] } {
  const rand = seeded(353);
  // This node first, then the others spread out, never too close to each other.
  const nodes: GraphNode[] = [ME];
  for (let tries = 0; nodes.length < 52 && tries < 4000; tries++) {
    const x = 18 + rand() * (W - 36);
    const y = 14 + rand() * (H - 28);
    if (nodes.every((n) => Math.hypot(n.x - x, n.y - y) > 44)) nodes.push({ x, y, r: 2 + rand() * 3 });
  }
  // Each node gets channels to its 2-3 nearest neighbours (this node to its 5 nearest).
  const seen = new Set<string>();
  const edges: GraphEdge[] = [];
  nodes.forEach((n, i) => {
    const nearest = nodes
      .map((m, j) => ({ m, j, d: Math.hypot(m.x - n.x, m.y - n.y) }))
      .filter(({ j }) => j !== i)
      .sort((p, q) => p.d - q.d)
      .slice(0, n === ME ? 5 : 2 + Math.floor(rand() * 2));
    for (const { m, j } of nearest) {
      const key = i < j ? `${i}-${j}` : `${j}-${i}`;
      if (!seen.has(key)) {
        seen.add(key);
        edges.push({ x1: n.x, y1: n.y, x2: m.x, y2: m.y, ours: n === ME || m === ME });
      }
    }
  });
  return { others: nodes.filter((n) => n !== ME), edges };
}

const GRAPH = layout();

function reducedMotion() {
  return typeof window !== "undefined" && !!window.matchMedia?.("(prefers-reduced-motion: reduce)").matches;
}

/**
 * The network as a picture: channels as lines, nodes as dots, this node in the middle, and,
 * while the map is live, gossip travelling along some channels. When channels are checked
 * on-chain, the share that failed the check is drawn as broken red lines.
 */
function GossipGraph({ live, rejectedShare }: { live: boolean; rejectedShare: number }) {
  const { others, edges } = GRAPH;
  const animate = live && !reducedMotion();
  const rejected = rejectedShare > 0 ? Math.max(1, Math.round(edges.length * rejectedShare)) : 0;
  // Spread the rejected and the animated channels over the drawing instead of bunching them.
  const rejectedAt = new Set(Array.from({ length: rejected }, (_, k) => Math.floor(((k + 0.5) * edges.length) / rejected)));
  const pulses = animate ? edges.filter((_, i) => i % 6 === 2 && !rejectedAt.has(i)) : [];
  return (
    <svg
      viewBox={`0 0 ${W} ${H}`}
      className="block h-40 w-full text-accent sm:h-48"
      preserveAspectRatio="xMidYMid slice"
      // The network goes on past the frame: fade the edges instead of cutting nodes off.
      style={{ maskImage: "radial-gradient(ellipse 75% 85% at center, black 60%, transparent 100%)" }}
      aria-hidden="true"
      data-testid="gossip-graph"
    >
      {edges.map((e, i) => {
        const bad = rejectedAt.has(i);
        return (
          <line
            key={i}
            x1={e.x1}
            y1={e.y1}
            x2={e.x2}
            y2={e.y2}
            stroke={bad ? "var(--color-danger)" : "currentColor"}
            strokeWidth={e.ours ? 1.6 : 1}
            strokeOpacity={bad ? 0.7 : e.ours ? 0.55 : 0.22}
            strokeDasharray={bad ? "3 4" : undefined}
            data-rejected={bad || undefined}
          />
        );
      })}
      {pulses.map((e, k) => {
        const dur = `${2.4 + (k % 4) * 0.7}s`;
        const begin = `${(k * 0.37) % 2}s`;
        return (
          <circle key={`p${k}`} r={2.6} fill="currentColor" opacity={0}>
            <animate attributeName="cx" values={`${e.x1};${e.x2}`} dur={dur} begin={begin} repeatCount="indefinite" />
            <animate attributeName="cy" values={`${e.y1};${e.y2}`} dur={dur} begin={begin} repeatCount="indefinite" />
            <animate attributeName="opacity" values="0;1;1;0" dur={dur} begin={begin} repeatCount="indefinite" />
          </circle>
        );
      })}
      {others.map((n, i) => (
        <circle key={i} cx={n.x} cy={n.y} r={n.r} fill="var(--v-surface)" stroke="currentColor" strokeOpacity={0.6} strokeWidth={1.2} />
      ))}
      <circle cx={ME.x} cy={ME.y} r={14} fill="currentColor" opacity={0.15}>
        {animate && <animate attributeName="r" values="10;24;10" dur="3s" repeatCount="indefinite" />}
        {animate && <animate attributeName="opacity" values="0.3;0;0.3" dur="3s" repeatCount="indefinite" />}
      </circle>
      <circle cx={ME.x} cy={ME.y} r={ME.r} fill="currentColor" />
    </svg>
  );
}

// --- Figures -------------------------------------------------------------------------------

function Stat({ label, value, sub, tone }: { label: string; value: string; sub?: string; tone?: "danger" }) {
  return (
    <div className="rounded-lg border border-line bg-surface p-3">
      <div className="text-xs text-muted">{label}</div>
      <div className={`font-display text-2xl font-semibold tracking-tight tabular-nums${tone === "danger" ? " text-danger" : ""}`}>{value}</div>
      {sub && <div className="text-xs text-muted">{sub}</div>}
    </div>
  );
}

/**
 * "4 of 6" while fewer peers than configured answer. The sidecar can also be connected to more
 * peers than it was configured with (peers that connected to it), and "7 of 6" reads like a
 * mistake, so then it is just "7 connected".
 */
function peers(sidecar: GossipSidecarStatus) {
  return sidecar.peers_connected > sidecar.peers_configured
    ? `${sidecar.peers_connected} connected`
    : `${sidecar.peers_connected} of ${sidecar.peers_configured}`;
}

/** Peers → collected → checked on-chain → handed to the node: what the gossip sidecar does. */
function Pipeline({ sidecar }: { sidecar: GossipSidecarStatus }) {
  const verify = sidecar.verify;
  const checked = verify.unspent_channels + verify.missing_channels;
  const total = checked + verify.pending_channels;
  const pct = (n: number) => (total > 0 ? (n / total) * 100 : 0);
  const steps = [
    { label: "Peers", value: peers(sidecar), sub: "Lightning gossip from other nodes", bar: false },
    { label: "Collected", value: sidecar.graph_channels.toLocaleString(), sub: "channels, before checks", bar: false },
    {
      label: "Checked on-chain",
      value: verify.enabled && checked > 0 ? `${Math.round((verify.unspent_channels / checked) * 100)}% real` : verify.enabled ? "Pending" : "Off",
      sub: verify.enabled ? "each funding output, on your own Bitcoin Core" : "channels are not checked",
      bar: verify.enabled && total > 0,
    },
    {
      label: "Handed to the node",
      value: sidecar.snapshot?.last_served_at ? ago(sidecar.snapshot.last_served_at) : "Not yet",
      sub: "a fresh copy every hour",
      bar: false,
    },
  ];
  return (
    <ol className="grid gap-2 sm:grid-cols-4" aria-label="How the network map is built">
      {steps.map((s, i) => (
        <li key={s.label} className="relative rounded-lg border border-line bg-surface p-3">
          <div className="text-xs text-muted">{s.label}</div>
          <div className="mt-1 font-display text-lg font-semibold tabular-nums">{s.value}</div>
          {s.bar && (
            <div className="mt-1 flex h-1.5 overflow-hidden rounded-full bg-surface-2" aria-hidden="true">
              <div className="bg-ok" style={{ width: `${pct(verify.unspent_channels)}%` }} />
              <div className="bg-danger" style={{ width: `${pct(verify.missing_channels)}%` }} />
              <div className="bg-warn" style={{ width: `${pct(verify.pending_channels)}%` }} />
            </div>
          )}
          <div className="mt-1 text-xs text-muted">{s.sub}</div>
          {i < steps.length - 1 && (
            <span
              className="absolute -right-[13px] top-1/2 z-10 hidden h-5 w-5 -translate-y-1/2 items-center justify-center rounded-full border border-line bg-surface text-xs text-accent sm:flex"
              aria-hidden="true"
            >
              ›
            </span>
          )}
        </li>
      ))}
    </ol>
  );
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
        <Row label="Peers it learns from" value={peers(sidecar)} />
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
 * The node's map of the Lightning network (gossip), which it finds payment routes in: a
 * drawing of the network, the figures the node uses, and, with the gossip sidecar, how the map
 * is built (peers, collected, checked on-chain, handed to the node). The sidecar's own
 * unchecked count and timings stay in the details.
 */
export function NetworkMap({ status }: { status: GossipStatus }) {
  const tone = status.state === "degraded" ? "danger" : status.state === "active" ? "ok" : "accent";
  const error = status.sidecar_error ? `The map helper does not answer: ${status.sidecar_error}` : (status.sidecar?.verify?.last_error ?? status.sidecar?.last_error);
  const now = progress(status);
  const sidecar = status.source === "sidecar" ? (status.sidecar ?? null) : null;
  const verify = sidecar?.verify?.enabled ? sidecar.verify : null;
  const checked = verify ? verify.unspent_channels + verify.missing_channels : 0;
  const rejectedShare = verify && checked > 0 ? verify.missing_channels / checked : 0;
  return (
    <div className="space-y-4">
      <div className="flex items-start justify-between gap-2">
        <div>
          <h4 className="font-medium">Network map</h4>
          <p className="text-xs text-muted">
            {sidecar
              ? "Your own Lightning gossip server: learned from peers, checked against your own Bitcoin Core, no third party."
              : "Learned by the node itself from the peers it is connected to."}
          </p>
        </div>
        <Badge tone={tone}>{labels[status.state]}</Badge>
      </div>

      <div className="overflow-hidden rounded-xl border border-line bg-surface-2/60">
        <GossipGraph live={status.state === "active" || status.state === "learning"} rejectedShare={rejectedShare} />
      </div>

      <div className="grid grid-cols-2 gap-2 sm:grid-cols-4">
        <Stat label="Channels" value={status.channels.toLocaleString()} sub="to find routes in" />
        <Stat label="Nodes" value={status.nodes.toLocaleString()} sub="in the network" />
        {verify && verify.unspent_capacity_sats > 0 ? (
          <Stat label="Capacity" value={btc(verify.unspent_capacity_sats)} sub="checked on-chain" />
        ) : (
          <Stat label="Source" value={sidecar ? "Own server" : "Own node"} sub={sidecar ? "Lightning gossip sidecar" : "peer-to-peer Lightning gossip"} />
        )}
        {verify && checked > 0 ? (
          <Stat label="Rejected" value={verify.missing_channels.toLocaleString()} sub="closed or never funded" tone="danger" />
        ) : (
          <Stat label="Third parties" value="None" sub="no outside map provider" />
        )}
      </div>

      {sidecar && <Pipeline sidecar={sidecar} />}

      {status.state === "learning" && (
        <div className="h-1.5 overflow-hidden rounded-full bg-surface-2" aria-label="Learning the network map">
          <div className="h-full w-1/2 animate-pulse rounded-full bg-accent" />
        </div>
      )}
      {now && <p className="text-xs text-muted">{now}</p>}
      {error && <p className="text-xs text-danger">{error}</p>}
      <Disclosure>{sidecar ? <SidecarDetails sidecar={sidecar} /> : <P2pDetails status={status} />}</Disclosure>
    </div>
  );
}
