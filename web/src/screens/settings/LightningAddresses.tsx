import { useState, type ReactNode } from "react";
import useSWR, { useSWRConfig } from "swr";
import { Link } from "react-router";
import { ArrowsClockwise, CaretRight, Plus, Trash, WarningCircle } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { CopyButton, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  domains as domainsApi,
  fetcher,
  publicName,
  PUBLIC_NAME_KEY,
  type DomainAddress,
  type DomainEntry,
  type Domains,
  type PublicName,
  type ZeusEntry,
} from "../../lib/api";
import { useAccounts } from "../../lib/account";
import { cn } from "../../lib/utils";
import { ladder, LadderPicker, SignalLegend, Signals, type LadderAction, type LadderStep } from "./AddressLadder";
import { AddressQuestions } from "./AddressQuestions";
import { RecordRows, useDomains } from "./Domains";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

type Provider = "own" | "twelve" | "clank" | "zeus" | "alby";
type Kind = "lnurl" | "bip353";
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
  if (d.alby) {
    const a = d.alby;
    out.push({
      key: "alby",
      address: a.address ?? "Alby address (setup not finished)",
      target: "main",
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
      target: "main",
      provider: "zeus",
      kinds: ["lnurl"],
      status: d.zeus.verified ? { tone: "ok", label: "Live" } : { tone: "warn", label: "Waiting for ZEUS Pay" },
      action: d.zeus.verified ? undefined : "ZEUS Pay has not answered for it yet",
      zeus: d.zeus,
    });
  }
  for (const dom of doms?.domains ?? []) {
    for (const a of dom.addresses) {
      const waiting = dom.served && !a.lnurl;
      out.push({
        key: `own-${a.id}`,
        address: a.address,
        target: a.target,
        provider: "own",
        kinds: dom.served ? ["bip353", a.lnurl ? "lnurl" : "lnurl_soon"] : ["bip353"],
        status: a.verified ? { tone: "ok", label: "Live" } : { tone: "warn", label: "Action needed" },
        action: a.verified
          ? undefined
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
      locked: !!e.purchase,
      status: e.purchase
        ? { tone: "warn", label: e.purchase.state === "paying" ? "Paying…" : "Finishing registration" }
        : !e.active
        ? { tone: "danger", label: "Offer disabled" }
        : e.verified
          ? { tone: "ok", label: "Live" }
          : { tone: "warn", label: "Waiting for DNS" },
      action: e.purchase
        ? "The purchase is not finished yet"
        : !e.active
          ? "Its wallet's offer is disabled"
          : e.verified
            ? undefined
            : "Waiting for twelve.cash to publish it",
      note: e.purchase?.error
        ? `Paid ${e.purchase.price_sats.toLocaleString()} sats; twelve.cash has not published it yet (${e.purchase.error}). Allod keeps trying.`
        : e.active
          ? undefined
          : "The wallet's offer was disabled, so payments fail. twelve.cash cannot delete the name; stop sharing it.",
    });
  }
  for (const e of d.clank) {
    const busy = e.state !== "registered";
    out.push({
      key: `clank-${e.target}`,
      address: e.address,
      target: e.target,
      provider: "clank",
      kinds: ["bip353"],
      status: busy
        ? { tone: "warn", label: e.state === "paying" ? "Paying…" : "Finishing registration" }
        : !e.active
          ? { tone: "danger", label: "Offer disabled" }
          : e.verified
            ? { tone: "ok", label: "Live" }
            : { tone: "warn", label: "Waiting for DNS" },
      note:
        busy && e.error
          ? `Paid ${e.price_sats.toLocaleString()} sats; registration not confirmed yet (${e.error}). Allod keeps trying.`
          : undefined,
      locked: busy,
      action: busy
        ? "The purchase is not finished yet"
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
/** Where the add dialog opens for a ladder choice. */
function stepFor(to: LadderAction): Step {
  switch (to) {
    case "own":
      return { at: "own", kind: "bip353" };
    case "own-lnurl":
      return { at: "own", kind: "lnurl" };
    case "twelve":
      return { at: "twelve", how: "free" };
    case "twelve-custom":
      return { at: "twelve", how: "bought" };
    default:
      return { at: to };
  }
}

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
  const [dialog, setDialog] = useState<Step | null>(null);
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
        <Button size="sm" onClick={() => setDialog({ at: "kind" })} aria-label="Add a Lightning address">
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
            <Button onClick={() => setDialog({ at: "kind" })}>
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
      {dialog && (
        <AddDialog
          key={JSON.stringify(dialog)}
          open
          initial={dialog}
          data={data}
          doms={doms}
          onClose={() => setDialog(null)}
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
  if (item.provider === "own") return "domain";
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
              : item.provider === "alby"
                ? await publicName.albyRemove()
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
          {step && <Signals s={step} />}
          <span>{wallet}</span>
          <span aria-hidden>·</span>
          <span>{PROVIDER_LABEL[item.provider]}</span>
          <Badge tone={item.status.tone}>{item.status.label}</Badge>
          <CopyButton value={item.address} />
        </div>
        {step && (
          <ul className="space-y-1 text-xs">
            {[step.sees, step.redirect, step.ip, step.wallets].map((sig) => (
              <li key={sig.text} className={sig.level === "ok" ? "text-ok" : sig.level === "warn" ? "text-warn" : "text-danger"}>
                • {sig.text}
              </li>
            ))}
          </ul>
        )}

        {item.own && (
          <section className="space-y-2">
            <h3 className="font-medium">DNS records of {item.own.d.name}</h3>
            <p className="text-muted">
              {item.own.d.dns_provider_id
                ? "Allod keeps these records at the DNS provider; they show up within minutes."
                : "Add these at your DNS host (the domain must use DNSSEC). Allod checks every few minutes, or use Check now."}
            </p>
            <RecordRows
              records={item.own.d.records.filter((r) => r.type === "A" || r.name === item.own?.a.record_name)}
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
        {item.note && <p className="text-xs text-muted">{item.note}</p>}
        {(item.action || item.provider === "own") && (
          <Button variant="secondary" size="sm" disabled={checking} onClick={() => void checkNow()}>
            <ArrowsClockwise size={16} /> {checking ? "Checking…" : "Check now"}
          </Button>
        )}

        {item.zeus && <ZeusPlusPanel zeus={item.zeus} onChange={onChange} />}

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
        {!item.locked && (
          <div className="flex justify-end border-t border-line pt-3">
            <Button variant="ghost" size="sm" onClick={() => setRemoving(true)}>
              <Trash size={16} /> Remove this address
            </Button>
          </div>
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

// ------------------------------------------------------------------ add dialog

type Step =
  | { at: "kind" }
  | { at: "ladder" }
  | { at: "picked" }
  | { at: "hosted" }
  | { at: "alby" }
  | { at: "own"; kind: Kind }
  | { at: "twelve"; how: "free" | "bought" }
  | { at: "clank" }
  | { at: "zeus" };

function AddDialog({
  open,
  initial,
  data,
  doms,
  onClose,
  onChange,
  onDomains,
}: {
  open: boolean;
  initial: Step;
  data: PublicName;
  doms: Domains | undefined;
  onClose: () => void;
  onChange: (v: PublicName) => void;
  onDomains: (v: Domains) => void;
}) {
  const [step, setStep] = useState<Step>(initial);
  const close = () => onClose();
  const done = (v: PublicName) => {
    onChange(v);
    close();
  };
  const back = (to: Step) => (
    <Button variant="ghost" onClick={() => setStep(to)}>
      Back
    </Button>
  );
  const title =
    step.at === "kind"
      ? "Add a Lightning address"
      : step.at === "ladder"
        ? "All options, most private first"
        : step.at === "hosted"
          ? "A name on a Lightning service"
          : step.at === "alby"
            ? "Alby Lightning address"
            : step.at === "zeus"
        ? "ZEUS Pay address"
        : step.at === "own"
          ? "Address on your domain"
          : step.at === "twelve"
            ? step.how === "free"
              ? "Free twelve.cash address"
              : "Your name on twelve.cash"
            : step.at === "picked"
              ? "A name you pick"
              : "Your name on clank.money";
  const toLadder = back({ at: "kind" });
  return (
    <Modal open={open} onClose={close} title={title} wide={step.at === "kind" || step.at === "ladder"}>
      {step.at === "kind" && (
        <AddressQuestions
          data={data}
          domainNames={doms?.domains.map((d) => d.name) ?? []}
          onChange={onChange}
          onPick={(to) => setStep(stepFor(to))}
          onAllOptions={() => setStep({ at: "ladder" })}
        />
      )}
      {step.at === "ladder" && (
        <div className="space-y-3">
          <LadderPicker data={data} onPick={(to) => setStep(stepFor(to))} />
          {toLadder}
        </div>
      )}
      {step.at === "hosted" && <PickService data={data} onPick={setStep} back={toLadder} />}
      {step.at === "alby" && <AlbyForm data={data} onChange={onChange} onDone={done} back={back({ at: "hosted" })} />}
      {step.at === "zeus" && <ZeusForm onDone={done} back={back({ at: "hosted" })} />}
      {step.at === "own" && (
        <OwnForm
          doms={doms}
          kind={step.kind}
          onDone={(v) => {
            onDomains(v);
            close();
          }}
          back={toLadder}
        />
      )}
      {step.at === "picked" && <PickRegistry data={data} onPick={setStep} back={toLadder} />}
      {step.at === "twelve" && (
        <TwelveForm data={data} how={step.how} onDone={done} back={step.how === "bought" ? back({ at: "picked" }) : toLadder} />
      )}
      {step.at === "clank" && <ClankForm data={data} onDone={done} back={back({ at: "picked" })} />}
    </Modal>
  );
}

/** Main and the accounts that do not have an address of this provider yet. */
function TargetSelect({ value, onChange, taken }: { value: string; onChange: (v: string) => void; taken: Set<string> }) {
  const targets = useTargets().filter((t) => !taken.has(t.id));
  return (
    <Field label="Pays into">
      <select
        className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
        value={value}
        onChange={(e) => onChange(e.target.value)}
      >
        {targets.map((t) => (
          <option key={t.id} value={t.id}>
            {t.name}
          </option>
        ))}
      </select>
    </Field>
  );
}

/** The first free target, or "" when every wallet already has one. */
function firstFree(targets: { id: string }[], taken: Set<string>): string {
  return targets.find((t) => !taken.has(t.id))?.id ?? "";
}

function FormFooter({ back, busy, label, disabled }: { back: ReactNode; busy: boolean; label: string; disabled?: boolean }) {
  return (
    <div className="flex justify-between gap-2">
      {back}
      <Button type="submit" disabled={busy || disabled}>
        {busy ? "Working…" : label}
      </Button>
    </div>
  );
}

function OwnForm({
  doms,
  kind,
  onDone,
  back,
}: {
  doms: Domains | undefined;
  kind: Kind;
  onDone: (v: Domains) => void;
  back: ReactNode;
}) {
  const list = doms?.domains ?? [];
  // For LNURL, a served domain first.
  const preferred = (kind === "lnurl" ? list.find((d) => d.served) : undefined) ?? list[0];
  const [domainId, setDomainId] = useState<number | "new">(preferred?.id ?? "new");
  const [newName, setNewName] = useState("");
  const [target, setTarget] = useState("main");
  const [username, setUsername] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const domain = list.find((d) => d.id === domainId);
  const shown = domain?.name ?? (newName.trim().toLowerCase() || "your-domain");
  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      let id = domainId;
      if (id === "new") {
        const v = await domainsApi.add({ name: newName.trim() });
        const added = v.domains.find((d) => d.name === newName.trim().toLowerCase()) ?? v.domains[v.domains.length - 1];
        if (!added) throw new Error("the domain was not added");
        id = added.id;
        setDomainId(id);
      }
      onDone(await domainsApi.addAddress(id, username, target));
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={(e) => {
        e.preventDefault();
        void submit();
      }}
    >
      <Field label="Domain">
        <select
          className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
          value={domainId}
          onChange={(e) => setDomainId(e.target.value === "new" ? "new" : Number(e.target.value))}
        >
          {list.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name} {d.served ? "(every wallet)" : "(BIP-353 only)"}
            </option>
          ))}
          <option value="new">Add a domain…</option>
        </select>
      </Field>
      {domainId === "new" && (
        <Field label="New domain" hint="A domain you control; for BIP-353 its DNS must use DNSSEC. Set its DNS provider and gateway later in Domains.">
          <Input value={newName} onChange={(e) => setNewName(e.target.value)} placeholder="pay.example.com" required />
        </Field>
      )}
      <TargetSelect value={target} onChange={setTarget} taken={new Set()} />
      <Field label="Username" hint="Lowercase letters, digits, '.', '_' or '-'.">
        <div className="flex items-center gap-2">
          <Input
            value={username}
            onChange={(e) => setUsername(e.target.value.toLowerCase())}
            placeholder="alice"
            required
            autoComplete="off"
            name="lightning-username"
            data-1p-ignore
            data-lpignore="true"
          />
          <span className="shrink-0 font-mono text-muted">@{shown}</span>
        </div>
      </Field>
      {kind === "lnurl" && !domain?.served && (
        <Alert tone="warn" title="Every wallet needs HTTPS">
          LNURL works once the domain is served over HTTPS: through a gateway (your home IP stays hidden) or from this
          machine. Choose that for the domain in{" "}
          <Link className="underline" to="/settings/domains">
            Domains
          </Link>
          ; until then the address answers BIP-353 wallets.
        </Alert>
      )}
      <p className="text-muted">
        {domain?.dns_provider_id
          ? "Allod adds its records at the DNS provider."
          : "After saving, Allod shows the TXT record to add at your DNS host."}
      </p>
      {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
      <FormFooter back={back} busy={busy} label="Add address" disabled={!username.trim() || (domainId === "new" && !newName.trim())} />
    </form>
  );
}

/** Step 2: the two registries side by side, each with its price and what it means. */
function PickRegistry({ data, onPick, back }: { data: PublicName; onPick: (s: Step) => void; back: ReactNode }) {
  const card = (
    name: string,
    price: string,
    points: string[],
    available: boolean | undefined,
    to: Step,
  ) => (
    <div className="flex flex-col gap-3 rounded-lg border border-line p-4">
      <div>
        <p className="font-semibold">{name}</p>
        <p className="whitespace-nowrap text-lg font-semibold">
          {price} <span className="text-sm font-normal text-muted">once</span>
        </p>
      </div>
      <ul className="flex-1 space-y-1 text-muted">
        {points.map((p) => (
          <li key={p}>• {p}</li>
        ))}
      </ul>
      <Button disabled={!available} onClick={() => onPick(to)}>
        {available ? `Choose ${name}` : "Mainnet only"}
      </Button>
    </div>
  );
  return (
    <div className="space-y-4 text-sm">
      <p className="text-muted">
        Both publish your name as a BIP-353 record pointing at this wallet's own offer: they never see your payments.
      </p>
      <div className="grid gap-3 sm:grid-cols-2">
        {card(
          "clank.money",
          `~${CLANK_SATS.toLocaleString()} sats`,
          ["mynode@clank.money", "Allod pays for it from your wallet", "You can point it elsewhere later"],
          data.clank_available,
          { at: "clank" },
        )}
        {card(
          "twelve.cash",
          `${TWELVE_CUSTOM_SATS.toLocaleString()} sats`,
          ["satoshi@twelve.cash", "Allod pays for it from your wallet", "Cannot be changed or deleted"],
          data.twelve_cash_available,
          { at: "twelve", how: "bought" },
        )}
      </div>
      <div className="flex justify-start">{back}</div>
    </div>
  );
}

function TwelveForm({
  data,
  how,
  onDone,
  back,
}: {
  data: PublicName;
  how: "free" | "bought";
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const targets = useTargets();
  const taken = new Set(data.twelve_cash.map((e) => e.target));
  const [target, setTarget] = useState(() => firstFree(targets, taken));
  // A name bought on the website earlier is linked instead of bought here.
  const [linking, setLinking] = useState(false);
  if (how === "bought" && !linking && target !== "") {
    return (
      <TwelveBuy data={data} target={target} setTarget={setTarget} taken={taken} onDone={onDone} back={back} onLink={() => setLinking(true)} />
    );
  }
  return <TwelveLinkOrFree data={data} how={how} target={target} setTarget={setTarget} taken={taken} onDone={onDone} back={back} />;
}

function TwelveLinkOrFree({
  how,
  target,
  setTarget,
  taken,
  onDone,
  back,
}: {
  data: PublicName;
  how: "free" | "bought";
  target: string;
  setTarget: (t: string) => void;
  taken: Set<string>;
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
  if (target === "") return <p className="text-sm text-muted">Main and every account already have a twelve.cash address.</p>;
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
      <TargetSelect
        value={target}
        onChange={(t) => {
          setTarget(t);
          setChosen(null);
        }}
        taken={taken}
      />
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

function ClankForm({ data, onDone, back }: { data: PublicName; onDone: (v: PublicName) => void; back: ReactNode }) {
  const targets = useTargets();
  const taken = new Set(data.clank.map((e) => e.target));
  const [target, setTarget] = useState(() => firstFree(targets, taken));
  const [username, setUsername] = useState("");
  const [maxPrice, setMaxPrice] = useState("1000");
  const [fromMain, setFromMain] = useState(false);
  const [check, setCheck] = useState<{ valid: boolean; available: boolean | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  if (target === "") return <p className="text-sm text-muted">Main and every account already have a clank.money name.</p>;
  const user = username.trim().toLowerCase();
  const max = Number.parseInt(maxPrice.replace(/[^0-9]/g, ""), 10) || 0;
  const wallet = targets.find((t) => t.id === target)?.name ?? "";
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
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
        } finally {
          setBusy(false);
        }
      }}
    >
      <TargetSelect value={target} onChange={setTarget} taken={taken} />
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

function ZeusForm({ onDone, back }: { onDone: (v: PublicName) => void; back: ReactNode }) {
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
      return await publicName.zeusAdd();
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
        this node by signing ZEUS Pay's challenge with the node key. Payments go to Main. ZEUS Pay is a third party: it
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
  setTarget,
  taken,
  onDone,
  back,
  onLink,
}: {
  data: PublicName;
  target: string;
  setTarget: (t: string) => void;
  taken: Set<string>;
  onDone: (v: PublicName) => void;
  back: ReactNode;
  onLink: () => void;
}) {
  const targets = useTargets();
  const [username, setUsername] = useState("");
  const [fromMain, setFromMain] = useState(false);
  const [confirm, setConfirm] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const user = username.trim().replace(/^₿/, "").replace(/@twelve\.cash$/i, "");
  const valid = /^[a-zA-Z0-9._-]{4,64}$/.test(user);
  const wallet = targets.find((t) => t.id === target)?.name ?? "";
  const buy = async () => {
    setBusy(true);
    setError(null);
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
      <TargetSelect value={target} onChange={setTarget} taken={taken} />
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

/** Step 0: the two Lightning services side by side. */
function PickService({ data, onPick, back }: { data: PublicName; onPick: (s: Step) => void; back: ReactNode }) {
  const card = (name: string, sub: string, points: string[], usable: boolean, reason: string, to: Step) => (
    <div className="flex flex-col gap-3 rounded-lg border border-line p-4">
      <div>
        <p className="font-semibold">{name}</p>
        <p className="text-lg font-semibold">
          Free <span className="text-sm font-normal text-muted">{sub}</span>
        </p>
      </div>
      <ul className="flex-1 space-y-1 text-muted">
        {points.map((p) => (
          <li key={p}>• {p}</li>
        ))}
      </ul>
      <Button disabled={!usable} onClick={() => onPick(to)}>
        {usable ? `Choose ${name}` : reason}
      </Button>
    </div>
  );
  return (
    <div className="space-y-4 text-sm">
      <p className="text-muted">
        Both answer every wallet and ask this node for each invoice over Nostr Wallet Connect, so your IP stays private.
        The service sees every payment and could hand payers its own invoice.
      </p>
      <div className="grid gap-3 sm:grid-cols-2">
        {card(
          "ZEUS Pay",
          "· automatic",
          ["A random name@zeuspay.com", "Set up in one step, nothing to do elsewhere", "Choosing the name costs ZEUS Pay+ (yearly)"],
          !!data.zeus_available && !data.zeus,
          data.zeus ? "Main already has one" : "Mainnet only",
          { at: "zeus" },
        )}
        {card(
          "Alby",
          "· your own name",
          ["you@getalby.com, any free name", "Needs an Alby account (email and captcha)", "A few steps on getalby.com"],
          !!data.alby_available && !data.alby?.verified,
          data.alby?.verified ? "Main already has one" : "Mainnet only",
          { at: "alby" },
        )}
      </div>
      <div className="flex justify-start">{back}</div>
    </div>
  );
}

/** Alby: Allod gives a receive-only connection string; the owner sets the address up on
 * getalby.com, then enters it, and Allod checks it asks this node for invoices. */
function AlbyForm({
  data,
  onChange,
  onDone,
  back,
}: {
  data: PublicName;
  onChange: (v: PublicName) => void;
  onDone: (v: PublicName) => void;
  back: ReactNode;
}) {
  const { mutate } = useSWRConfig();
  const [uri, setUri] = useState<string | null>(null);
  const [address, setAddress] = useState(data.alby?.address ?? "");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const getString = async () => {
    setBusy(true);
    setError(null);
    try {
      setUri((await publicName.albyConnection()).uri);
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
        setBusy(true);
        setError(null);
        try {
          onDone(await publicName.albyLink(address));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <ol className="space-y-3">
        <li className="space-y-2">
          <p>
            <span className="font-medium">1. Get a connection string.</span>{" "}
            <span className="text-muted">
              It lets Alby make invoices for this node, never pay. Payments go to Main. It is shown once; getting it again
              replaces the old one.
            </span>
          </p>
          {uri ? (
            <div className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate rounded-lg bg-surface-2 p-2 font-mono text-xs">{uri}</span>
              <CopyButton value={uri} />
            </div>
          ) : (
            <Button type="button" variant="secondary" disabled={busy} onClick={() => void getString()}>
              {data.alby ? "Get a new connection string" : "Get the connection string"}
            </Button>
          )}
        </li>
        <li>
          <span className="font-medium">2. Set it up on getalby.com.</span>{" "}
          <span className="text-muted">
            Create an account at{" "}
            <a className="underline" href="https://getalby.com" target="_blank" rel="noreferrer">
              getalby.com
            </a>
            , connect a wallet with the connection string (Nostr Wallet Connect), and choose your Lightning address.
          </span>
        </li>
        <li className="space-y-2">
          <span className="font-medium">3. Enter the address.</span>{" "}
          <span className="text-muted">Allod asks it for a test invoice to check it really pays this node.</span>
          <Input
            value={address}
            onChange={(e) => setAddress(e.target.value)}
            placeholder="you@getalby.com"
            required
            autoComplete="off"
            name="lightning-address"
            data-1p-ignore
            data-lpignore="true"
          />
        </li>
      </ol>
      {error && <Alert tone="danger" title="Alby">{error}</Alert>}
      <FormFooter back={back} busy={busy} label="Check and add" disabled={!data.alby && !uri} />
      {!data.alby && !uri && <p className="text-xs text-muted">Get the connection string first.</p>}
      {data.alby && !uri && (
        <p className="text-xs text-muted">
          Already connected on getalby.com? Just enter the address.{" "}
          <button type="button" className="underline" onClick={() => void mutate(PUBLIC_NAME_KEY).then((v) => v && onChange(v as PublicName))}>
            Refresh
          </button>
        </p>
      )}
    </form>
  );
}
