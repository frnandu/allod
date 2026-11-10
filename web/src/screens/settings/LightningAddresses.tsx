import { useEffect, useState, type ReactNode } from "react";
import useSWR, { useSWRConfig } from "swr";
import { Link } from "react-router";
import { ArrowsClockwise, CaretRight, Eye, Globe, Plus, ShieldCheck, Trash, Wallet, Warning, WarningCircle, type Icon } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { CopyButton, Field, Modal, QrBlock, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  ApiError,
  domains as domainsApi,
  fetcher,
  publicName,
  PUBLIC_NAME_KEY,
  type DomainAddress,
  type DomainEntry,
  type Domains,
  type AlbyEntry,
  type PublicName,
  type ZeusEntry,
} from "../../lib/api";
import { useAccounts } from "../../lib/account";
import { cn } from "../../lib/utils";
import { ladder, SignalLegend, SignalList, Signals, type LadderStep } from "./AddressLadder";
import { BuyDomain, PurchaseCard, RecordRows, useDomains } from "./Domains";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

type Provider = "own" | "twelve" | "clank" | "zeus" | "alby";
/** What the address answers: BIP-353, LNURL, or LNURL once the domain is reachable. */
type Tag = "bip353" | "lnurl" | "lnurl_soon";
type Tone = "ok" | "warn" | "danger" | "neutral";

/** One configured Lightning address, whatever kind or provider. */
interface Item {
  key: string;
  address: string;
  /** "main" or the account id. */
  target: string;
  provider: Provider;
  kinds: Tag[];
  status: { tone: Tone; label: string };
  note?: string;
  /** An Alby address. */
  alby?: AlbyEntry;
  /** An address on an own domain. */
  own?: { a: DomainAddress; d: DomainEntry };
  /** Removing is not possible yet (a clank.money purchase in progress). */
  locked?: boolean;
  zeus?: ZeusEntry;
  /** A twelve.cash name bought on its website (not a free random one). */
  custom?: boolean;
  /** Short "what is missing" line while the address is not live. */
  action?: string;
}

const PROVIDER_LABEL: Record<Provider, string> = {
  alby: "Alby",
  own: "Own domain",
  twelve: "twelve.cash",
  clank: "clank.money",
  zeus: "ZEUS Pay",
};

function items(d: PublicName, doms: Domains | undefined): Item[] {
  const out: Item[] = [];
  for (const a of d.alby) {
    out.push({
      key: `alby-${a.connection_id}`,
      address: a.address ?? "Alby address (setup not finished)",
      target: a.target,
      alby: a,
      provider: "alby",
      kinds: ["lnurl"],
      status: a.verified ? { tone: "ok", label: "Live" } : { tone: "warn", label: "Action needed" },
      action: a.verified
        ? undefined
        : a.address
          ? "It does not ask this node for invoices yet"
          : "Finish the setup on getalby.com and enter the address",
      note: a.error ?? undefined,
    });
  }
  if (d.zeus) {
    out.push({
      key: "zeus",
      address: d.zeus.address,
      target: d.zeus.target,
      provider: "zeus",
      kinds: ["lnurl"],
      status: d.zeus.verified ? { tone: "ok", label: "Live" } : { tone: "warn", label: "Waiting for ZEUS Pay" },
      action: d.zeus.verified ? undefined : "ZEUS Pay has not answered for it yet",
      zeus: d.zeus,
    });
  }
  for (const dom of doms?.domains ?? []) {
    for (const a of dom.addresses) {
      const every = dom.served && a.lnurl_enabled;
      const waiting = every && !a.lnurl;
      const kinds: Tag[] = [];
      if (a.bip353) kinds.push("bip353");
      if (every) kinds.push(a.lnurl ? "lnurl" : "lnurl_soon");
      const live = (a.bip353 && a.verified) || a.lnurl;
      out.push({
        key: `own-${a.id}`,
        address: a.address,
        target: a.target,
        provider: "own",
        kinds,
        status: live ? { tone: "ok", label: "Live" } : { tone: "warn", label: "Action needed" },
        action:
          !a.bip353 || a.verified
          ? waiting && !a.bip353
            ? "Not reachable over HTTPS yet"
            : undefined
          : dom.dns_provider_id
            ? "Waiting for the DNS provider to serve its record"
            : "Add its TXT record at your DNS host",
        note: waiting
          ? `LNURL answers once https://${dom.name} reaches this node${dom.reach?.detail ? ` (${dom.reach.detail})` : ""}.`
          : undefined,
        own: { a, d: dom },
      });
    }
  }
  for (const e of d.twelve_cash) {
    out.push({
      key: `twelve-${e.target}`,
      address: e.address,
      target: e.target,
      provider: "twelve",
      kinds: ["bip353"],
      custom: e.custom,
      locked: e.purchase?.state === "paid",
      status: e.purchase
        ? {
            tone: "warn",
            label: e.purchase.state === "paying" ? "Payment not finished" : "Finishing registration",
          }
        : !e.active
        ? { tone: "danger", label: "Offer disabled" }
        : e.verified
          ? { tone: "ok", label: "Live" }
          : { tone: "warn", label: "Waiting for DNS" },
      action: e.purchase
        ? e.purchase.state === "paying"
          ? "The invoice is not paid yet"
          : "The purchase is not finished yet"
        : !e.active
          ? "Its wallet's offer is disabled"
          : e.verified
            ? undefined
            : "Waiting for twelve.cash to publish it",
      note: !e.purchase
        ? e.active
          ? undefined
          : "The wallet's offer was disabled, so payments fail. twelve.cash cannot delete the name; stop sharing it."
        : e.purchase.state === "paying"
          ? `The invoice is not paid yet${e.purchase.error ? ` (${e.purchase.error})` : ""}. Pay it from another wallet; Allod finishes the purchase when twelve.cash sees it.`
          : `Paid ${e.purchase.price_sats.toLocaleString()} sats; twelve.cash has not published it yet${e.purchase.error ? ` (${e.purchase.error})` : ""}. Allod keeps trying.`,
    });
  }
  for (const e of d.clank) {
    const busy = e.state !== "registered";
    // Unpaid: the invoice can still be paid from another wallet, or the attempt dropped.
    const unpaid = e.state === "paying";
    out.push({
      key: `clank-${e.target}`,
      address: e.address,
      target: e.target,
      provider: "clank",
      kinds: ["bip353"],
      status: busy
        ? { tone: "warn", label: unpaid ? "Payment not finished" : "Finishing registration" }
        : !e.active
          ? { tone: "danger", label: "Offer disabled" }
          : e.verified
            ? { tone: "ok", label: "Live" }
            : { tone: "warn", label: "Waiting for DNS" },
      note: !busy
        ? undefined
        : unpaid
          ? `The invoice is not paid yet${e.error ? ` (${e.error})` : ""}. Pay it from another wallet, or remove the attempt.`
          : `Paid ${e.price_sats.toLocaleString()} sats; registration not confirmed yet (${e.error}). Allod keeps trying.`,
      // Only a paid attempt must stay: nothing can finish an unpaid one silently.
      locked: e.state === "paid",
      action: busy
        ? unpaid
          ? "The invoice is not paid yet"
          : "The purchase is not finished yet"
        : !e.active
          ? "Its wallet's offer is disabled"
          : e.verified
            ? undefined
            : "Waiting for clank.money to publish it",
    });
  }
  return out;
}

/** A twelve.cash or clank.money name not seen in DNS yet. */
function namesWaiting(d: PublicName): boolean {
  return (
    (!!d.zeus && !d.zeus.verified) ||
    d.twelve_cash.some((e) => e.active && !e.verified) ||
    d.clank.some((e) => e.active && e.state === "registered" && !e.verified)
  );
}

/** Main and the open accounts, as targets. */
function useTargets(): { id: string; name: string }[] {
  const { open } = useAccounts();
  return [{ id: "main", name: "Main" }, ...open.map((a) => ({ id: String(a.id), name: a.name }))];
}

/** Lightning addresses of Main and the accounts (SPEC §12): own domain, twelve.cash, clank.money. */
/** twelve.cash's price for a custom name (their website, 2026-11). */
const TWELVE_CUSTOM_SATS = 5_000;
/** clank.money's price as its documentation states it; Allod reads the real one from the invoice. */
const CLANK_SATS = 999;

export function LightningAddressSettings() {
  const { data, error, mutate } = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher, {
    // While a new name waits for DNS, Allod checks it within seconds.
    refreshInterval: (d) => (d && namesWaiting(d) ? 10_000 : 60_000),
  });
  const { data: doms, mutate: mutateDoms } = useDomains();
  const [dialog, setDialog] = useState(0);
  const [open, setOpen] = useState(false);
  if (error) return <Alert tone="danger" title="Could not load Lightning addresses">{message(error)}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  const update = (v: PublicName) => void mutate(v, { revalidate: false });
  const updateDoms = (v: Domains) => void mutateDoms(v, { revalidate: false });
  const list = items(data, doms);
  return (
    <section className="space-y-3" aria-label="Lightning addresses">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">Lightning addresses</h2>
          <p className="text-sm text-muted">
            Addresses like alice@example.com: share one and anyone can pay you any amount, without an invoice.
          </p>
        </div>
        <Button size="sm" onClick={() => (setDialog((n) => n + 1), setOpen(true))} aria-label="Add a Lightning address">
          <Plus size={16} weight="bold" /> Add
        </Button>
      </div>
      {list.length === 0 ? (
        <Card>
          <CardContent className="space-y-3 pt-6 text-sm">
            <p className="text-base font-semibold">No Lightning address yet</p>
            <p className="text-muted">
              Share an address once, on a website or a Nostr profile, and anyone can pay you any amount, without you making
              an invoice. A few questions find the right kind for you.
            </p>
            <Button onClick={() => (setDialog((n) => n + 1), setOpen(true))}>
              <Plus size={16} weight="bold" /> Get my first address
            </Button>
          </CardContent>
        </Card>
      ) : (
        <AddressList data={data} list={list} onChange={update} onDomains={updateDoms} />
      )}
      <p className="text-xs text-muted">
        Your own domains, DNS providers and gateways are in{" "}
        <Link className="underline" to="/settings/domains">
          Domains
        </Link>{" "}
        and{" "}
        <Link className="underline" to="/settings/gateways">
          Gateways
        </Link>
        .
      </p>
      {open && (
        <AddDialog
          key={dialog}
          open
          data={data}
          doms={doms}
          onClose={() => setOpen(false)}
          onChange={update}
          onDomains={updateDoms}
        />
      )}
    </section>
  );
}

