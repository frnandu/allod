import { useState } from "react";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { Empty, Field, QrBlock } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type Balances, type LnPayment } from "../../lib/api";
import { ago, parseSats } from "../../lib/format";
import { shorten } from "../../lib/utils";

export function Onchain() {
  const { data: b, mutate: refreshBalances } = useSWR<Balances>("/api/balances", fetcher);
  const { data: txs, mutate } = useSWR<LnPayment[]>("/api/onchain/transactions", fetcher, { refreshInterval: 30000 });
  const [address, setAddress] = useState<string | null>(null);
  const [to, setTo] = useState("");
  const [amount, setAmount] = useState("");
  const [all, setAll] = useState(false);
  const [rate, setRate] = useState("");
  const [result, setResult] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  return (
    <div className="grid gap-4 lg:grid-cols-2">
      <Card>
        <CardHeader><CardTitle>On-chain wallet</CardTitle></CardHeader>
        <CardContent className="space-y-4">
          {b && <Amount msat={b.onchain.spendable_sat * 1000} big />}
          {address ? (
            <QrBlock value={address} />
          ) : (
            <Button variant="secondary" onClick={async () => setAddress((await api.post<{ address: string }>("/api/onchain/address")).address)}>
              Receive: new address
            </Button>
          )}
        </CardContent>
      </Card>
      <Card>
        <CardHeader><CardTitle>Send on-chain</CardTitle></CardHeader>
        <CardContent className="space-y-4">
          <Field label="Address"><Input value={to} onChange={(e) => setTo(e.target.value)} spellCheck={false} /></Field>
          <Field label="Amount (sats)">
            <Input inputMode="numeric" value={amount} disabled={all} onChange={(e) => setAmount(e.target.value)} />
          </Field>
          <label className="flex items-center gap-2 text-sm"><input type="checkbox" checked={all} onChange={(e) => setAll(e.target.checked)} /> Send everything</label>
          <Field label="Fee rate (sat/vB, optional)"><Input inputMode="numeric" value={rate} onChange={(e) => setRate(e.target.value)} /></Field>
          {error && <Alert tone="danger" title="Could not send">{error}</Alert>}
          {result && <Alert tone="info" title="Sent">Transaction <span className="break-all font-mono text-xs">{result}</span></Alert>}
          <Button
            className="w-full"
            disabled={!to.trim() || (!all && !parseSats(amount))}
            onClick={async () => {
              setError(null);
              try {
                const r = await api.post<{ txid: string }>("/api/onchain/send", {
                  address: to.trim(),
                  amount_sat: all ? undefined : parseSats(amount),
                  send_all: all,
                  fee_rate_sat_vb: parseSats(rate) ?? undefined,
                });
                setResult(r.txid);
                void mutate();
                void refreshBalances();
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              }
            }}
          >
            Send
          </Button>
        </CardContent>
      </Card>
      <Card className="lg:col-span-2">
        <CardHeader><CardTitle>Transactions</CardTitle></CardHeader>
        <div className="divide-y divide-line border-t border-line">
          {txs?.length ? (
            txs.map((p) => (
              <div key={p.id} className="flex items-center justify-between gap-3 px-5 py-3 text-sm">
                <div>
                  <p className="font-mono text-xs">{shorten(p.kind.txid ?? p.id, 10, 8)}</p>
                  <p className="text-xs text-muted">{ago(p.updated_at)}</p>
                </div>
                <div className="flex items-center gap-3">
                  {p.kind.confirmed_height ? <Badge tone="ok">confirmed</Badge> : <Badge tone="warn">unconfirmed</Badge>}
                  {!p.kind.confirmed_height && p.direction === "outbound" && (
                    <Button size="sm" variant="ghost" onClick={async () => { await api.post("/api/onchain/bump-fee", { payment_id: p.id }); void mutate(); }}>Bump fee</Button>
                  )}
                  <Amount msat={p.amount_msat} sign={p.direction === "inbound" ? "+" : "-"} />
                </div>
              </div>
            ))
          ) : (
            <div className="p-4"><Empty>No on-chain transactions.</Empty></div>
          )}
        </div>
      </Card>
    </div>
  );
}
