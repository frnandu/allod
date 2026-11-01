import { useState } from "react";
import useSWR from "swr";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type Peer } from "../../lib/api";
import { NodeLabel } from "../../components/NodeLabel";

export function Peers() {
  const { data, mutate } = useSWR<Peer[]>("/api/peers", fetcher, { refreshInterval: 15000 });
  const [uri, setUri] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-4">
      <div className="flex gap-2">
        <Input placeholder="node_id@host:port" value={uri} onChange={(e) => setUri(e.target.value)} spellCheck={false} />
        <Button
          disabled={!uri.includes("@")}
          onClick={async () => {
            setError(null);
            try {
              await api.post("/api/peers", { pubkey: uri.trim(), persist: true });
              setUri("");
              void mutate();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Connect
        </Button>
      </div>
      {error && <Alert tone="danger" title="Could not connect">{error}</Alert>}
      {data?.length ? (
        <Card className="divide-y divide-line">
          {data.map((p) => (
            <div key={p.node_id} className="flex items-center justify-between gap-3 p-4">
              <div className="min-w-0">
                <NodeLabel pubkey={p.node_id} alias={p.alias} />
                <p className="text-xs text-muted">{p.address}</p>
              </div>
              <div className="flex items-center gap-2">
                <Badge tone={p.is_connected ? "ok" : "neutral"}>{p.is_connected ? "connected" : "offline"}</Badge>
                <Button size="sm" variant="ghost" onClick={async () => { await api.del(`/api/peers/${p.node_id}`); void mutate(); }}>Disconnect</Button>
              </div>
            </div>
          ))}
        </Card>
      ) : (
        <Empty>{data ? "No peers." : "Loading…"}</Empty>
      )}
    </div>
  );
}