// ------------------------------------------------------------------ list

function AddressList({
  data,
  list,
  onChange,
  onDomains,
}: {
  data: PublicName;
  list: Item[];
  onChange: (v: PublicName) => void;
  onDomains: (v: Domains) => void;
}) {
  const steps = ladder(data);
  const targets = useTargets();
  const name = (t: string) => targets.find((x) => x.id === t)?.name ?? "Closed account";
  const [checking, setChecking] = useState(false);
  const waiting = namesWaiting(data);
  return (
    <Card>
      <CardContent className="space-y-3 pt-6">
        {list.map((i) => (
          <AddressRow
            key={i.key}
            item={i}
            wallet={name(i.target)}
            step={steps.find((s) => s.id === stepId(i))}
            onChange={onChange}
            onDomains={onDomains}
          />
        ))}
        {waiting && (
          <Button
            variant="ghost"
            size="sm"
            disabled={checking}
            onClick={async () => {
              setChecking(true);
              try {
                onChange(await publicName.checkNames());
              } finally {
                setChecking(false);
              }
            }}
          >
            <ArrowsClockwise size={14} /> {checking ? "Checking…" : "Check DNS now"}
          </Button>
        )}
        <SignalLegend />
      </CardContent>
    </Card>
  );
}

/** The ladder step an address sits on (AddressLadder ids). */
function stepId(item: Item): string {
  if (item.own) {
    // Every wallet through a gateway: fully yours; from this machine: its IP is public;
    // BIP-353 only: the own domain alone.
    const { a, d } = item.own;
    if (!d.served || !a.lnurl_enabled) return "domain";
    return d.gateway_id ? "relay" : "domain-lnurl";
  }
  if (item.provider === "zeus" || item.provider === "alby") return "hosted";
  if (item.provider === "clank") return "picked";
  return item.custom ? "picked" : "free";
}

function AddressRow({
  item,
  wallet,
  step,
  onChange,
  onDomains,
}: {
  item: Item;
  wallet: string;
  step: LadderStep | undefined;
  onChange: (v: PublicName) => void;
  onDomains: (v: Domains) => void;
}) {
  const [open, setOpen] = useState(false);
  return (
    <div
      id={`address-${item.key}`}
      role="button"
      tabIndex={0}
      onClick={() => setOpen(true)}
      onKeyDown={(e) => (e.key === "Enter" || e.key === " ") && setOpen(true)}
      className={cn(
        "flex cursor-pointer flex-wrap items-center justify-between gap-2 rounded-lg border p-3 text-sm transition hover:border-accent",
        item.action ? "border-warn/60" : "border-line",
      )}
    >
      <div className="min-w-0 space-y-1">
        <p className="flex flex-wrap items-center gap-2">
          <span className="truncate font-mono font-medium">{item.address}</span>
          {step && <Signals s={step} />}
        </p>
        <p className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
          <span>{wallet}</span>
          <span aria-hidden>·</span>
          <span>{PROVIDER_LABEL[item.provider]}</span>
          <Badge tone={item.status.tone}>{item.status.label}</Badge>
        </p>
        {item.action && (
          <p className="flex items-center gap-1 text-xs text-warn">
            <WarningCircle size={14} weight="fill" /> {item.action}: open it to fix
          </p>
        )}
      </div>
      <span className="flex items-center gap-1" onClick={(e) => e.stopPropagation()}>
        <CopyButton value={item.address} />
        <Button variant="ghost" size="sm" onClick={() => setOpen(true)} aria-label={`Open ${item.address}`}>
          <CaretRight size={16} />
        </Button>
      </span>
      {open && (
        <span onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
          <AddressDetail
            item={item}
            wallet={wallet}
            step={step}
            onChange={onChange}
            onDomains={onDomains}
            onClose={() => setOpen(false)}
          />
        </span>
      )}
    </div>
  );
}

/** One address: its status, what is missing and how to fix it, its settings, and removing it. */
function AddressDetail({
  item,
  wallet,
  step,
  onChange,
  onDomains,
  onClose,
}: {
  item: Item;
  wallet: string;
  step: LadderStep | undefined;
  onChange: (v: PublicName) => void;
  onDomains: (v: Domains) => void;
  onClose: () => void;
}) {
  const [removing, setRemoving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  // ZEUS Pay could not be reached: removing anyway still revokes its connection.
  const [canForce, setCanForce] = useState(false);
  const remove = async (force = false) => {
    setError(null);
    try {
      if (item.own) {
        onDomains(await domainsApi.removeAddress(item.own.a.id));
        onClose();
        return;
      }
      onChange(
        item.provider === "twelve"
            ? await publicName.twelveForget(item.target)
            : item.provider === "zeus"
              ? await publicName.zeusRemove(force)
              : item.alby
                ? await publicName.albyRemove(item.alby.connection_id)
              : await publicName.clankForget(item.target),
      );
      setCanForce(false);
      onClose();
    } catch (e) {
      setError(message(e));
      setCanForce(item.provider === "zeus" && !force);
    } finally {
      setRemoving(false);
    }
  };
  // Own-domain addresses: settings first, their DNS records on a tab of their own.
  const [tab, setTab] = useState<"settings" | "records">("settings");
  const showSettings = !item.own || tab === "settings";
  const trash = item.locked ? null : (
    <Button
      variant="ghost"
      size="sm"
      className="text-danger hover:bg-danger/10"
      onClick={() => setRemoving(true)}
      aria-label="Remove this address"
      title="Remove this address"
    >
      <Trash size={18} />
    </Button>
  );
  const checkNow = async () => {
    setChecking(true);
    try {
      if (item.own) onDomains(await domainsApi.check());
      else onChange(await publicName.checkNames());
    } finally {
      setChecking(false);
    }
  };
  return (
    <Modal open onClose={onClose} title={item.address}>
      <div className="space-y-4 text-sm">
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted">
          <span>{wallet}</span>
          <span aria-hidden>·</span>
          <span>{PROVIDER_LABEL[item.provider]}</span>
          <Badge tone={item.status.tone}>{item.status.label}</Badge>
          <CopyButton value={item.address} />
          {/* Without a Save row (own domain), the remove button ends this line. */}
          {!item.own && <span className="ml-auto">{trash}</span>}
        </div>
        {step && <SignalList s={step} />}

        {item.own && (
          <Tabs
            tabs={[
              { id: "settings" as const, label: "Settings" },
              { id: "records" as const, label: item.own.a.verified ? "DNS records" : "DNS records (to do)" },
            ]}
            value={tab}
            onChange={setTab}
          />
        )}
        {item.own && tab === "settings" && <TargetEditor own={item.own} onDomains={onDomains} />}
        {item.own && tab === "settings" && <NatureEditor own={item.own} onDomains={onDomains} trailing={trash} />}
        {item.own && tab === "records" && (
          <section className="space-y-2">
            <h3 className="font-medium">DNS records of {item.own.d.name}</h3>
            <p className="text-muted">
              {item.own.d.dns_provider_id
                ? "Allod keeps these records at the DNS provider; they show up within minutes."
                : "Add these at your DNS host (the domain must use DNSSEC). Allod checks every few minutes, or use Check now."}
            </p>
            <RecordRows
              records={item.own.d.records.filter(
                (r) => r.type === "A" || (item.own?.a.bip353 && r.name === item.own?.a.record_name),
              )}
              managed={item.own.d.dns_provider_id !== null}
            />
            <p className="text-xs text-muted">
              DNS provider, gateway and the domain's other addresses:{" "}
              <Link className="underline" to="/settings/domains">
                Domains
              </Link>
              .
            </p>
          </section>
        )}
        {item.provider !== "own" && item.action && (
          <section className="space-y-1">
            <h3 className="font-medium">Not live yet</h3>
            <p className="text-muted">
              {item.action}.{" "}
              {item.provider === "alby"
                ? "In your Alby account, connect a wallet with the connection string from Allod, then enter the address here."
                : item.provider === "zeus"
                ? "This usually takes seconds; this node must be online and reachable over Nostr."
                : item.action.startsWith("Waiting")
                  ? "This usually takes seconds to minutes."
                  : ""}
            </p>
          </section>
        )}
        {item.note && showSettings && <p className="text-xs text-muted">{item.note}</p>}
        {(item.own ? tab === "records" : item.action) && (
          <Button variant="secondary" size="sm" disabled={checking} onClick={() => void checkNow()}>
            <ArrowsClockwise size={16} /> {checking ? "Checking…" : "Check now"}
          </Button>
        )}

        {item.zeus && <ZeusTargetEditor zeus={item.zeus} onChange={onChange} />}
        {item.zeus && <ZeusPlusPanel zeus={item.zeus} onChange={onChange} />}
        {item.alby && !item.alby.verified && (
          <AlbyForm entry={item.alby} onChange={onChange} onDone={onChange} back={null} />
        )}

        {error && (
          <Alert tone="danger" title="Could not remove it">
            {error}
            {canForce && (
              <Button size="sm" variant="secondary" className="mt-2" onClick={() => void remove(true)}>
                Remove from Allod anyway
              </Button>
            )}
          </Alert>
        )}

      </div>
      <Modal open={removing} onClose={() => setRemoving(false)} title={`Remove ${item.address}?`}>
        <div className="space-y-4 text-sm">
          <p>
            {item.provider === "alby"
              ? "Allod forgets the address and revokes the connection Alby requests invoices through, so the address stops working. Your Alby account stays; close it on getalby.com if you no longer need it."
              : item.provider === "zeus"
              ? "Allod gives the address up at ZEUS Pay and revokes the connection ZEUS Pay requests invoices through."
              : item.provider === "own"
                ? item.own?.d.dns_provider_id
                  ? "The address stops working; Allod removes its record at the DNS provider and frees the username."
                  : "The address stops working and the username is free again. Delete its TXT record at your DNS host."
                : item.provider === "twelve"
                  ? "Allod forgets the address. twelve.cash cannot delete names, so it keeps paying this wallet's offer until the offer is disabled."
                  : "Allod forgets the name and deletes its management token, so the name can no longer be changed from here. It stays registered at clank.money, pointing at this wallet's offer."}
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRemoving(false)}>
              Cancel
            </Button>
            <Button variant="danger" onClick={() => void remove(false)}>
              Remove
            </Button>
          </div>
        </div>
      </Modal>
    </Modal>
  );
}

