import { Plug, Plus } from "@phosphor-icons/react";
import { Link } from "react-router";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Badge } from "../../components/ui/badge";
import { Card } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { fetcher, type AppConn } from "../../lib/api";
import { ago } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { BudgetBar } from "./BudgetBar";

export function Connections() {
  const { data, error } = useSWR<AppConn[]>("/api/apps", fetcher, { refreshInterval: 15000 });
  const now = useNow();
  return (
    <div className="space-y-6">
      <div className="flex items-center justify-between gap-4">
        <div>
          <h1 className="text-3xl font-semibold">Connections</h1>
          <p className="text-sm text-muted">Apps that use your wallet through Nostr Wallet Connect.</p>
        </div>
        <Link to="/connections/new" className="inline-flex h-10 items-center gap-2 rounded-full bg-accent px-5 text-sm font-medium text-accent-fg">
          <Plus size={16} weight="bold" /> New connection
        </Link>
      </div>
      {error && <p className="text-sm text-muted">{String(error.message)}</p>}
      {data?.length ? (
        <Card className="divide-y divide-line">
          {data.map((a) => (
            <Link key={a.id} to={`/connections/${a.id}`} className="grid gap-3 p-4 hover:bg-surface-2 sm:grid-cols-[1fr_220px]">
              <div className="flex min-w-0 items-start gap-3">
                <span className="mt-0.5 grid size-9 shrink-0 place-items-center rounded-full bg-surface-2 text-accent">
                  <Plug size={18} />
                </span>
                <div className="min-w-0">
                  <div className="flex flex-wrap items-center gap-2">
                    <span className="font-medium">{a.name}</span>
                    {a.isolated && <Badge tone="accent">isolated</Badge>}
                    {a.expires_at != null && a.expires_at <= now && <Badge tone="danger">expired</Badge>}
                  </div>
                  <p className="text-xs text-muted">
                    {a.last_used_at ? `Used ${ago(a.last_used_at)}` : "Never used"}
                    {a.expires_at != null && a.expires_at > now && ` · expires ${new Date(a.expires_at * 1000).toLocaleDateString()}`}
                  </p>
                  {a.balance_msat != null && (
                    <p className="text-xs text-muted">
                      Balance <Amount msat={a.balance_msat} />
                    </p>
                  )}
                </div>
              </div>
              <BudgetBar budget={a.budget} />
            </Link>
          ))}
        </Card>
      ) : (
        <Empty>{data ? "No connections yet. Create one and paste its link into a Nostr Wallet Connect app." : "Loading…"}</Empty>
      )}
    </div>
  );
}
