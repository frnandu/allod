import { CaretRight, Plus } from "@phosphor-icons/react";
import { Link } from "react-router";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Sigil } from "../../components/Sigil";
import { Badge } from "../../components/ui/badge";
import { Card } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { useAccounts } from "../../lib/account";
import { fetcher, type Balances } from "../../lib/api";
import { ago } from "../../lib/format";
import { CoverageWarning } from "./shared";

export function Accounts() {
  const { data, error } = useAccounts();
  const balances = useSWR<Balances>("/api/balances", fetcher, { refreshInterval: 30000 });
  const summary = useSWR<{ count: number; total_balance_msat: number }>("/api/accounts/summary", fetcher);
  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="text-3xl font-semibold">Accounts</h1>
          <p className="text-sm text-muted">Separate balances on your node, each with its own wallet connection.</p>
        </div>
        <Link to="/accounts/new" className="inline-flex h-10 items-center gap-2 rounded-full bg-accent px-5 text-sm font-medium text-accent-fg">
          <Plus size={16} weight="bold" /> New account
        </Link>
      </div>
      <CoverageWarning accountsMsat={summary.data?.total_balance_msat} balances={balances.data} />
      {error && <p className="text-sm text-muted">{String(error.message)}</p>}
      <Card className="divide-y divide-line">
        <div className="flex items-center gap-3 p-4">
          <Sigil seed="main" size={36} />
          <div className="min-w-0 flex-1">
            <p className="font-medium">Main</p>
            <p className="text-xs text-muted">Node funds not in an account</p>
          </div>
          <Amount msat={balances.data?.main_spendable_msat ?? null} className="text-right" />
        </div>
        {data?.items.map((a) => (
          <Link key={a.id} to={`/accounts/${a.id}`} className="flex items-center gap-3 p-4 hover:bg-surface-2">
            <Sigil seed={a.app_pubkey} size={36} />
            <div className="min-w-0 flex-1">
              <p className="flex items-center gap-2 font-medium">
                <span className="truncate">{a.name}</span>
                {a.closed_at != null && <Badge tone="warn">closed</Badge>}
              </p>
              <p className="text-xs text-muted">
                {a.closed_at != null ? "Received a payment after closing: withdraw it to Main" : a.last_used_at ? `Used ${ago(a.last_used_at)}` : "Never used"}
              </p>
            </div>
            <Amount msat={a.balance_msat} className="text-right" />
            <CaretRight size={16} className="text-muted" />
          </Link>
        ))}
      </Card>
      {data && !data.items.length && (
        <Empty>No accounts yet. Create one for a family member, a device or an app, and fund it from Main.</Empty>
      )}
      {summary.data && summary.data.count > 0 && (
        <p className="text-sm text-muted">
          {summary.data.count} account{summary.data.count === 1 ? "" : "s"} holding <Amount msat={summary.data.total_balance_msat} className="text-fg" />
        </p>
      )}
    </div>
  );
}