/** Which account the node's ZEUS Pay address pays into. Only Allod's connection changes:
 * the address and ZEUS Pay stay the same. */
function ZeusTargetEditor({ zeus, onChange }: { zeus: ZeusEntry; onChange: (v: PublicName) => void }) {
  const targets = useTargets();
  const [target, setTarget] = useState(zeus.target);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <section className="space-y-2">
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onChange(await publicName.zeusTarget(target));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <div className="min-w-48 flex-1">
          <Field label="Pays into">
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
            >
              {targets.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </select>
          </Field>
        </div>
        <Button type="submit" size="sm" variant="secondary" disabled={busy || target === zeus.target}>
          {busy ? "Saving…" : "Change"}
        </Button>
      </form>
      {target !== zeus.target && (
        <p className="text-xs text-muted">
          Payments to {zeus.address} go to the new account from now on; the address itself stays the same.
        </p>
      )}
      {error && <Alert tone="danger" title="Could not change it">{error}</Alert>}
    </section>
  );
}

/** Which account an own-domain address pays into. A change gives it a new offer, owned by
 * the new account (the old one is disabled), so its TXT record changes too. */
function TargetEditor({
  own: { a, d },
  onDomains,
}: {
  own: { a: DomainAddress; d: DomainEntry };
  onDomains: (v: Domains) => void;
}) {
  const targets = useTargets();
  const [target, setTarget] = useState(a.target);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <section className="space-y-2">
      <form
        className="flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onDomains(await domainsApi.updateAddress(a.id, { target }));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <div className="min-w-48 flex-1">
          <Field label="Pays into">
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={target}
              onChange={(e) => setTarget(e.target.value)}
            >
              {targets.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                </option>
              ))}
            </select>
          </Field>
        </div>
        <Button type="submit" size="sm" variant="secondary" disabled={busy || target === a.target}>
          {busy ? "Saving…" : "Change"}
        </Button>
      </form>
      {target !== a.target && (
        <p className="text-xs text-muted">
          The address gets a new offer owned by that account, and the old one stops working.{" "}
          {d.dns_provider_id
            ? "Allod updates its TXT record at the DNS provider."
            : "Then update its TXT record at your DNS host (DNS records tab)."}
        </p>
      )}
      {error && <Alert tone="danger" title="Could not change it">{error}</Alert>}
    </section>
  );
}

/** Who can pay an own-domain address: BIP-353 wallets only, or every wallet (LNURL),
 * which needs the domain served over HTTPS (through a gateway, or from this machine). */
