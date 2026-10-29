import { useState } from "react";
import useSWR from "swr";
import { TxDetail } from "../../components/TxDetail";
import { TxRow } from "../../components/TxRow";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { Empty, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { fetcher, type Tx } from "../../lib/api";

type Filter = "all" | "incoming" | "outgoing" | "pending";
const PAGE = 25;

export function Activity() {
  const [filter, setFilter] = useState<Filter>("all");
  const [q, setQ] = useState("");
  const [page, setPage] = useState(0);
  const [open, setOpen] = useState<string | null>(null);
  const params = new URLSearchParams({ limit: String(PAGE), offset: String(page * PAGE) });
  if (filter === "incoming" || filter === "outgoing") params.set("direction", filter);
  if (filter === "pending") params.set("state", "PENDING");
  if (q.trim()) params.set("q", q.trim());
  const { data } = useSWR<{ items: Tx[]; total: number }>(`/api/transactions?${params}`, fetcher);
  const pages = data ? Math.ceil(data.total / PAGE) : 0;

  return (
    <div className="space-y-5">
      <h1 className="text-3xl font-semibold">Activity</h1>
      <div className="flex flex-wrap items-center gap-3">
        <Tabs
          tabs={[
            { id: "all", label: "All" },
            { id: "incoming", label: "Received" },
            { id: "outgoing", label: "Sent" },
            { id: "pending", label: "Pending" },
          ]}
          value={filter}
          onChange={(f) => { setFilter(f); setPage(0); }}
        />
        <Input className="max-w-xs" placeholder="Search description or hash" value={q} onChange={(e) => { setQ(e.target.value); setPage(0); }} />
      </div>
      <Card className="divide-y divide-line overflow-hidden">
        {data?.items.length ? data.items.map((tx) => <TxRow key={tx.public_id} tx={tx} onClick={() => setOpen(tx.public_id)} />) : <div className="p-4"><Empty>{data ? "Nothing here yet." : "Loading…"}</Empty></div>}
      </Card>
      {pages > 1 && (
        <div className="flex items-center justify-center gap-3 text-sm">
          <Button size="sm" variant="secondary" disabled={page === 0} onClick={() => setPage(page - 1)}>Previous</Button>
          <span className="text-muted">{page + 1} / {pages}</span>
          <Button size="sm" variant="secondary" disabled={page + 1 >= pages} onClick={() => setPage(page + 1)}>Next</Button>
        </div>
      )}
      <TxDetail id={open} onClose={() => setOpen(null)} />
    </div>
  );
}
