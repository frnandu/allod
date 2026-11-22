import { ArrowRight } from "@phosphor-icons/react";
import type { ReactNode } from "react";
import { Link } from "react-router";
import { Amount } from "./Amount";
import { InboundConnections } from "./LightningExposure";
import { NetworkMap } from "./NetworkMap";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { CopyButton, Disclosure } from "./ui/extras";
import type { Balances, Channel, GossipStatus, NodeInfo } from "../lib/api";
import { cn } from "../lib/utils";

/** A part of the Lightning node card. */
function Part({ children, className }: { children: ReactNode; className?: string }) {
  return <section className={cn("rounded-lg border border-line p-4", className)}>{children}</section>;
}

function Figure({ label, msat, big }: { label: string; msat: number; big?: boolean }) {
  return (
    <div>
      <p className="text-xs text-muted">{label}</p>
      <div className={big ? "text-lg" : "text-sm"}>
        <Amount msat={msat} />
      </div>
    </div>
  );
}

function Funds({ balances, channels }: { balances?: Balances; channels?: Channel[] }) {
  const open = channels?.filter((c) => c.is_channel_ready).length ?? 0;
  const opening = (channels?.length ?? 0) - open;
  const offline = channels?.filter((c) => c.is_channel_ready && !c.is_usable).length ?? 0;
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h4 className="font-medium">Funds</h4>
        {channels && (
          <Link to="/node/channels" className="inline-flex items-center gap-1 text-xs font-medium text-accent underline-offset-2 hover:underline">
            {channels.length === 0 ? "No channels yet" : `${open} channel${open === 1 ? "" : "s"}${opening ? `, ${opening} opening` : ""}`}
            <ArrowRight size={12} />
          </Link>
        )}
      </div>
      {balances ? (
        <>
          <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
            <Figure big label="Can send" msat={balances.lightning.spendable_msat} />
            <Figure big label="Can receive" msat={balances.lightning.receivable_msat} />
            <Figure big label="On-chain" msat={balances.onchain.spendable_sat * 1000} />
          </div>
          {offline > 0 && (
            <p className="text-xs text-warn">
              {offline} channel{offline === 1 ? " is" : "s are"} offline: what {offline === 1 ? "it holds" : "they hold"} cannot be sent until the peer is back.
            </p>
          )}
          <Disclosure>
            <div className="grid grid-cols-2 gap-3 sm:grid-cols-3">
              <Figure label="In channels, in total" msat={balances.lightning.total_msat} />
              <Figure label="Kept for channel fees" msat={balances.onchain.reserved_sat * 1000} />
              <Figure label="Returning from closed channels" msat={balances.onchain.pending_from_closures_sat * 1000} />
            </div>
            <p className="text-xs text-muted">
              “Can send” is what your channels let you pay right now; “Kept for channel fees” stays on-chain so a channel
              can always be closed.
            </p>
          </Disclosure>
        </>
      ) : (
        <p className="text-sm text-muted">Loading…</p>
      )}
    </div>
  );
}

/** One piece of software, linked to its exact commit when known. */
function Component({ name, version, repo, rev }: { name: string; version: string; repo: string; rev?: string | null }) {
  const label = `${name} ${version}${rev ? ` (${rev.slice(0, 7)})` : ""}`;
  return rev ? (
    <a href={`https://github.com/lightningdevkit/${repo}/commit/${rev}`} target="_blank" rel="noreferrer" className="underline-offset-2 hover:underline" title={rev}>
      {label}
    </a>
  ) : (
    <span>{label}</span>
  );
}

/** What the node runs: LDK Server, and the LDK Node and rust-lightning (LDK) it is built on. */
function Software({ node }: { node: NodeInfo }) {
  // "0.1.0 (590cdd9…)": the version and the commit ldk-server was built from.
  const [, version, rev] = /^(\S+)\s*\(([0-9a-f]{7,40})\)/.exec(node.version) ?? [];
  const c = node.components;
  return (
    <p className="flex flex-wrap gap-x-2 gap-y-1 pt-1 text-xs text-muted" aria-label="Software">
      {version && rev ? <Component name="LDK Server" version={version} repo="ldk-server" rev={rev} /> : <span>LDK Server {node.version}</span>}
      {c?.ldk_node && (
        <>
          <span aria-hidden>·</span>
          <Component name="LDK Node" version={c.ldk_node.version} repo="ldk-node" rev={c.ldk_node.rev} />
        </>
      )}
      {c?.lightning && (
        <>
          <span aria-hidden>·</span>
          <Component name="LDK" version={c.lightning.version} repo="rust-lightning" rev={c.lightning.rev} />
        </>
      )}
    </p>
  );
}

/** Who the node is on the network, in the stats-row style of the Bitcoin node card. */
function Identity({ node }: { node: NodeInfo }) {
  const id = node.pubkey;
  return (
    <dl className="grid grid-cols-2 gap-x-4 gap-y-4 text-sm sm:grid-cols-4">
      <div className="min-w-0">
        <dt className="text-muted">Alias</dt>
        <dd className={cn("truncate", !node.alias && "text-muted")}>{node.alias || "none set"}</dd>
      </div>
      <div className="order-last col-span-2 min-w-0">
        <dt className="text-muted">Node ID</dt>
        <dd className="flex min-w-0 items-center gap-2">
          <span className="truncate font-mono" title={id}>
            {id.slice(0, 12)}…{id.slice(-12)}
          </span>
          <span className="shrink-0">
            <CopyButton value={id} label="Copy ID" />
          </span>
        </dd>
      </div>
      <div>
        <dt className="text-muted">Block height</dt>
        <dd className="font-mono">{node.block_height.toLocaleString()}</dd>
      </div>
    </dl>
  );
}

/**
 * The Lightning node in one card: who it is, its funds, who can connect to it, and its map
 * of the network. Details that matter less day to day are folded away.
 */
export function LightningNodeCard({
  node,
  error,
  balances,
  channels,
  gossip,
}: {
  node?: NodeInfo;
  error?: Error;
  balances?: Balances;
  channels?: Channel[];
  gossip?: GossipStatus;
}) {
  if (!node)
    return (
      // The Bitcoin node card below is still worth showing: ldk-server not starting is
      // often exactly what missing fee estimates or an unsynced bitcoind cause.
      <Card className="card-wash-lightning">
        <CardHeader>
          <CardTitle>Lightning node</CardTitle>
        </CardHeader>
        <CardContent>
          <p className="text-sm text-muted">{error ? `The Lightning node is not reachable yet: ${String(error.message)}` : "Loading…"}</p>
        </CardContent>
      </Card>
    );
  return (
    <Card className="card-wash-lightning">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div className="min-w-0">
          <CardTitle>Lightning node</CardTitle>
          <Software node={node} />
        </div>
        <Badge tone="accent">{node.network}</Badge>
      </CardHeader>
      <CardContent className="space-y-4">
        <Identity node={node} />
        <div className="grid gap-4 lg:grid-cols-2">
          <Part>
            <Funds balances={balances} channels={channels} />
          </Part>
          <Part>
            <InboundConnections nodeUris={node.node_uris} />
          </Part>
          {gossip && (
            <Part className="lg:col-span-2">
              <NetworkMap status={gossip} />
            </Part>
          )}
        </div>
      </CardContent>
    </Card>
  );
}