function NatureEditor({
  own: { a, d },
  onDomains,
  trailing,
}: {
  own: { a: DomainAddress; d: DomainEntry };
  onDomains: (v: Domains) => void;
  /** At the right end of the Save row (the remove button). */
  trailing?: ReactNode;
}) {
  const { data: doms } = useDomains();
  const gateways = doms?.gateways ?? [];
  const current: Wallets = a.bip353 && a.lnurl_enabled && d.served ? "both" : a.bip353 ? "bip353" : "lnurl";
  const [choice, setChoice] = useState<Wallets>(current);
  const [serving, setServing] = useState<string>(gateways[0] ? `gateway:${gateways[0].id}` : "direct");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const needsServing = choice !== "bip353" && !d.served;
  const others = d.addresses.filter((x) => x.id !== a.id && x.lnurl_enabled).length;
  const save = async () => {
    setBusy(true);
    setError(null);
    try {
      if (needsServing) {
        await domainsApi.update(
          d.id,
          serving.startsWith("gateway:")
            ? { gateway_id: Number(serving.slice(8)), direct: false }
            : { gateway_id: null, direct: true },
        );
      }
      onDomains(await domainsApi.updateAddress(a.id, { lnurl: choice !== "bip353", bip353: choice !== "lnurl" }));
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const options: { id: Wallets; label: string; body: string }[] = [
    { id: "both", label: "Both", body: "Every wallet (LNURL), and BIP-353 wallets privately through DNS." },
    {
      id: "lnurl",
      label: "LNURL only",
      body: "Every Lightning wallet. No BIP-353 record for it.",
    },
    {
      id: "bip353",
      label: "BIP-353 only",
      body: "Nothing connects to this machine. Only wallets supporting BOLT12/BIP-353 can pay.",
    },
  ];
  return (
    <section className="space-y-2">
      <h3 className="font-medium">Which wallets can pay it</h3>
      {options.map((o) => (
        <label
          key={o.id}
          className={cn(
            "flex cursor-pointer items-start gap-2 rounded-lg border p-3",
            choice === o.id ? "border-accent" : "border-line",
          )}
        >
          <input type="radio" className="mt-1" checked={choice === o.id} onChange={() => setChoice(o.id)} />
          <span>
            <span className="block font-medium">{o.label}</span>
            <span className="block text-xs text-muted">{o.body}</span>
          </span>
        </label>
      ))}
      {choice !== "bip353" && d.served && (
        <p className="text-xs text-muted">
          {d.name} is served over HTTPS {d.gateway_id ? "through a gateway" : "from this machine"} (change it in Domains).
        </p>
      )}
      {needsServing && (
        <div className="space-y-2">
          <Field label={`Serve ${d.name} over HTTPS`}>
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={serving}
              onChange={(e) => setServing(e.target.value)}
            >
              {gateways.map((g) => (
                <option key={g.id} value={`gateway:${g.id}`}>
                  Through gateway {g.label} ({g.ipv4 ?? "being set up"}): home IP stays hidden
                </option>
              ))}
              <option value="direct">From my home IP (your IP becomes public)</option>
            </select>
          </Field>
          {gateways.length === 0 && (
            <p className="text-xs text-muted">
              No gateway yet:{" "}
              <Link className="underline" to="/settings/gateways">
                add one
              </Link>{" "}
              to keep your home IP hidden.
            </p>
          )}
          {others > 0 && (
            <p className="text-xs text-muted">
              The domain's other {others === 1 ? "address answers" : `${others} addresses answer`} LNURL too; change any of
              them in its own dialog.
            </p>
          )}
        </div>
      )}
      {error && <Alert tone="danger" title="Could not change it">{error}</Alert>}
      <div className="flex items-center justify-between gap-2">
        <Button size="sm" disabled={busy || (choice === current && !needsServing)} onClick={() => void save()}>
          {busy ? "Saving…" : "Save"}
        </Button>
        {trailing}
      </div>
    </section>
  );
}

// ------------------------------------------------------------------ add dialog

/** Which wallets an address is for: LNURL (every wallet), BIP-353 (private, newer
 * wallets) or both. */
type Wallets = "both" | "lnurl" | "bip353";

/** A third-party address service. */
type Service = "zeus" | "alby" | "twelve-free" | "twelve-buy" | "clank";

/** What the owner picked so far. */
interface Draft {
  /** A domain id, or a third-party service. */
  where?: number | "third";
  wallets?: Wallets;
  /** How payers reach a domain not served yet: "gateway:<id>" or "direct". */
  serving?: string;
  service?: Service;
  /** "main" or the account id. */
  target?: string;
}

type StepName = "where" | "newDomain" | "buyDomain" | "wallets" | "reach" | "service" | "account" | "finish";

/** The accounts that already have an address of this service (one each). */
function takenFor(data: PublicName, service: Service | undefined): Set<string> {
  if (service === "twelve-free" || service === "twelve-buy") return new Set(data.twelve_cash.map((e) => e.target));
  if (service === "clank") return new Set(data.clank.map((e) => e.target));
  return new Set();
}

/** A username from an account name: "Kids savings" → "kids-savings". */
function slug(s: string): string {
  return s
    .normalize("NFD")
    .replace(/[̀-ͯ]/g, "")
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 32);
}

/** The suggested username: "sats" for Main, the account's name otherwise; empty when taken. */
function suggestName(target: { id: string; name: string } | undefined, taken: Set<string>): string {
  if (!target) return "";
  const s = target.id === "main" ? "sats" : slug(target.name);
  return s && !taken.has(s) ? s : "";
}

function AddDialog({
  open,
  data,
  doms,
  onClose,
  onChange,
  onDomains,
}: {
  open: boolean;
  data: PublicName;
  doms: Domains | undefined;
  onClose: () => void;
  onChange: (v: PublicName) => void;
  onDomains: (v: Domains) => void;
}) {
  const targets = useTargets();
  const [path, setPath] = useState<StepName[]>(["where"]);
  const [draft, setDraft] = useState<Draft>({});
  // A domain added in this dialog, before the domain list reloads.
  const [added, setAdded] = useState<Domains | null>(null);
  const all = added ?? doms;
  const step = path[path.length - 1] ?? "where";
  const domain = typeof draft.where === "number" ? all?.domains.find((d) => d.id === draft.where) : undefined;
  const close = () => onClose();
  const done = (v: PublicName) => {
    onChange(v);
    close();
  };
  const go = (next: StepName, patch: Partial<Draft> = {}) => {
    const d = { ...draft, ...patch };
    // With only Main (and Main free for this service), there is no account to choose.
    if (next === "account" && targets.length === 1 && !takenFor(data, d.service).has("main")) {
      d.target = "main";
      next = "finish";
    }
    setDraft(d);
    setPath((p) => [...p, next]);
  };
  const back =
    path.length > 1 ? (
      <Button variant="ghost" onClick={() => setPath((p) => p.slice(0, -1))}>
        Back
      </Button>
    ) : null;
  const afterWallets = (wallets: Wallets) => {
    // LNURL needs the domain served over HTTPS: ask how, unless it already is.
    if (wallets !== "bip353" && !domain?.served) return go("reach", { wallets });
    go("account", { wallets, serving: undefined });
  };
  const target = targets.find((t) => t.id === draft.target);
  const finishTitle: Record<Service, string> = {
    zeus: "ZEUS Pay address",
    alby: "Alby Lightning address",
    "twelve-free": "Free twelve.cash address",
    "twelve-buy": "Your name on twelve.cash",
    clank: "Your name on clank.money",
  };
  const title = {
    where: "Add a Lightning address",
    newDomain: "Add a domain",
    buyDomain: "Buy a domain",
    wallets: "Which wallets should pay it?",
    reach: `How payers reach ${domain?.name ?? "the domain"}`,
    service: draft.wallets === "lnurl" ? "Choose an LNURL service" : "Choose a BIP-353 registry",
    account: "Which account does it pay into?",
    finish: draft.service ? finishTitle[draft.service] : "Choose the username",
  }[step];
  if (step === "buyDomain" && all) {
    return <BuyDomain
      data={all}
      onChange={(v) => { setAdded(null); onDomains(v); }}
      onClose={() => setPath((p) => p.slice(0, -1))}
      onDone={(v) => {
        setAdded(null);
        onDomains(v);
        setPath(["where"]);
        const bought = v.domains.find((d) => d.name === v.njalla_purchase?.domain);
        if (bought) go("wallets", { where: bought.id, service: undefined });
      }}
    />;
  }
  return (
    <Modal open={open} onClose={close} title={title} wide={step !== "finish" && step !== "newDomain"}>
      {step === "where" && (
        <PickWhere
          doms={all}
          onDomains={onDomains}
          onPick={(where, wallets) => {
            if (where === "new") return go("newDomain");
            if (where === "buy") return go("buyDomain");
            // Third parties: BIP-353 or LNURL was already chosen, so go straight to the services.
            if (where === "third") return go("service", { where, wallets, service: undefined });
            go("wallets", { where, service: undefined });
          }}
        />
      )}
      {step === "newDomain" && (
        <NewDomain
          back={back}
          onAdded={(v, id) => {
            setAdded(v);
            onDomains(v);
            go("wallets", { where: id, service: undefined });
          }}
        />
      )}
      {step === "wallets" && <PickWallets own={draft.where !== "third"} domain={domain} onPick={afterWallets} back={back} />}
      {step === "reach" && domain && (
        <PickReach doms={all} onPick={(serving) => go("account", { serving })} back={back} />
      )}
      {step === "service" && draft.wallets && (
        <PickService
          data={data}
          wallets={draft.wallets}
          onPick={(service) => go("account", { service, target: "main" })}
          back={back}
        />
      )}
      {step === "account" && (
        <PickAccount
          taken={takenFor(data, draft.service)}
          preview={(t) =>
            draft.service === "twelve-free" || draft.service === "zeus" || draft.service === "alby"
              ? null
              : `${suggestName(t, usernamesOf(domain)) || "name"}@${domain?.name ?? (draft.service === "clank" ? "clank.money" : "twelve.cash")}`
          }
          onPick={(t) => go("finish", { target: t })}
          back={back}
        />
      )}
      {step === "finish" && domain && draft.wallets && target && (
        <OwnFinish
          domain={domain}
          wallets={draft.wallets}
          serving={draft.serving}
          gateways={all?.gateways ?? []}
          target={target}
          onDone={(v) => {
            onDomains(v);
            close();
          }}
          back={back}
        />
      )}
      {step === "finish" && draft.service === "zeus" && target && (
        <ZeusForm target={target} onDone={done} back={back} />
      )}
      {step === "finish" && draft.service === "alby" && target && (
        <AlbyForm target={target.id} onChange={onChange} onDone={done} back={back} />
      )}
      {step === "finish" && (draft.service === "twelve-free" || draft.service === "twelve-buy") && target && (
        <TwelveForm
          data={data}
          how={draft.service === "twelve-free" ? "free" : "bought"}
          target={target.id}
          suggested={suggestName(target, new Set())}
          onDone={done}
          back={back}
        />
      )}
      {step === "finish" && draft.service === "clank" && target && (
        <ClankForm target={target.id} suggested={suggestName(target, new Set())} onDone={done} back={back} />
      )}
    </Modal>
  );
}

function usernamesOf(d: DomainEntry | undefined): Set<string> {
  return new Set(d?.addresses.map((a) => a.username) ?? []);
}

/** A big choice button. */
function Choice({ title, body, onClick, disabled }: { title: ReactNode; body: ReactNode; onClick: () => void; disabled?: boolean }) {
  return (
    <button
      type="button"
      disabled={disabled}
      onClick={onClick}
      className="flex w-full items-center justify-between gap-3 rounded-lg border border-line p-4 text-left transition hover:border-accent disabled:opacity-50"
    >
      <span>
        <span className="block font-medium">{title}</span>
        <span className="block text-muted">{body}</span>
      </span>
      <CaretRight size={18} className="shrink-0 text-muted" />
    </button>
  );
}

type Downside = { icon: Icon; tone: "warn" | "danger"; text: string };

function Downsides({ items }: { items: Downside[] }) {
  return <span className="mt-2 block space-y-1 text-xs">
    {items.map(({ icon: IconView, tone, text }) => <span key={text} className="flex items-start gap-1.5">
      <IconView size={15} weight="duotone" className={cn("mt-px shrink-0", tone === "danger" ? "text-danger" : "text-warn")} aria-hidden="true" />
      <span className="text-fg">{text}</span>
    </span>)}
  </span>;
}

const REGISTRY_RISKS: Downside[] = [
  { icon: Wallet, tone: "warn", text: "Only BOLT12/BIP-353 wallets can pay." },
  { icon: ShieldCheck, tone: "warn", text: "Registry could change its public offer; Allod checks the record." },
];

const LNURL_RISKS: Downside[] = [
  { icon: Eye, tone: "danger", text: "Service sees each payment's amount and comment." },
  { icon: ShieldCheck, tone: "danger", text: "Service answers payers and could give them its own invoice." },
];

/** How a domain is reached, in a few words. */
function reachOf(d: DomainEntry, gateways: { id: number; label: string }[]): string {
  if (d.gateway_id) return `Through gateway ${gateways.find((g) => g.id === d.gateway_id)?.label ?? d.gateway_id}`;
  if (d.direct) return "From your home IP";
  return "Not served: BIP-353 only until you choose how payers reach it";
}

/** A link to a service's website inside a card that is clickable as a whole. */
function ServiceLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a
      className="pointer-events-auto underline decoration-accent/40 underline-offset-2 hover:decoration-accent"
      href={href}
      target="_blank"
      rel="noreferrer"
    >
      {children}
    </a>
  );
}

/** A third-party category (BIP-353 or LNURL), picked on the first screen. */
function ThirdPartyOption({ title, body, onClick }: { title: string; body: ReactNode; onClick: () => void }) {
  return (
    <div className="relative rounded-lg border border-line p-3 transition hover:border-accent">
      <button
        type="button"
        onClick={onClick}
        className="absolute inset-0 rounded-lg focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
        aria-label={`Choose ${title}`}
      />
      <span className="pointer-events-none relative z-10 block">
        <span className="flex items-center justify-between gap-3">
          <span className="font-medium">{title}</span>
          <CaretRight size={16} className="shrink-0 text-muted" />
        </span>
        <span className="block text-xs text-muted">{body}</span>
      </span>
    </div>
  );
}

