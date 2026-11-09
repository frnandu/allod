import { ArrowLeft, At, CheckCircle, Spinner } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import { AccountSelect } from "../../components/AccountSelect";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { UnverifiedBackupWarning } from "../../components/BackupBanner";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { QRCodeSVG } from "qrcode.react";
import { CopyButton, Empty, Field, QrBlock, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { appIdOf, useSelectedAccount, type AccountId } from "../../lib/account";
import {
  api,
  DOMAINS_KEY,
  fetcher,
  PUBLIC_NAME_KEY,
  type Domains,
  type NodeInfo,
  type Offer,
  type PublicName,
  type Tx,
} from "../../lib/api";
import { parseSats } from "../../lib/format";

type Tab = "address" | "invoice" | "offer" | "unified" | "onchain";

/** A Lightning address of one wallet, as Receive shows it. */
interface ReceiveAddress {
  address: string;
  live: boolean;
  /** LNURL answers for it, so every Lightning wallet can pay: ZEUS Pay addresses. Own
   * domains not yet (Allod does not serve LUD-16 itself). */
  lnurl: boolean;
  /** Why it is not live yet. */
  status: string;
}

/** The Lightning addresses (own domain, twelve.cash, clank.money) that pay into `account`. */
function addressesFor(d: PublicName | undefined, doms: Domains | undefined, account: AccountId): ReceiveAddress[] {
  if (!d) return [];
  const target = account === "main" ? "main" : String(account);
  const out: ReceiveAddress[] = [];
  for (const a of d.alby.filter((a) => a.address && a.target === target)) {
    // Alby answers LNURL for it: every Lightning wallet can pay.
    out.push({ address: a.address ?? "", live: a.verified, lnurl: true, status: "it does not ask this node for invoices yet" });
  }
  if (d.zeus && d.zeus.target === target) {
    // ZEUS Pay answers LNURL for it: every Lightning wallet can pay.
    out.push({ address: d.zeus.address, live: d.zeus.verified, lnurl: true, status: "ZEUS Pay has not answered for it yet" });
  }
  for (const a of (doms?.domains ?? []).flatMap((x) => x.addresses).filter((a) => a.target === target)) {
    out.push({
      address: a.address,
      live: a.verified || a.lnurl,
      lnurl: a.lnurl,
      status: "its DNS record is not visible yet",
    });
  }
  for (const e of d.twelve_cash.filter((e) => e.target === target)) {
    out.push({
      address: e.address,
      live: e.active && e.verified,
      lnurl: false,
      status: e.active ? "twelve.cash has not published it yet" : "its offer is disabled",
    });
  }
  for (const e of d.clank.filter((e) => e.target === target)) {
    out.push({
      address: e.address,
      live: e.state === "registered" && e.active && e.verified,
      lnurl: false,
      status:
        e.state !== "registered"
          ? "the purchase is not finished yet"
          : e.active
            ? "clank.money has not published it yet"
            : "its offer is disabled",
    });
  }
  return out;
}

export function Receive() {
  // Receive into the account chosen in the top bar unless changed here.
  const selected = useSelectedAccount();
  const [into, setInto] = useState<AccountId | null>(null);
  const account = into ?? selected.id;
  const names = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher);
  const doms = useSWR<Domains>(DOMAINS_KEY, fetcher);
  const addresses = addressesFor(names.data, doms.data, account);
  const hasLive = addresses.some((a) => a.live);
  // Open on the address when this wallet has a live one, unless the owner picked a tab.
  const [picked, setPicked] = useState<Tab | null>(null);
  const tab: Tab = picked ?? (hasLive ? "address" : "invoice");
  const setTab = setPicked;
  return (
    <div className="mx-auto max-w-lg space-y-5">
      <Link to="/" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Home</Link>
      <h1 className="text-3xl font-semibold">Receive</h1>
      <Tabs
        tabs={[
          { id: "address", label: "Address", muted: !hasLive },
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
          {(tab === "address" || tab === "invoice" || tab === "offer") && (
            <AccountSelect label="Receive into" value={account} onChange={setInto} />
          )}
          {tab === "address" && <AddressTab addresses={addresses} />}
          {tab === "invoice" && <InvoiceTab account={account} />}
          {tab === "offer" && <OfferTab account={account} />}
          {tab === "unified" && <UnifiedTab />}
          {tab === "onchain" && <OnchainTab />}
        </CardContent>
      </Card>
    </div>
  );
}

/** BIP-353-only addresses are written with ₿, as BIP-353 asks; LNURL ones as plain names. */
function display(a: ReceiveAddress): string {
  return a.lnurl ? a.address : `₿${a.address}`;
}

function AddressTab({ addresses }: { addresses: ReceiveAddress[] }) {
  const live = addresses.filter((a) => a.live);
  const [chosen, setChosen] = useState(0);
  const shown = live[Math.min(chosen, live.length - 1)];
  if (shown) {
    return (
      <div className="space-y-3">
        {live.length > 1 && (
          <div className="flex flex-wrap gap-2">
            {live.map((a, i) => (
              <Button key={a.address} size="sm" variant={a === shown ? "primary" : "secondary"} onClick={() => setChosen(i)}>
                {display(a)}
              </Button>
            ))}
          </div>
        )}
        <div className="flex flex-col items-center gap-3">
          {/* The QR code carries the bare name, which every wallet that can scan an address parses. */}
          <QRCodeSVG value={shown.address} size={220} className="rounded-xl bg-white p-3" />
          <p className="break-all text-center font-mono text-lg font-medium">{display(shown)}</p>
          <p className="text-xs text-muted">Any amount, any number of times. Share it like an email address.</p>
          <CopyButton value={display(shown)} />
        </div>
        {!shown.lnurl && (
          <Alert tone="warn" title="BIP-353 only: not every wallet can pay it">
            Wallets that support BIP-353 and BOLT12 (Phoenix, Zeus and others) pay this address. LNURL is not active for
            it, so wallets that only know LNURL Lightning addresses cannot pay it yet.{" "}
            <Link to="/settings/lightning-addresses" className="underline">
              Set up LNURL in Lightning address settings
            </Link>
            .
          </Alert>
        )}
      </div>
    );
  }
  const pending = addresses.filter((a) => !a.live);
  return (
    <div className="space-y-3 text-sm">
      <p className="flex items-center gap-2 font-medium">
        <At size={18} weight="duotone" /> No Lightning address here yet
      </p>
      <p className="text-muted">
        A Lightning address looks like an email address, such as alice@example.com. Share it once, on a website or a
        Nostr profile, and anyone can pay it any amount, any number of times, without you making an invoice first.
      </p>
      {pending.length > 0 && (
        <ul className="space-y-1">
          {pending.map((a) => (
            <li key={a.address} className="rounded-lg border border-line p-2">
              <span className="font-mono">{a.address}</span>
              <span className="block text-xs text-muted">Not usable yet: {a.status}.</span>
            </li>
          ))}
        </ul>
      )}
      <Link
        to="/settings/lightning-addresses"
        className="inline-flex h-10 items-center rounded-full border border-line bg-surface px-5 text-sm font-medium hover:bg-surface-2"
      >
        {pending.length ? "Open Lightning address settings" : "Set up a Lightning address"}
      </Link>
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

function InvoiceTab({ account }: { account: AccountId }) {
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
      {jitAvailable && jit && (
        <UnverifiedBackupWarning what="A just-in-time channel puts the received sats in a new channel on this node." />
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
            setTx(await api.post<Tx>("/api/invoices", { amount_msat: f.amount ? f.amount * 1000 : undefined, description: f.description, jit, app_id: appIdOf(account) }));
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

function OfferTab({ account }: { account: AccountId }) {
  const f = useAmountDesc();
  const offers = useSWR<Offer[]>(`/api/offers?app_id=${account}`, fetcher);
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
            const o = await api.post<Offer>("/api/offers", { amount_msat: f.amount ? f.amount * 1000 : undefined, description: f.description, app_id: appIdOf(account) });
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
      <p className="text-sm text-muted">One QR code with both an invoice and an offer (BIP-321). Without an amount, only a reusable offer is included. Unified requests always pay into Main.</p>
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
    <>
      <UnverifiedBackupWarning what="On-chain funds sent to this address can only be recovered with your recovery phrase." />
      <QrBlock value={address} caption="On-chain funds can be used to open channels." />
    </>
  ) : (
    <>
      <p className="text-sm text-muted">Receive bitcoin on-chain into your node's wallet.</p>
      <UnverifiedBackupWarning what="On-chain funds sent to this node can only be recovered with your recovery phrase." />
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
