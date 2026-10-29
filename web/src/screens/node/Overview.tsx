import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { AnchorReserveWarning } from "../../components/AnchorReserveWarning";
import { Sigil } from "../../components/Sigil";
import { Badge } from "../../components/ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton } from "../../components/ui/extras";
import { fetcher, type Balances, type Channel, type NodeInfo } from "../../lib/api";

export function Overview() {
  const { data: n, error } = useSWR<NodeInfo>("/api/node", fetcher, { refreshInterval: 30000 });
  const { data: b } = useSWR<Balances>("/api/balances", fetcher, { refreshInterval: 30000 });
  const { data: channels } = useSWR<Channel[]>("/api/channels", fetcher, { refreshInterval: 30000 });
  if (error) return <p className="text-sm text-muted">The Lightning node is not reachable yet: {String(error.message)}</p>;
  if (!n) return <p className="text-sm text-muted">Loading…</p>;
  const uri = n.node_uris[0] ?? n.pubkey;
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {b && channels && <div className="lg:col-span-2"><AnchorReserveWarning channels={channels} spendableSat={b.onchain.spendable_sat} /></div>}
      <Card>
        <CardHeader className="flex-row items-center gap-3">
          <Sigil seed={n.pubkey} size={44} />
          <div>
            <CardTitle>{n.alias || "Unnamed node"}</CardTitle>
            <div className="flex gap-2 pt-1">
              <Badge tone="accent">{n.network}</Badge>
              <Badge>block {n.block_height.toLocaleString()}</Badge>
            </div>
          </div>
        </CardHeader>
        <CardContent className="space-y-3 text-sm">
          <div>
            <p className="text-muted">Node ID</p>
            <p className="break-all font-mono text-xs">{n.pubkey}</p>
          </div>
          <div>
            <p className="text-muted">Connect with</p>
            <p className="break-all font-mono text-xs">{uri}</p>
            <div className="pt-2"><CopyButton value={uri} /></div>
          </div>
          <p className="text-xs text-muted">ldk-server {n.version}</p>
        </CardContent>
      </Card>
      {b && (
        <Card>
          <CardHeader><CardTitle>Balances</CardTitle></CardHeader>
          <CardContent className="grid grid-cols-2 gap-4 text-sm">
            <div><p className="text-muted">Lightning total</p><Amount msat={b.lightning.total_msat} /></div>
            <div><p className="text-muted">Can send</p><Amount msat={b.lightning.spendable_msat} /></div>
            <div><p className="text-muted">Can receive</p><Amount msat={b.lightning.receivable_msat} /></div>
            <div><p className="text-muted">On-chain spendable</p><Amount msat={b.onchain.spendable_sat * 1000} /></div>
            <div><p className="text-muted">Anchor reserve</p><Amount msat={b.onchain.reserved_sat * 1000} /></div>
            <div><p className="text-muted">Pending from closes</p><Amount msat={b.onchain.pending_from_closures_sat * 1000} /></div>
          </CardContent>
        </Card>
      )}
    </div>
  );
}