/** Step 1, side by side: an own domain (one of yours, or a new one), or a third-party service. */
function PickWhere({
  doms,
  onDomains,
  onPick,
}: {
  doms: Domains | undefined;
  onDomains: (v: Domains) => void;
  /** Third parties pass the kind of address so the service list comes next, not the wallet step. */
  onPick: (where: number | "third" | "new" | "buy", wallets?: Wallets) => void;
}) {
  const list = doms?.domains ?? [];
  const gateways = doms?.gateways ?? [];
  const row =
    "flex w-full items-center justify-between gap-3 rounded-lg border border-line p-3 text-left transition hover:border-accent";
  return (
    <div className="space-y-3 text-sm">
      <div className="grid gap-3 sm:grid-cols-2">
        <section className="flex flex-col gap-3 rounded-lg border border-line p-4" aria-label="Own domain">
          <div>
            <p className="font-semibold">Your own domain</p>
            <p className="text-muted">Like alice@your-domain. No third party between you and the payer.</p>
          </div>
          <div className="space-y-2">
            {doms?.njalla_purchase && <PurchaseCard p={doms.njalla_purchase} onChange={onDomains} />}
            {list.map((d) => (
              <button key={d.id} type="button" className={row} onClick={() => onPick(d.id)}>
                <span className="min-w-0">
                  <span className="flex items-center gap-2 font-medium">
                    <span className="truncate">{d.name}</span>
                    {d.live ? <Badge tone="ok">live</Badge> : <Badge>not live yet</Badge>}
                  </span>
                  <span className="block text-xs text-muted">
                    {reachOf(d, gateways)} · {d.addresses.length} address{d.addresses.length === 1 ? "" : "es"}
                  </span>
                </span>
                <CaretRight size={16} className="shrink-0 text-muted" />
              </button>
            ))}
            <button type="button" className={cn(row, "border-dashed")} onClick={() => onPick("new")}>
              <span className="flex items-center gap-2 font-medium">
                <Plus size={16} /> Add a domain you own
              </span>
              <CaretRight size={16} className="shrink-0 text-muted" />
            </button>
            {!doms?.njalla_purchase && (
              <button type="button" className={cn(row, "border-dashed")} onClick={() => onPick("buy")} disabled={!doms}>
                <span className="flex items-center gap-2 font-medium">
                  <Globe size={16} /> Buy a domain
                </span>
                <CaretRight size={16} className="shrink-0 text-muted" />
              </button>
            )}
          </div>
          <p className="mt-auto text-xs text-muted">
            DNS providers and gateways:{" "}
            <Link className="underline" to="/settings/domains">
              Domains
            </Link>
            .
          </p>
        </section>
        <section className="flex flex-col gap-3 rounded-lg border border-line p-4" aria-label="A third-party service">
          <div>
            <p className="font-semibold">A third-party service</p>
            <p className="text-muted">No domain needed. Pick the kind of address, then the service.</p>
          </div>
          <div className="space-y-2">
            <ThirdPartyOption
              title="BIP-353"
              onClick={() => onPick("third", "bip353")}
              body={
                <>
                  <span className="font-medium">
                    <ServiceLink href="https://twelve.cash">twelve.cash</ServiceLink> or{" "}
                    <ServiceLink href="https://clank.money">clank.money</ServiceLink>
                  </span>
                  <Downsides items={REGISTRY_RISKS} />
                </>
              }
            />
            <ThirdPartyOption
              title="LNURL"
              onClick={() => onPick("third", "lnurl")}
              body={
                <>
                  <span className="font-medium">
                    <ServiceLink href="https://zeusln.com">ZEUS Pay</ServiceLink> or{" "}
                    <ServiceLink href="https://getalby.com/">Alby</ServiceLink>
                  </span>
                  <Downsides items={LNURL_RISKS} />
                </>
              }
            />
          </div>
        </section>
      </div>
    </div>
  );
}

/** Adds a domain, then goes on with it. */
function NewDomain({ onAdded, back }: { onAdded: (v: Domains, id: number) => void; back: ReactNode }) {
  const [name, setName] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          const n = name.trim().toLowerCase();
          const v = await domainsApi.add({ name: n });
          const d = v.domains.find((x) => x.name === n) ?? v.domains[v.domains.length - 1];
          if (!d) throw new Error("the domain was not added");
          onAdded(v, d.id);
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <Field
        label="Domain"
        hint="A domain you control. For BIP-353 its DNS must use DNSSEC. Its DNS provider is set in Domains."
      >
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="pay.example.com" required autoFocus />
      </Field>
      {error && <Alert tone="danger" title="Could not add the domain">{error}</Alert>}
      <FormFooter back={back} busy={busy} label="Continue" disabled={!name.trim()} />
    </form>
  );
}

/** Step 2: LNURL, BIP-353 or both. */
function PickWallets({
  own,
  domain,
  onPick,
  back,
}: {
  own: boolean;
  domain: DomainEntry | undefined;
  onPick: (w: Wallets) => void;
  back: ReactNode;
}) {
  const serve = domain?.served ? "" : " You choose next how payers reach the domain: a gateway or your home IP.";
  return (
    <div className="space-y-3 text-sm">
      <Choice
        title="Both"
        body={
          own
            ? `Every wallet can pay (LNURL), and wallets with BIP-353 resolve it privately through DNS.${serve}`
            : "No service gives both on one name: add one of each, one after the other."
        }
        disabled={!own}
        onClick={() => onPick("both")}
      />
      <Choice
        title="LNURL only"
        body={
          own
            ? `Every Lightning wallet can pay. Needs the domain served over HTTPS.${serve}`
            : "Every Lightning wallet can pay. The service answers payers and sees every payment."
        }
        onClick={() => onPick("lnurl")}
      />
      <Choice
        title="BIP-353 only"
        body={
          own
            ? "Most private: nothing connects to this machine. Only wallets with BOLT12/BIP-353 can pay."
            : "A name on a registry that never sees your payments. Only wallets with BOLT12/BIP-353 can pay."
        }
        onClick={() => onPick("bip353")}
      />
      {back && <div className="flex justify-start">{back}</div>}
    </div>
  );
}

/** Own domain with LNURL: through a gateway, or from the home IP. */
function PickReach({ doms, onPick, back }: { doms: Domains | undefined; onPick: (serving: string) => void; back: ReactNode }) {
  const gateways = doms?.gateways ?? [];
  return (
    <div className="space-y-3 text-sm">
      {gateways.map((g) => (
        <Choice
          key={g.id}
          title={`Through gateway ${g.label}`}
          body={`${g.ipv4 ?? "being set up"} · your home IP stays hidden; it passes payments through encrypted.`}
          onClick={() => onPick(`gateway:${g.id}`)}
        />
      ))}
      <Choice
        title="From my home IP"
        body="Anyone who sees the address can learn where this node runs. Port 443 must be forwarded here, and it does not work behind CGNAT."
        onClick={() => onPick("direct")}
      />
      {gateways.length === 0 && (
        <p className="px-1 text-xs text-muted">
          No gateway yet:{" "}
          <Link className="underline" to="/settings/gateways">
            add one in Gateways
          </Link>{" "}
          to keep your home IP hidden (your own server, or one Allod rents).
        </p>
      )}
      {back && <div className="flex justify-start">{back}</div>}
    </div>
  );
}

/** Third parties: the services that give what was chosen. */
function PickService({
  data,
  wallets,
  onPick,
  back,
}: {
  data: PublicName;
  wallets: Wallets;
  onPick: (s: Service) => void;
  back: ReactNode;
}) {
  const mainnet = "Mainnet only";
  return (
    <div className="space-y-3 text-sm">
      {wallets !== "lnurl" && (
        <>
          <p className="text-muted">
            BIP-353 registries publish your name pointing at this wallet's own offer: they never see your payments.
          </p>
          <Choice
            title="twelve.cash · free random name"
            body={<><span>{data.twelve_cash_available ? "Like brave.penguin@twelve.cash." : mainnet}</span><Downsides items={[...REGISTRY_RISKS, { icon: Warning, tone: "warn", text: "Random name; cannot choose, change or delete it." }]} /></>}
            disabled={!data.twelve_cash_available}
            onClick={() => onPick("twelve-free")}
          />
          <Choice
            title={`twelve.cash · your name · ${TWELVE_CUSTOM_SATS.toLocaleString()} sats`}
            body={<><span>{data.twelve_cash_available ? "Like satoshi@twelve.cash, paid by Allod." : mainnet}</span><Downsides items={[...REGISTRY_RISKS, { icon: Warning, tone: "warn", text: "One-off payment; name cannot be changed or deleted." }]} /></>}
            disabled={!data.twelve_cash_available}
            onClick={() => onPick("twelve-buy")}
          />
          <Choice
            title={`clank.money · your name · ~${CLANK_SATS.toLocaleString()} sats`}
            body={<><span>{data.clank_available ? "Like mynode@clank.money, paid by Allod. You can point it elsewhere later." : mainnet}</span><Downsides items={[...REGISTRY_RISKS, { icon: Warning, tone: "warn", text: "One-off payment; registry keeps the name." }]} /></>}
            disabled={!data.clank_available}
            onClick={() => onPick("clank")}
          />
        </>
      )}
      {wallets !== "bip353" && (
        <>
          <p className="text-muted">
            LNURL services ask this node for each invoice over Nostr Wallet Connect, so your IP stays private. They see
            every payment and could hand payers their own invoice.
          </p>
          <Choice
            title="ZEUS Pay · free"
            body={<><span>{!data.zeus_available
                ? mainnet
                : data.zeus
                  ? "Main already has one (one per node)."
                  : "A random name@zeuspay.com, set up in one step. Choosing the name costs ZEUS Pay+ (yearly). One per node."
            }</span><Downsides items={[...LNURL_RISKS, { icon: Warning, tone: "warn", text: "One address per node; chosen name needs a yearly plan." }]} /></>}
            disabled={!data.zeus_available || !!data.zeus}
            onClick={() => onPick("zeus")}
          />
          <Choice
            title="Alby · free"
            body={<><span>{data.alby_available ? "you@getalby.com, any free name." : mainnet}</span><Downsides items={[...LNURL_RISKS, { icon: Warning, tone: "warn", text: "Needs an Alby account and setup on getalby.com." }]} /></>}
            disabled={!data.alby_available}
            onClick={() => onPick("alby")}
          />
        </>
      )}
      {back && <div className="flex justify-start">{back}</div>}
    </div>
  );
}

