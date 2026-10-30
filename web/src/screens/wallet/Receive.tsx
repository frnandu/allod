import { ArrowLeft, CheckCircle, Spinner } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { Empty, Field, QrBlock, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type NodeInfo, type Offer, type Tx } from "../../lib/api";
import { parseSats } from "../../lib/format";

type Tab = "invoice" | "offer" | "unified" | "onchain";

export function Receive() {
  const [tab, setTab] = useState<Tab>("invoice");
  return (
    <div className="mx-auto max-w-lg space-y-5">
      <Link to="/" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Home</Link>
      <h1 className="text-3xl font-semibold">Receive</h1>
      <Tabs
        tabs={[
          { id: "invoice", label: "Invoice" },
          { id: "offer", label: "Offer" },
          { id: "unified", label: "Unified" },
          { id: "onchain", label: "On-chain" },
        ]}
        value={tab}
        onChange={setTab}
      />
      <Card>
        <CardContent className="space-y-5 p-6">
          {tab === "invoice" && <InvoiceTab />}
          {tab === "offer" && <OfferTab />}
          {tab === "unified" && <UnifiedTab />}
          {tab === "onchain" && <OnchainTab />}
        </CardContent>
      </Card>
    </div>
  );
}

function useAmountDesc() {
  const [amount, setAmount] = useState("");
  const [description, setDescription] = useState("");
  const fields = (optional: boolean) => (
    <>
      <Field label={`Amount (sats${optional ? ", optional" : ""})`}>
        <Input inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="e.g. 21,000" />
      </Field>
      <Field label="Description (optional)">
        <Input value={description} maxLength={640} onChange={(e) => setDescription(e.target.value)} />
      </Field>
    </>
  );
  return { amount: parseSats(amount), description, fields };
}

