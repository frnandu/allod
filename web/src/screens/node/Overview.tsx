import useSWR from "swr";
import { AllodCard } from "../../components/AllodCard";
import { BitcoinCoreCard } from "../../components/BitcoinCoreCard";
import { LightningNodeCard } from "../../components/LightningNodeCard";
import { fetcher, type Balances, type Channel, type GossipStatus, type NodeInfo } from "../../lib/api";

export function Overview() {
  const { data: n, error } = useSWR<NodeInfo>("/api/node", fetcher, { refreshInterval: 30000 });
  const { data: b } = useSWR<Balances>("/api/balances", fetcher, { refreshInterval: 30000 });
  const { data: channels } = useSWR<Channel[]>("/api/channels", fetcher, { refreshInterval: 30000 });
  const { data: gossip } = useSWR<GossipStatus>("/api/node/gossip", fetcher, { refreshInterval: 10000 });
  return (
    <div className="grid gap-4 lg:grid-cols-2">
      {/* The layers top down: Allod, the Lightning node it runs on, the Bitcoin node below. */}
      <AllodCard />
      {/* min-w-0: a grid cell would otherwise grow to the full node ID instead of truncating it. */}
      <div className="min-w-0 lg:col-span-2">
        <LightningNodeCard node={n} error={error} balances={b} channels={channels} gossip={gossip} />
      </div>
      {/* Lightning runs on top of the Bitcoin chain: the layer below goes last. */}
      <BitcoinCoreCard />
    </div>
  );
}