/** Main or an account. */
function PickAccount({
  taken,
  preview,
  onPick,
  back,
}: {
  taken: Set<string>;
  preview: (t: { id: string; name: string }) => string | null;
  onPick: (target: string) => void;
  back: ReactNode;
}) {
  const targets = useTargets();
  return (
    <div className="space-y-3 text-sm">
      {targets.map((t) => (
        <Choice
          key={t.id}
          title={t.name}
          body={taken.has(t.id) ? "Already has one." : (preview(t) ?? (t.id === "main" ? "The main wallet." : "An account."))}
          disabled={taken.has(t.id)}
          onClick={() => onPick(t.id)}
        />
      ))}
      {back && <div className="flex justify-start">{back}</div>}
    </div>
  );
}

function FormFooter({ back, busy, label, disabled }: { back: ReactNode; busy: boolean; label: string; disabled?: boolean }) {
  return (
    <div className="flex justify-between gap-2">
      {back ?? <span />}
      <Button type="submit" disabled={busy || disabled}>
        {busy ? "Working…" : label}
      </Button>
    </div>
  );
}

/** Last step on an own domain: the username, suggested from the account. */
function OwnFinish({
  domain,
  wallets,
  serving,
  gateways,
  target,
  onDone,
  back,
}: {
  domain: DomainEntry;
  wallets: Wallets;
  serving: string | undefined;
  gateways: { id: number; label: string }[];
  target: { id: string; name: string };
  onDone: (v: Domains) => void;
  back: ReactNode;
}) {
  const taken = usernamesOf(domain);
  const [username, setUsername] = useState(() => suggestName(target, taken));
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const user = username.trim().toLowerCase();
  const kinds = wallets === "both" ? "LNURL and BIP-353" : wallets === "lnurl" ? "LNURL" : "BIP-353";
  const reach = serving
    ? serving === "direct"
      ? "from your home IP"
      : `through gateway ${gateways.find((g) => `gateway:${g.id}` === serving)?.label ?? ""}`
    : null;
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          if (serving) {
            await domainsApi.update(
              domain.id,
              serving.startsWith("gateway:")
                ? { gateway_id: Number(serving.slice(8)), direct: false }
                : { gateway_id: null, direct: true },
            );
          }
          onDone(await domainsApi.addAddress(domain.id, user, target.id, { lnurl: wallets !== "bip353", bip353: wallets !== "lnurl" }));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <Field
        label="Username"
        hint={taken.has(user) ? "Taken on this domain." : "Lowercase letters, digits, '.', '_' or '-'."}
      >
        <div className="flex items-center gap-2">
          <Input
            value={username}
            onChange={(e) => setUsername(e.target.value.toLowerCase())}
            placeholder="alice"
            required
            autoFocus
            autoComplete="off"
            name="lightning-username"
            data-1p-ignore
            data-lpignore="true"
          />
          <span className="shrink-0 font-mono text-muted">@{domain.name}</span>
        </div>
      </Field>
      <p className="text-muted">
        Pays into {target.name}. {kinds}
        {reach ? `, ${domain.name} reached ${reach}` : ""}.{" "}
        {wallets === "lnurl"
          ? "No DNS record for the address itself; only the domain's A record."
          : domain.dns_provider_id
            ? "Allod adds its records at the DNS provider."
            : "After saving, Allod shows the records to add at your DNS host."}
      </p>
      {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
      <FormFooter back={back} busy={busy} label="Add address" disabled={!user || taken.has(user)} />
    </form>
  );
}

