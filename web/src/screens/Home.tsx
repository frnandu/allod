import { ArrowDownLeft, ArrowUpRight } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { Amount } from "../components/Amount";
import { TxDetail } from "../components/TxDetail";
import { TxRow } from "../components/TxRow";
import { Alert } from "../components/ui/alert";
import { Badge } from "../components/ui/badge";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../components/ui/card";
import { Empty } from "../components/ui/extras";
import { fetcher, type Balances, type HealthResponse, type InfoResponse, type SetupProgress, type Tx } from "../lib/api";
import { SyncStatus } from "./setup/SetupWizard";

export function Home() {
  const info = useSWR<InfoResponse>("/api/info", fetcher);
  const health = useSWR<HealthResponse>("/api/health", fetcher, { refreshInterval: 15000 });
  const progress = useSWR<SetupProgress>("/api/setup/progress", fetcher, { refreshInterval: 15000 });
  const ready = health.data?.status === "ok";
  const balances = useSWR<Balances>(ready ? "/api/balances" : null, fetcher, { refreshInterval: 30000 });
  const recent = useSWR<{ items: Tx[] }>(ready ? "/api/transactions?limit=8" : null, fetcher);
  const [open, setOpen] = useState<string | null>(null);
  const b = balances.data;

  return (
    <div className="space-y-6">
      <section className="v-hero rounded-[var(--radius-card)] border border-line p-6 sm:p-8">
        <div className="flex flex-wrap items-center gap-3">
          <h1 className="text-xl font-semibold text-muted">Main</h1>
          <Badge tone={ready ? "ok" : "warn"}>{ready ? "Node running" : "Node starting"}</Badge>
          {info.data?.network && <Badge tone="accent">{info.data.network}</Badge>}
        </div>
        <div className="mt-4">
          <Amount msat={b?.main_spendable_msat ?? null} big />
        </div>
        {b && (
          <div className="mt-4 flex flex-wrap gap-x-8 gap-y-2 text-sm text-muted">
            <span>In accounts <Amount msat={b.accounts_total_msat ?? 0} className="text-fg" /></span>
            {!!b.in_flight_msat && <span>In flight <Amount msat={b.in_flight_msat} className="text-fg" /></span>}
            <span>Can receive <Amount msat={b.lightning.receivable_msat} className="text-fg" /></span>
            <span>On-chain <Amount msat={b.onchain.total_sat * 1000} className="text-fg" /></span>
          </div>
        )}
        <div className="mt-6 flex gap-3">
          <Link to="/send" className="inline-flex h-11 items-center gap-2 rounded-full bg-accent px-6 text-sm font-medium text-accent-fg">
            <ArrowUpRight size={18} weight="bold" /> Send
          </Link>
          <Link to="/receive" className="inline-flex h-11 items-center gap-2 rounded-full border border-line bg-surface px-6 text-sm font-medium">
            <ArrowDownLeft size={18} weight="bold" /> Receive
          </Link>
        </div>
      </section>

      {!ready && progress.data && (
        <Card>
          <CardHeader>
            <CardTitle>Getting ready</CardTitle>
            <CardDescription>Your node is catching up with the Bitcoin network.</CardDescription>
          </CardHeader>
          <CardContent>
            <SyncStatus progress={progress.data} />
          </CardContent>
        </Card>
      )}
      {ready && b && b.lightning.spendable_msat === 0 && b.lightning.receivable_msat === 0 && (
        <Alert tone="info" title="Open your first channel">
          Lightning payments need a channel. <Link to="/node/channels" className="text-accent underline">Open one in Node → Channels</Link>.
        </Alert>
      )}

      {ready && (
        <Card>
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle>Recent activity</CardTitle>
            <Link to="/activity" className="text-sm text-accent">See all</Link>
          </CardHeader>
          <div className="divide-y divide-line border-t border-line">
            {recent.data?.items.length ? (
              recent.data.items.map((tx) => <TxRow key={tx.public_id} tx={tx} onClick={() => setOpen(tx.public_id)} />)
            ) : (
              <div className="p-4"><Empty>No payments yet.</Empty></div>
            )}
          </div>
        </Card>
      )}
      <TxDetail id={open} onClose={() => setOpen(null)} />
    </div>
  );
}