function InvoiceTab() {
  const f = useAmountDesc();
  const [jit, setJit] = useState(false);
  const [tx, setTx] = useState<Tx | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Just-in-time receive needs a configured LSPS2 LSP (node capabilities).
  const node = useSWR<NodeInfo>("/api/node", fetcher);
  const jitAvailable = node.data?.capabilities.jit_receive ?? false;
  const live = useSWR<Tx>(tx ? `/api/transactions/${tx.public_id}` : null, fetcher, { refreshInterval: 3000 });
  const current = live.data ?? tx;
  if (current?.state === "SETTLED") {
    return (
      <div className="space-y-3 py-6 text-center">
        <CheckCircle size={56} weight="duotone" className="mx-auto text-ok" />
        <p className="text-xl font-semibold">Received</p>
        <Amount msat={current.amount_msat} big />
        <Button variant="secondary" onClick={() => setTx(null)}>New invoice</Button>
      </div>
    );
  }
  if (current) {
    return (
      <div className="space-y-3">
        <QrBlock value={current.payment_request ?? ""} caption="Waiting for payment…" />
        {current.failure_reason ? (
          <Alert tone="warn" title="The channel opening did not complete">
            {current.failure_reason}
            <br />
            The payment never reached this wallet. The invoice is still valid: ask the payer to retry, or
            create a new one.
          </Alert>
        ) : (
          <p className="flex items-center justify-center gap-2 text-xs text-muted"><Spinner size={14} className="animate-spin" /> This page updates when it is paid.</p>
        )}
        <Button variant="secondary" onClick={() => setTx(null)}>New invoice</Button>
      </div>
    );
  }
  return (
    <>
      {f.fields(true)}
      {jitAvailable && (
        <label className="flex items-start gap-3 rounded-lg p-2 hover:bg-surface-2">
          <input type="checkbox" className="mt-1" checked={jit} onChange={(e) => setJit(e.target.checked)} />
          <span>
            <span className="block text-sm">Just-in-time channel</span>
            <span className="block text-xs text-muted">
              No inbound liquidity needed: the LSP opens a channel when this invoice is paid and the payment
              travels over it. The payer pays the LSP&rsquo;s channel fee, so the invoice is for slightly more
              than the amount above. Needs an amount.
            </span>
          </span>
        </label>
      )}
      {error && <Alert tone="danger" title="Could not create the invoice">{error}</Alert>}
      <Button
        size="lg"
        className="w-full"
        onClick={async () => {
          setError(null);
          if (jit && !f.amount) {
            setError("Just-in-time invoices need an amount.");
            return;
          }
          try {
            setTx(await api.post<Tx>("/api/invoices", { amount_msat: f.amount ? f.amount * 1000 : undefined, description: f.description, jit }));
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }}
      >
        Create invoice
      </Button>
    </>
  );
}

function OfferTab() {
  const f = useAmountDesc();
  const offers = useSWR<Offer[]>("/api/offers", fetcher);
  const [shown, setShown] = useState<Offer | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (shown) {
    return (
      <div className="space-y-3">
        <QrBlock value={shown.offer} caption="Reusable: anyone can pay this offer, any number of times." />
        <Button variant="ghost" onClick={() => setShown(null)}>Back to offers</Button>
      </div>
    );
  }
  return (
    <>
      <p className="text-sm text-muted">A BOLT12 offer is a reusable payment code: post it once and receive many payments.</p>
      {f.fields(true)}
      {error && <Alert tone="danger" title="Could not create the offer">{error}</Alert>}
      <Button
        size="lg"
        className="w-full"
        onClick={async () => {
          setError(null);
          try {
            const o = await api.post<Offer>("/api/offers", { amount_msat: f.amount ? f.amount * 1000 : undefined, description: f.description });
            await offers.mutate();
            setShown(o);
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }}
      >
        Create offer
      </Button>
      <div className="space-y-2 pt-2">
        <p className="text-sm font-medium">Your offers</p>
        {offers.data?.length ? (
          offers.data.map((o) => (
            <div key={o.offer_id} className="flex items-center justify-between gap-3 rounded-lg border border-line p-3">
              <button className="min-w-0 text-left" onClick={() => setShown(o)}>
                <p className="truncate text-sm font-medium">{o.description || "No description"}</p>
                <p className="text-xs text-muted">{o.amount_msat ? `${Math.floor(o.amount_msat / 1000).toLocaleString()} sats` : "Any amount"}</p>
              </button>
              <div className="flex items-center gap-2">
                <Badge tone={o.state === "active" ? "ok" : "neutral"}>{o.state}</Badge>
                {(o.state === "active" || o.state === "disabled") && (
                  <Button
                    size="sm"
                    variant="ghost"
                    title="A shared offer cannot be revoked; disabling only hides it here."
                    onClick={async () => {
                      await api.patch(`/api/offers/${o.offer_id}`, { state: o.state === "active" ? "disabled" : "active" });
                      await offers.mutate();
                    }}
                  >
                    {o.state === "active" ? "Disable" : "Enable"}
                  </Button>
                )}
              </div>
            </div>
          ))
        ) : (
          <Empty>No offers yet.</Empty>
        )}
      </div>
    </>
  );
}

function UnifiedTab() {
  const f = useAmountDesc();
  const [uri, setUri] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (uri) {
    return (
      <div className="space-y-3">
        <QrBlock value={uri} caption="Works with wallets that support BOLT11 or BOLT12." />
        <Button variant="ghost" onClick={() => setUri(null)}>New request</Button>
      </div>
    );
  }
  return (
    <>
      <p className="text-sm text-muted">One QR code with both an invoice and an offer (BIP-321). Without an amount, only a reusable offer is included.</p>
      {f.fields(true)}
      {error && <Alert tone="danger" title="Could not create the request">{error}</Alert>}
      <Button
        size="lg"
        className="w-full"
        onClick={async () => {
          setError(null);
          try {
            const r = await api.post<{ bip321: string }>("/api/receive", { amount_msat: f.amount ? f.amount * 1000 : undefined, description: f.description });
            setUri(r.bip321);
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }}
      >
        Create
      </Button>
    </>
  );
}

function OnchainTab() {
  const [address, setAddress] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  return address ? (
    <QrBlock value={address} caption="On-chain funds can be used to open channels." />
  ) : (
    <>
      <p className="text-sm text-muted">Receive bitcoin on-chain into your node's wallet.</p>
      {error && <Alert tone="danger" title="Could not get an address">{error}</Alert>}
      <Button
        size="lg"
        className="w-full"
        onClick={async () => {
          try {
            setAddress((await api.post<{ address: string }>("/api/onchain/address")).address);
          } catch (e) {
            setError(e instanceof Error ? e.message : String(e));
          }
        }}
      >
        Get a new address
      </Button>
    </>
  );
}