function TwelveForm({
  data,
  how,
  target,
  suggested,
  onDone,
  back,
}: {
  data: PublicName;
  how: "free" | "bought";
  target: string;
  suggested: string;
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  // A name bought on the website earlier is linked instead of bought here.
  const [linking, setLinking] = useState(false);
  if (how === "bought" && !linking) {
    return (
      <TwelveBuy target={target} suggested={suggested} onDone={onDone} back={back} onLink={() => setLinking(true)} />
    );
  }
  return <TwelveLinkOrFree data={data} how={how} target={target} onDone={onDone} back={back} />;
}

function TwelveLinkOrFree({
  how,
  target,
  onDone,
  back,
}: {
  data: PublicName;
  how: "free" | "bought";
  target: string;
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const [address, setAddress] = useState("");
  // The offer is made for the chosen name (its description is the name), so the name comes first.
  const [chosen, setChosen] = useState<string | null>(null);
  const { data: offer, error: offerError } = useSWR(how === "bought" && chosen ? ["twelve-offer", target, chosen] : null, () =>
    publicName.twelveOffer(target, chosen ?? ""),
  );
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        if (how === "bought" && !chosen) {
          setChosen(address.trim().toLowerCase().replace(/^₿/, ""));
          return;
        }
        setBusy(true);
        setError(null);
        try {
          onDone(await publicName.twelveAdd(target, how === "bought" ? (chosen ?? undefined) : undefined));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <p className="text-muted">
        {how === "free"
          ? "A random name like brave.penguin@twelve.cash, registered right away. Free; it cannot be changed or deleted."
          : "Link a name you already bought on the twelve.cash website: Allod shows the offer it must carry, then checks it."}
      </p>
      {how === "bought" && (
        <>
          <Field label="The name you want" hint="Payers see it as the description of the payment.">
            <Input
              value={address}
              onChange={(e) => {
                setAddress(e.target.value);
                setChosen(null);
              }}
              placeholder="satoshi@twelve.cash"
              required
            />
          </Field>
          {chosen && (
            <div className="space-y-2">
              <p>
                Buy <span className="font-mono">{chosen}</span> at{" "}
                <a className="underline" href="https://twelve.cash" target="_blank" rel="noreferrer">
                  twelve.cash
                </a>{" "}
                with this BOLT12 offer as its payment option, then link it. Allod checks that it points at this offer.
              </p>
              {offer ? (
                <div className="flex items-center gap-2">
                  <span className="min-w-0 flex-1 truncate font-mono text-xs">{offer.offer}</span>
                  <CopyButton value={offer.offer} />
                </div>
              ) : (
                <p className={offerError ? "text-danger" : "text-muted"}>{offerError ? message(offerError) : "Loading…"}</p>
              )}
            </div>
          )}
        </>
      )}
      {error && <Alert tone="danger" title="twelve.cash">{error}</Alert>}
      <FormFooter
        back={back}
        busy={busy}
        label={how === "free" ? "Get a free address" : chosen ? "Link it" : "Show the offer"}
        disabled={how === "bought" && !!chosen && !offer}
      />
    </form>
  );
}

/** An invoice the node could not pay itself (insufficient balance), to pay elsewhere. */
function InvoicePanel({
  invoice,
  hint,
  onPaid,
  onProof,
}: {
  invoice: string;
  hint: string;
  /** For twelve.cash: check whether the service has seen the external payment. */
  onPaid?: () => Promise<void>;
  /** For clank.money: the L402 payment proof the service needs to register. */
  onProof?: (preimage: string) => Promise<void>;
}) {
  const [preimage, setPreimage] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const proof = preimage.trim();
  const valid = /^[0-9a-fA-F]{64}$/.test(proof);
  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await fn();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-3 rounded-lg border border-line p-4">
      <p className="font-medium">Pay from another wallet</p>
      <p className="text-xs text-muted">{hint}</p>
      <QrBlock value={invoice} caption="Scan it, or copy the invoice into the wallet that has the sats." />
      {onPaid && (
        <Button variant="secondary" size="sm" disabled={busy} onClick={() => void run(onPaid)}>
          I paid it — check now
        </Button>
      )}
      {onProof && (
        <div className="space-y-2">
          <Field
            label="Payment preimage"
            hint="The 64-character proof the wallet shows after paying. Allod sends it to clank.money."
          >
            <Input
              value={preimage}
              onChange={(e) => setPreimage(e.target.value)}
              placeholder="64 hex characters"
              spellCheck={false}
              autoComplete="off"
            />
          </Field>
          <Button size="sm" disabled={busy || !valid} onClick={() => void run(() => onProof(proof))}>
            Finish registration
          </Button>
        </div>
      )}
      {error && <Alert tone="danger" title="The name is not registered yet">{error}</Alert>}
    </div>
  );
}

function ClankForm({
  target,
  suggested,
  onDone,
  back,
}: {
  target: string;
  suggested: string;
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const targets = useTargets();
  const [username, setUsername] = useState(suggested);
  const [maxPrice, setMaxPrice] = useState("1000");
  const [fromMain, setFromMain] = useState(false);
  const [check, setCheck] = useState<{ valid: boolean; available: boolean | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [invoice, setInvoice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const user = username.trim().toLowerCase();
  const max = Number.parseInt(maxPrice.replace(/[^0-9]/g, ""), 10) || 0;
  const wallet = targets.find((t) => t.id === target)?.name ?? "";
  useEffect(() => {
    if (suggested) void publicName.clankAvailable(suggested).then(setCheck, () => setCheck(null));
  }, [suggested]);
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        setInvoice(null);
        try {
          onDone(
            await publicName.clankBuy({
              target,
              username: user,
              max_price_sats: max,
              pay_from: fromMain || target === "main" ? "main" : target,
            }),
          );
        } catch (err) {
          setError(message(err));
          setInvoice(err instanceof ApiError ? (err.invoice ?? null) : null);
        } finally {
          setBusy(false);
        }
      }}
    >
      <Field
        label="Name"
        hint={
          check && !check.valid
            ? "3–32 lowercase letters, digits or hyphens; no hyphen at the start or end."
            : check?.available === false
              ? "Taken."
              : check?.available
                ? "Available."
                : "First come, first served."
        }
      >
        <div className="flex items-center gap-2">
          <Input
            value={username}
            onChange={(e) => {
              setUsername(e.target.value);
              setCheck(null);
            }}
            onBlur={async () => {
              if (user) setCheck(await publicName.clankAvailable(user).catch(() => null));
            }}
            placeholder="mynode"
            required
          />
          <span className="shrink-0 font-mono text-muted">@clank.money</span>
        </div>
      </Field>
      <Field label="Pay at most (sats)" hint="Allod checks the invoice and stops if it costs more.">
        <Input inputMode="numeric" value={maxPrice} onChange={(e) => setMaxPrice(e.target.value)} />
      </Field>
      {target !== "main" && (
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={fromMain} onChange={(e) => setFromMain(e.target.checked)} />
          Pay from Main instead of {wallet}
        </label>
      )}
      <p className="text-muted">
        Allod makes a BOLT12 offer for this name (described <span className="font-mono">₿{user || "name"}@clank.money</span>
        ), pays clank.money and registers it. The payment cannot be refunded.
      </p>
      {error && <Alert tone="danger" title="Could not buy the name">{error}</Alert>}
      {invoice && (
        <InvoicePanel
          invoice={invoice}
          hint="clank.money needs both the payment and its proof. Pay it from another wallet, then paste the payment preimage it shows."
          onProof={async (preimage) => {
            const v = await publicName.clankProof({ target, preimage });
            const entry = v.clank.find((e) => e.target === target);
            // Keep the invoice at hand if clank.money has not registered it yet.
            if (entry && entry.state !== "registered") {
              throw new Error(entry.error ?? "clank.money has not confirmed the registration yet");
            }
            onDone(v);
          }}
        />
      )}
      <FormFooter
        back={back}
        busy={busy}
        label={`Buy for up to ${max.toLocaleString()} sats`}
        disabled={!user || max <= 0 || check?.valid === false || check?.available === false}
      />
    </form>
  );
}

const ZEUS_DOMAINS = ["zeuspay.com", "zeusnuts.com"] as const;

function ZeusForm({
  target,
  onDone,
  back,
}: {
  target: { id: string; name: string };
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const { mutate } = useSWRConfig();
  const [how, setHow] = useState<"free" | "plus">("free");
  const [handle, setHandle] = useState("");
  const [domain, setDomain] = useState<string>(ZEUS_DOMAINS[0]);
  // Paid path, after the free address exists: ZEUS's price, to confirm before paying.
  const [quote, setQuote] = useState<{ view: PublicName; fee: number } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const name = handle.trim().toLowerCase();
  const fail = (err: unknown) => {
    setError(message(err));
    void mutate(PUBLIC_NAME_KEY);
  };
  const getFree = async () => {
    setBusy("Getting the address…");
    setError(null);
    try {
      return await publicName.zeusAdd(target.id);
    } finally {
      setBusy(null);
    }
  };
  const pay = async (fee: number) => {
    setError(null);
    try {
      setBusy(`Paying ${fee.toLocaleString()} sats…`);
      await publicName.zeusBuyPlus(fee);
      setBusy("Renaming…");
      onDone(await publicName.zeusRename(name, domain));
    } catch (err) {
      fail(err);
    } finally {
      setBusy(null);
      setQuote(null);
    }
  };
  if (quote) {
    return (
      <div className="space-y-4 text-sm">
        <p>
          Main now has the free address <span className="font-mono">{quote.view.zeus?.address}</span>.
        </p>
        <p>
          To change it to <span className="font-mono">{name}@{domain}</span>, ZEUS Pay+ costs{" "}
          <span className="font-medium">{quote.fee.toLocaleString()} sats a year</span>, paid from Main. Allod checks the
          invoice is for exactly that amount. The payment cannot be refunded.
        </p>
        {error && <Alert tone="danger" title="ZEUS Pay+">{error}</Alert>}
        <div className="flex justify-between gap-2">
          <Button variant="ghost" disabled={!!busy} onClick={() => onDone(quote.view)}>
            Keep the free name
          </Button>
          <Button disabled={!!busy} onClick={() => void pay(quote.fee)}>
            {busy ?? `Pay ${quote.fee.toLocaleString()} sats`}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        try {
          const view = await getFree();
          if (how === "free") return onDone(view);
          const fee = view.zeus?.plus_fee_sats;
          if (!fee) {
            setError("ZEUS Pay did not say what ZEUS Pay+ costs. The free address is ready; try ZEUS Pay+ from the list.");
            void mutate(PUBLIC_NAME_KEY, view, { revalidate: false });
            return;
          }
          setQuote({ view, fee });
        } catch (err) {
          fail(err);
        }
      }}
    >
      <fieldset className="space-y-2">
        <label className="flex cursor-pointer items-start gap-2 rounded-lg border border-line p-3">
          <input type="radio" className="mt-1" checked={how === "free"} onChange={() => setHow("free")} />
          <span>
            <span className="font-medium">Free, with a random name</span>
            <span className="block text-muted">Like rebellioustree35@zeuspay.com. Nothing to choose or pay.</span>
          </span>
        </label>
        <label className="flex cursor-pointer items-start gap-2 rounded-lg border border-line p-3">
          <input type="radio" className="mt-1" checked={how === "plus"} onChange={() => setHow("plus")} />
          <span>
            <span className="font-medium">Choose your name (ZEUS Pay+, paid yearly)</span>
            <span className="block text-muted">
              Allod first gets the free address, then shows ZEUS Pay's exact yearly price and pays it from Main only after
              you confirm.
            </span>
          </span>
        </label>
      </fieldset>
      {how === "plus" && (
        <Field label="Your name">
          <div className="flex items-center gap-2">
            <Input
              value={handle}
              onChange={(e) => setHandle(e.target.value.toLowerCase())}
              placeholder="mynode"
              required
              autoComplete="off"
              name="lightning-username"
              data-1p-ignore
              data-lpignore="true"
            />
            <select
              className="h-10 rounded-lg border border-line bg-surface px-2 font-mono text-sm"
              value={domain}
              onChange={(e) => setDomain(e.target.value)}
              aria-label="Domain"
            >
              {ZEUS_DOMAINS.map((d) => (
                <option key={d} value={d}>
                  @{d}
                </option>
              ))}
            </select>
          </div>
        </Field>
      )}
      <p className="text-muted">
        Allod creates a receive-only wallet connection for ZEUS Pay (it can make invoices, never pay) and proves it owns
        this node by signing ZEUS Pay's challenge with the node key. Payments go to {target.name}; ZEUS Pay+ is paid from Main. ZEUS Pay is a third party: it
        sees payment amounts and comments, and this node must be online to receive.
      </p>
      {error && <Alert tone="danger" title="ZEUS Pay">{error}</Alert>}
      <FormFooter
        back={back}
        busy={!!busy}
        label={how === "free" ? "Get a free address" : "Continue to the price"}
        disabled={how === "plus" && !/^[a-z0-9._-]{1,64}$/.test(name)}
      />
    </form>
  );
}

function dateOf(t: number): string {
  return new Date(t * 1000).toLocaleDateString(undefined, { year: "numeric", month: "long", day: "numeric" });
}

/** ZEUS Pay+: buy or extend it (paid from Main), automatic renewal, and renaming. */
function ZeusPlusPanel({ zeus, onChange }: { zeus: ZeusEntry; onChange: (v: PublicName) => void }) {
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [handle, setHandle] = useState(zeus.handle);
  const [domain, setDomain] = useState(zeus.domain);
  const [renewMax, setRenewMax] = useState(String(zeus.renew_max_sats ?? zeus.plus_fee_sats ?? ""));
  const fee = zeus.plus_fee_sats;
  const run = async (f: () => Promise<PublicName>) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await f());
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const max = Number.parseInt(renewMax.replace(/[^0-9]/g, ""), 10) || 0;
  return (
    <div className="space-y-3 rounded-lg bg-surface-2 p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <p className="font-medium">
          ZEUS Pay+{" "}
          {zeus.plus_active && zeus.plus_expires_at ? (
            <Badge tone="ok">until {dateOf(zeus.plus_expires_at)}</Badge>
          ) : (
            <Badge>not active</Badge>
          )}
        </p>
        <span className="flex gap-1">
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => void run(publicName.zeusRefresh)}>
            <ArrowsClockwise size={14} /> Refresh
          </Button>
          <Button size="sm" disabled={busy || !fee} onClick={() => setConfirm(true)}>
            {zeus.plus_active ? "Extend a year" : "Get ZEUS Pay+"}
            {fee ? ` · ${fee.toLocaleString()} sats` : ""}
          </Button>
        </span>
      </div>
      {!zeus.plus_active && (
        <p className="text-xs text-muted">
          A yearly ZEUS Pay subscription that lets you choose your own name and domain. Without it the address keeps the
          name ZEUS Pay picked, and keeps working.
        </p>
      )}
      {zeus.plus_error && <p className="text-xs text-warn">{zeus.plus_error}</p>}
      {zeus.plus_active && (
        <>
          <form
            className="flex flex-wrap items-end gap-2"
            onSubmit={(e) => {
              e.preventDefault();
              void run(() => publicName.zeusRename(handle.trim().toLowerCase(), domain));
            }}
          >
            <Field label="Name">
              <div className="flex items-center gap-2">
                <Input
                  value={handle}
                  onChange={(e) => setHandle(e.target.value.toLowerCase())}
                  autoComplete="off"
                  name="lightning-username"
                  data-1p-ignore
                  data-lpignore="true"
                />
                <select
                  className="h-10 rounded-lg border border-line bg-surface px-2 font-mono text-sm"
                  value={domain}
                  onChange={(e) => setDomain(e.target.value)}
                  aria-label="Domain"
                >
                  {ZEUS_DOMAINS.map((d) => (
                    <option key={d} value={d}>
                      @{d}
                    </option>
                  ))}
                </select>
              </div>
            </Field>
            <Button type="submit" size="sm" variant="secondary" disabled={busy || (handle === zeus.handle && domain === zeus.domain)}>
              Rename
            </Button>
          </form>
          <div className="flex flex-wrap items-end gap-2">
            <label className="flex items-center gap-2 pb-2">
              <input
                type="checkbox"
                checked={zeus.auto_renew}
                disabled={busy || (!zeus.auto_renew && max <= 0)}
                onChange={(e) => void run(() => publicName.zeusAutoRenew(e.target.checked, max || undefined))}
              />
              Renew automatically, paying at most
            </label>
            <Input
              className="w-28"
              inputMode="numeric"
              value={renewMax}
              onChange={(e) => setRenewMax(e.target.value)}
              onBlur={() => {
                if (zeus.auto_renew && max > 0 && max !== zeus.renew_max_sats) void run(() => publicName.zeusAutoRenew(true, max));
              }}
              aria-label="Most to pay for a renewal, in sats"
            />
            <span className="pb-2 text-muted">sats a year</span>
          </div>
          {zeus.auto_renew && (
            <p className="text-xs text-muted">
              Allod pays from Main up to 3 days before the end, and stops if the price went above your limit.
            </p>
          )}
        </>
      )}
      {error && <Alert tone="danger" title="ZEUS Pay+">{error}</Alert>}
      <Modal open={confirm} onClose={() => setConfirm(false)} title={zeus.plus_active ? "Extend ZEUS Pay+?" : "Get ZEUS Pay+?"}>
        <div className="space-y-4 text-sm">
          <p>
            Allod pays ZEUS Pay <span className="font-medium">{fee?.toLocaleString()} sats</span> from Main for one year
            {zeus.plus_active ? " more" : ""}. It checks that the invoice is for exactly that amount. The payment cannot be
            refunded.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirm(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => {
                setConfirm(false);
                if (fee) void run(() => publicName.zeusBuyPlus(fee));
              }}
            >
              Pay {fee?.toLocaleString()} sats
            </Button>
          </div>
        </div>
      </Modal>
    </div>
  );
}

/** Buys a chosen twelve.cash name: Allod pays the 5,000-sat checkout from the wallet. */
function TwelveBuy({
  target,
  suggested,
  onDone,
  back,
  onLink,
}: {
  target: string;
  suggested: string;
  onDone: (v: PublicName) => void;
  back: ReactNode;
  onLink: () => void;
}) {
  const targets = useTargets();
  const [username, setUsername] = useState(suggested);
  const [fromMain, setFromMain] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [invoice, setInvoice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const user = username.trim().replace(/^₿/, "").replace(/@twelve\.cash$/i, "");
  const valid = /^[a-zA-Z0-9._-]{4,64}$/.test(user);
  const wallet = targets.find((t) => t.id === target)?.name ?? "";
  const buy = async () => {
    setBusy(true);
    setError(null);
    setInvoice(null);
    try {
      onDone(
        await publicName.twelveBuy({
          target,
          username: user,
          max_price_sats: TWELVE_CUSTOM_SATS,
          pay_from: fromMain || target === "main" ? "main" : target,
        }),
      );
    } catch (e) {
      setError(message(e));
      setInvoice(e instanceof ApiError ? (e.invoice ?? null) : null);
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={(e) => {
        e.preventDefault();
        setConfirm(true);
      }}
    >
      <Field label="Name" hint="At least 4 letters, digits, '.', '-' or '_'.">
        <div className="flex items-center gap-2">
          <Input
            value={username}
            onChange={(e) => setUsername(e.target.value)}
            placeholder="satoshi"
            required
            autoComplete="off"
            name="lightning-username"
            data-1p-ignore
            data-lpignore="true"
          />
          <span className="shrink-0 font-mono text-muted">@twelve.cash</span>
        </div>
      </Field>
      {target !== "main" && (
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={fromMain} onChange={(e) => setFromMain(e.target.checked)} />
          Pay from Main instead of {wallet}
        </label>
      )}
      <p className="text-muted">
        Allod makes a BOLT12 offer for this name, pays twelve.cash {TWELVE_CUSTOM_SATS.toLocaleString()} sats and has it
        published once twelve.cash confirms the payment. The payment cannot be refunded.
      </p>
      {error && <Alert tone="danger" title="Could not buy the name">{error}</Alert>}
      {invoice && (
        <InvoicePanel
          invoice={invoice}
          hint="Pay it from another wallet. Allod finishes the purchase as soon as twelve.cash sees the payment."
          onPaid={async () => {
            const v = await publicName.refresh();
            // Not seen yet: keep the invoice on screen so it can be paid or checked again.
            const pending = v.twelve_cash.find((e) => e.target === target)?.purchase;
            if (pending) {
              throw new Error(
                pending.error ?? "twelve.cash has not seen the payment yet; try again in a moment",
              );
            }
            onDone(v);
          }}
        />
      )}
      <div className="flex flex-wrap items-center justify-between gap-2">
        {back}
        <span className="flex items-center gap-2">
          <Button variant="ghost" size="sm" onClick={onLink}>
            Already bought one? Link it
          </Button>
          <Button type="submit" disabled={busy || !valid}>
            {busy ? "Paying…" : `Buy for ${TWELVE_CUSTOM_SATS.toLocaleString()} sats`}
          </Button>
        </span>
      </div>
      <Modal open={confirm} onClose={() => setConfirm(false)} title={`Buy ${user}@twelve.cash?`}>
        <div className="space-y-4 text-sm">
          <p>
            Allod pays twelve.cash <span className="font-medium">{TWELVE_CUSTOM_SATS.toLocaleString()} sats</span> from{" "}
            {fromMain || target === "main" ? "Main" : wallet}. It checks the invoice is for exactly that amount. The name
            cannot be changed or deleted later, and the payment cannot be refunded.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirm(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => {
                setConfirm(false);
                void buy();
              }}
            >
              Pay {TWELVE_CUSTOM_SATS.toLocaleString()} sats
            </Button>
          </div>
        </div>
      </Modal>
    </form>
  );
}

/** Alby: Allod gives a receive-only connection string; the owner sets the address up on
 * getalby.com, then enters it, and Allod checks it asks this node for invoices. */
function AlbyForm({
  entry,
  target: chosen,
  onChange,
  onDone,
  back,
}: {
  /** Finish this one (from its dialog) instead of adding a new one. */
  entry?: AlbyEntry;
  /** The account a new one pays into. */
  target?: string;
  onChange: (v: PublicName) => void;
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const { mutate } = useSWRConfig();
  const targets = useTargets();
  const target = entry?.target ?? chosen ?? "main";
  const [uri, setUri] = useState<string | null>(null);
  const [connection, setConnection] = useState<number | null>(entry?.connection_id ?? null);
  const [username, setUsername] = useState(entry?.address?.replace(/@getalby\.com$/i, "") ?? "");
  const user = username.trim().toLowerCase().replace(/^₿/, "").replace(/@getalby\.com$/, "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const accountName = targets.find((t) => t.id === target)?.name ?? "Main";
  const getString = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await publicName.albyConnection(target, connection ?? undefined);
      setUri(r.uri);
      setConnection(r.connection_id);
      void mutate(PUBLIC_NAME_KEY);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        if (connection === null) return;
        setBusy(true);
        setError(null);
        try {
          onDone(await publicName.albyLink(connection, `${user}@getalby.com`));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      {!entry && (
        <p className="text-xs text-muted">One Alby account gives one address; for another address, use another Alby account.</p>
      )}
      <ol className="space-y-3">
        <li className="space-y-2">
          <p>
            <span className="font-medium">1. Get a connection string.</span>{" "}
            <span className="text-muted">
              It lets Alby make invoices for this node, never pay. Payments go to {accountName}. It is shown once; getting
              it again replaces the old one.
            </span>
          </p>
          {uri ? (
            <div className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate rounded-lg bg-surface-2 p-2 font-mono text-xs">{uri}</span>
              <CopyButton value={uri} />
            </div>
          ) : (
            <Button type="button" variant="secondary" disabled={busy} onClick={() => void getString()}>
              {connection !== null ? "Get a new connection string" : "Get the connection string"}
            </Button>
          )}
        </li>
        <li>
          <span className="font-medium">2. Set it up on getalby.com.</span>{" "}
          <span className="text-muted">
            In an Alby account at{" "}
            <a className="underline" href="https://getalby.com" target="_blank" rel="noreferrer">
              getalby.com
            </a>
            , connect a wallet with the connection string (Nostr Wallet Connect), and choose your Lightning address.
          </span>
        </li>
        <li className="space-y-2">
          <span className="font-medium">3. Enter the username you chose.</span>{" "}
          <span className="text-muted">Allod asks it for a test invoice to check it really pays {accountName}.</span>
          <div className="flex items-center gap-2">
            <Input
              value={username}
              onChange={(e) => setUsername(e.target.value)}
              placeholder="you"
              required
              autoComplete="off"
              name="lightning-username"
              data-1p-ignore
              data-lpignore="true"
            />
            <span className="shrink-0 font-mono text-muted">@getalby.com</span>
          </div>
        </li>
      </ol>
      {error && <Alert tone="danger" title="Alby">{error}</Alert>}
      <FormFooter back={back} busy={busy} label="Check and add" disabled={connection === null || !/^[a-z0-9._-]+$/.test(user)} />
      {connection === null && <p className="text-xs text-muted">Get the connection string first.</p>}
      {connection !== null && !uri && (
        <p className="text-xs text-muted">
          Already connected on getalby.com? Just enter the username.{" "}
          <button type="button" className="underline" onClick={() => void mutate(PUBLIC_NAME_KEY).then((v) => v && onChange(v as PublicName))}>
            Refresh
          </button>
        </p>
      )}
    </form>
  );
}
