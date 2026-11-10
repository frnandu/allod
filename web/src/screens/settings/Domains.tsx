import { useState, type ReactNode } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { ArrowsClockwise, CaretRight, Globe, LockSimple, Plus, ShieldCheck, Trash } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { CopyButton, Field, Modal, QrBlock, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  api as http,
  domains as api,
  DOMAINS_KEY,
  fetcher,
  type DnsProviderEntry,
  type DnsProviderKind,
  type NjallaName,
  type NjallaPurchase,
  type DomainEntry,
  type DomainRecord,
  type Domains,
  type GatewayEntry,
  type ReachState,
} from "../../lib/api";
import { cn } from "../../lib/utils";

/**
 * Settings → Domains (SPEC §12.1): the owner's domains, how each is served, their
 * addresses and DNS records, and the DNS providers Allod keeps records at.
 */

export function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Something the page should watch closely: records not served yet, a certificate on its way. */
export function domainsWaiting(d: Domains): boolean {
  return (
    d.domains.some((x) => x.addresses.some((a) => !a.verified) || x.reach?.state === "waiting_for_certificate") ||
    d.gateways.some((g) => g.state === "ordering" || g.state === "setup") ||
    d.desec_signup?.state === "activating" ||
    d.njalla_purchase?.state === "topup" ||
    d.njalla_purchase?.state === "registering" ||
    d.njalla_signup?.state === "activating"
  );
}

/** The domain list, polled faster while something is on its way. */
export function useDomains() {
  return useSWR<Domains>(DOMAINS_KEY, fetcher, {
    refreshInterval: (d) => (d && domainsWaiting(d) ? 10_000 : 60_000),
  });
}

const REACH: Record<ReachState, { tone: "ok" | "warn" | "danger"; label: string }> = {
  reachable: { tone: "ok", label: "Reachable over HTTPS" },
  waiting_for_certificate: { tone: "warn", label: "Waiting for the certificate" },
  not_reachable: { tone: "danger", label: "Not reachable" },
};

/** How a domain is served, in words. */
export function servedBy(d: DomainEntry, gateways: GatewayEntry[]): string {
  if (d.gateway_id) {
    const g = gateways.find((x) => x.id === d.gateway_id);
    return `Through gateway ${g?.label ?? d.gateway_id}`;
  }
  return d.direct ? "From this machine (public IP)" : "BIP-353 only";
}

export const PROVIDER_KIND: Record<DnsProviderKind, string> = { njalla: "Njalla", desec: "deSEC" };

function providerName(d: DomainEntry, providers: DnsProviderEntry[]): string {
  const p = providers.find((x) => x.id === d.dns_provider_id);
  return p ? `${p.label} (${PROVIDER_KIND[p.kind]})` : "Manual";
}

/** A domain's records: managed at its provider, or to add by hand. */
export function RecordRows({ records, managed }: { records: DomainRecord[]; managed: boolean }) {
  if (records.length === 0) return <p className="text-xs text-muted">No records yet: add an address first.</p>;
  return (
    <div className="space-y-2">
      {records.map((r) => (
        <div key={`${r.type} ${r.name}`} className="space-y-1 rounded-lg bg-surface-2 p-2 text-xs">
          <p className="flex items-center gap-2">
            <Badge>{r.type}</Badge>
            <span className="min-w-0 flex-1 truncate font-mono">{r.name}</span>
            <CopyButton value={r.name} />
            {r.ok ? <Badge tone="ok">in place</Badge> : <Badge tone="warn">{managed ? "publishing" : "to add"}</Badge>}
          </p>
          {r.value ? (
            <p className="flex items-center gap-2">
              <span className="min-w-0 flex-1 truncate font-mono text-muted">{r.value}</span>
              <CopyButton value={r.value} />
            </p>
          ) : (
            <p className="text-muted">The value appears once the wallet is running.</p>
          )}
        </div>
      ))}
    </div>
  );
}

export function DomainSettings() {
  const { data, error, mutate } = useDomains();
  const [adding, setAdding] = useState(false);
  const [buying, setBuying] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  if (error) return <Alert tone="danger" title="Could not load domains">{message(error)}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  const update = (v: Domains) => void mutate(v, { revalidate: false });
  const current = data.domains.find((d) => d.id === open);
  return (
    <div className="space-y-6">
      <section className="space-y-3" aria-label="Domains">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="text-lg font-semibold">Domains</h2>
            <p className="text-sm text-muted">
              Your own domain names, for Lightning addresses like alice@your-domain. Allod keeps their records with a DNS
              provider, or lists them for you to add.
            </p>
          </div>
          <span className="flex shrink-0 gap-2">
            <Button size="sm" variant="secondary" onClick={() => setBuying(true)} aria-label="Buy a domain">
              <Globe size={16} /> Buy
            </Button>
            <Button size="sm" onClick={() => setAdding(true)} aria-label="Add a domain">
              <Plus size={16} weight="bold" /> Add
            </Button>
          </span>
        </div>
        {data.njalla_purchase && <PurchaseCard p={data.njalla_purchase} onChange={update} />}
        {data.domains.length === 0 ? (
          <Card>
            <CardContent className="space-y-3 pt-6 text-sm">
              <p className="text-base font-semibold">No domain yet</p>
              <p className="text-muted">
                With a domain you control, every account can have addresses like alice@your-domain, and nobody in between
                sees your payments. For BIP-353 addresses, the domain's DNS must be signed with DNSSEC. To answer every
                wallet (LNURL) without showing your home IP, serve it through a{" "}
                <Link className="underline" to="/settings/gateways">
                  gateway
                </Link>
                .
              </p>
              <div className="flex flex-wrap gap-2">
                <Button onClick={() => setAdding(true)}>
                  <Plus size={16} weight="bold" /> Add my domain
                </Button>
                <Button variant="secondary" onClick={() => setBuying(true)}>
                  <Globe size={16} /> Buy a new domain
                </Button>
              </div>
            </CardContent>
          </Card>
        ) : (
          <Card>
            <CardContent className="space-y-2 pt-6">
              {data.domains.map((d) => (
                <button
                  key={d.id}
                  type="button"
                  onClick={() => setOpen(d.id)}
                  className={cn(
                    "flex w-full flex-wrap items-center justify-between gap-2 rounded-lg border p-3 text-left text-sm transition hover:border-accent",
                    d.live ? "border-line" : "border-warn/60",
                  )}
                >
                  <span className="min-w-0 space-y-1">
                    <span className="flex flex-wrap items-center gap-2">
                      {d.served ? <Globe size={16} weight="duotone" /> : <LockSimple size={16} weight="duotone" />}
                      <span className="font-mono font-medium">{d.name}</span>
                      <Badge tone={d.live ? "ok" : "warn"}>{d.live ? "Live" : "Not live yet"}</Badge>
                      {d.env && <Badge>ALLOD_PUBLIC_URL</Badge>}
                    </span>
                    <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
                      <span>{servedBy(d, data.gateways)}</span>
                      <span aria-hidden>·</span>
                      <span>DNS: {providerName(d, data.dns_providers)}</span>
                      <span aria-hidden>·</span>
                      <span>
                        {d.addresses.length} address{d.addresses.length === 1 ? "" : "es"}
                      </span>
                    </span>
                    {!d.live && d.checks && (
                      <span className="block text-xs text-warn">
                        {!d.checks.dns
                          ? "DNS does not answer for it"
                          : !d.checks.dnssec
                            ? "DNSSEC is off: turn it on at the registrar"
                            : d.checks.provider === false
                              ? "The DNS provider cannot reach it"
                              : "Not reachable over HTTPS yet"}
                      </span>
                    )}
                    {d.error && <span className="block text-xs text-danger">{d.error}</span>}
                  </span>
                  <CaretRight size={16} className="text-muted" />
                </button>
              ))}
            </CardContent>
          </Card>
        )}
      </section>
      <DnsProviders data={data} onChange={update} />
      {adding && <AddDomain data={data} onClose={() => setAdding(false)} onDone={(v) => (update(v), setAdding(false))} />}
      {buying && (
        <BuyDomain
          data={data}
          onChange={update}
          onClose={() => setBuying(false)}
          onDone={(v) => (update(v), setBuying(false))}
        />
      )}
      {current && <DomainDetail domain={current} data={data} onChange={update} onClose={() => setOpen(null)} />}
    </div>
  );
}

// ------------------------------------------------------------------ serving choice

type Serving = "none" | "direct" | `gateway:${number}`;

function servingOf(d: { gateway_id: number | null; direct: boolean }): Serving {
  return d.gateway_id ? `gateway:${d.gateway_id}` : d.direct ? "direct" : "none";
}

function servingBody(s: Serving): { gateway_id: number | null; direct: boolean } {
  return s.startsWith("gateway:")
    ? { gateway_id: Number(s.slice(8)), direct: false }
    : { gateway_id: null, direct: s === "direct" };
}

function ServingSelect({ value, onChange, gateways }: { value: Serving; onChange: (v: Serving) => void; gateways: GatewayEntry[] }) {
  return (
    <Field
      label="Serve it over HTTPS (for every wallet)"
      hint="BIP-353 needs no server at all. LNURL needs the domain to reach this node over HTTPS."
    >
      <select
        className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
        value={value}
        onChange={(e) => onChange(e.target.value as Serving)}
      >
        <option value="none">No: BIP-353 only (nothing connects to this machine)</option>
        {gateways.map((g) => (
          <option key={g.id} value={`gateway:${g.id}`}>
            Through gateway {g.label} ({g.ipv4 ?? "being set up"}): home IP stays hidden
          </option>
        ))}
        <option value="direct">From this machine's public IP (your IP becomes public)</option>
      </select>
    </Field>
  );
}

function ProviderSelect({
  value,
  onChange,
  providers,
}: {
  value: number | null;
  onChange: (v: number | null) => void;
  providers: DnsProviderEntry[];
}) {
  return (
    <Field label="DNS records" hint="Manual: Allod lists the records and you add them at your DNS host.">
      <select
        className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
        value={value ?? ""}
        onChange={(e) => onChange(e.target.value ? Number(e.target.value) : null)}
      >
        <option value="">Manual (any DNS host, e.g. MyNymBox)</option>
        {providers.map((p) => (
          <option key={p.id} value={p.id}>
            Kept by Allod at {p.label} ({PROVIDER_KIND[p.kind]})
          </option>
        ))}
      </select>
    </Field>
  );
}

/** Before serving from this machine: the home IP becomes public. */
function DirectWarning() {
  return (
    <Alert tone="warn" title="Your IP becomes public">
      The domain points at this machine's public IP, so anyone who sees one of your addresses can learn where this node
      runs. Port 443 must reach this machine (port forward), and connections behind CGNAT cannot be reached. A gateway
      avoids both.
    </Alert>
  );
}

function AddDomain({ data, onClose, onDone }: { data: Domains; onClose: () => void; onDone: (v: Domains) => void }) {
  const [name, setName] = useState("");
  const [provider, setProvider] = useState<number | null>(data.dns_providers[0]?.id ?? null);
  const [serving, setServing] = useState<Serving>(data.gateways[0] ? `gateway:${data.gateways[0].id}` : "none");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title="Add a domain">
      <form
        className="space-y-4 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onDone(await api.add({ name: name.trim(), dns_provider_id: provider, ...servingBody(serving) }));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <Field label="Domain" hint="A domain or subdomain you control, like pay.example.com.">
          <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="pay.example.com" required autoComplete="off" />
        </Field>
        <ProviderSelect value={provider} onChange={setProvider} providers={data.dns_providers} />
        {data.dns_providers.length === 0 && (
          <p className="text-xs text-muted">
            Domain at Njalla, or any domain delegated to deSEC (free)? Add it under DNS providers below and Allod writes the
            records itself.
          </p>
        )}
        <ServingSelect value={serving} onChange={setServing} gateways={data.gateways} />
        {data.gateways.length === 0 && (
          <p className="text-xs text-muted">
            No gateway yet:{" "}
            <Link className="underline" to="/settings/gateways">
              add one
            </Link>{" "}
            to serve LNURL without showing your home IP.
          </p>
        )}
        {serving === "direct" && <DirectWarning />}
        {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !name.trim()}>
            {busy ? "Adding…" : "Add domain"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}

// ------------------------------------------------------------------ one domain

function DomainDetail({
  domain: d,
  data,
  onChange,
  onClose,
}: {
  domain: DomainEntry;
  data: Domains;
  onChange: (v: Domains) => void;
  onClose: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [serving, setServing] = useState<Serving>(servingOf(d));
  const [provider, setProvider] = useState<number | null>(d.dns_provider_id);
  const [removing, setRemoving] = useState(false);
  const [tab, setTab] = useState<"settings" | "records">("settings");
  const run = async (f: () => Promise<Domains>, after?: () => void) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await f());
      after?.();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const changed = serving !== servingOf(d) || provider !== d.dns_provider_id;
  const managed = d.dns_provider_id !== null;
  return (
    <Modal open onClose={onClose} title={d.name} wide>
      <div className="space-y-5 text-sm">
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted">
          <Badge tone={d.live ? "ok" : "warn"}>{d.live ? "Live" : "Not live yet"}</Badge>
          <span>{servedBy(d, data.gateways)}</span>
          {d.reach && <Badge tone={REACH[d.reach.state].tone}>{REACH[d.reach.state].label}</Badge>}
        </div>
        {d.reach?.state === "not_reachable" && (
          <Alert tone="warn" title={`https://${d.name} does not reach this node`}>
            {d.gateway_id
              ? "Check that the gateway is active (Settings → Gateways) and that the A record points at it."
              : "Forward port 443 to this machine and turn on the stack's https profile, or use a gateway."}
            {d.reach.detail && <span className="mt-1 block font-mono text-xs text-muted">{d.reach.detail}</span>}
          </Alert>
        )}
        {d.error && <Alert tone="warn" title="Last DNS sync">{d.error}</Alert>}
        <DomainChecks d={d} kind={data.dns_providers.find((p) => p.id === d.dns_provider_id)?.kind} />
        {d.ds && d.ds.length > 0 && <DsRecord ds={d.ds} signed={!!d.checks?.dnssec} />}

        <Tabs
          tabs={[
            { id: "settings" as const, label: "Settings" },
            { id: "records" as const, label: "DNS records" },
          ]}
          value={tab}
          onChange={setTab}
        />

        {tab === "records" && (
          <section className="space-y-2">
            <p className="text-xs text-muted">
              {managed
                ? "Allod keeps these records at the DNS provider and changes only the ones it created."
                : "Add these at your DNS host. The domain must have DNSSEC turned on for BIP-353."}
            </p>
            <RecordRows records={d.records} managed={managed} />
            <Button variant="secondary" size="sm" disabled={busy} onClick={() => void run(api.check)}>
              <ArrowsClockwise size={16} /> {busy ? "Checking…" : "Check now"}
            </Button>
          </section>
        )}

        {tab === "settings" && (
          <section className="space-y-3">
            <p className="text-muted">
              {d.addresses.length} address{d.addresses.length === 1 ? "" : "es"} on this domain: add or remove them in{" "}
              <Link className="underline" to="/settings/lightning-addresses">
                Lightning addresses
              </Link>
              .
            </p>
            {d.env ? (
              <p className="text-xs text-muted">
                Set by <span className="font-mono">ALLOD_PUBLIC_URL</span> on this installation: served from this machine.
              </p>
            ) : (
              <>
                <ProviderSelect value={provider} onChange={setProvider} providers={data.dns_providers} />
                <ServingSelect value={serving} onChange={setServing} gateways={data.gateways} />
                {serving === "direct" && !d.direct && <DirectWarning />}
                <div className="flex flex-wrap justify-between gap-2">
                  <Button variant="ghost" size="sm" disabled={busy} onClick={() => setRemoving(true)}>
                    <Trash size={16} /> Remove domain
                  </Button>
                  <Button
                    size="sm"
                    disabled={busy || !changed}
                    onClick={() => void run(() => api.update(d.id, { dns_provider_id: provider, ...servingBody(serving) }))}
                  >
                    Save
                  </Button>
                </div>
              </>
            )}
          </section>
        )}
        {error && <Alert tone="danger" title="Could not save">{error}</Alert>}
      </div>
      <Modal open={removing} onClose={() => setRemoving(false)} title={`Remove ${d.name}?`}>
        <div className="space-y-4 text-sm">
          <p>
            Its {d.addresses.length} address{d.addresses.length === 1 ? "" : "es"} stop working.{" "}
            {managed ? "Allod removes the records it created at the DNS provider." : "Remove its records at your DNS host."}
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRemoving(false)}>
              Cancel
            </Button>
            <Button variant="danger" onClick={() => void run(() => api.remove(d.id), onClose)}>
              Remove
            </Button>
          </div>
        </div>
      </Modal>
    </Modal>
  );
}

/** The domain's own checks, with what to do when one fails. */
function DomainChecks({ d, kind }: { d: DomainEntry; kind?: DnsProviderKind }) {
  const c = d.checks;
  if (!c) return <p className="text-xs text-muted">Checking the domain…</p>;
  const rows: { ok: boolean; label: string; fix: string }[] = [
    {
      ok: c.dns,
      label: "DNS answers for it",
      fix:
        kind === "desec"
          ? "At the registrar, set the domain's name servers to ns1.desec.io and ns2.desec.org (Allod created the domain at deSEC)."
          : "Its name servers do not answer: check the domain is registered and delegated.",
    },
    {
      ok: c.dnssec,
      label: "Signed with DNSSEC",
      fix:
        kind === "desec"
          ? "deSEC signs the zone; the registrar must publish its DS record: add the values below in the registrar's DNSSEC settings. BIP-353 wallets refuse unsigned records."
          : "Turn DNSSEC on at the registrar (Njalla: the domain's DNSSEC settings). BIP-353 wallets refuse unsigned records.",
    },
  ];
  if (c.provider !== null) {
    rows.push({
      ok: c.provider,
      label: "DNS provider reaches it",
      fix: "The API token cannot read this domain: check it is in the same account, or enter a new token.",
    });
  }
  if (d.served) {
    rows.push({
      ok: d.reach?.state === "reachable",
      label: "HTTPS reaches this node",
      fix: d.reach?.state === "waiting_for_certificate" ? "Waiting for the certificate (a few minutes)." : "See above.",
    });
  }
  return (
    <ul className="space-y-1 text-xs">
      {rows.map((r) => (
        <li key={r.label} className={r.ok ? "text-ok" : "text-warn"}>
          {r.ok ? "✓" : "✗"} {r.label}
          {!r.ok && <span className="block pl-4 text-muted">{r.fix}</span>}
        </li>
      ))}
    </ul>
  );
}

// ------------------------------------------------------------------ buying at Njalla

const NAME_STATUS: Record<string, { tone: "ok" | "warn" | "danger" | "neutral"; label: string }> = {
  available: { tone: "ok", label: "available" },
  taken: { tone: "neutral", label: "taken" },
  unavailable: { tone: "neutral", label: "unavailable" },
  own: { tone: "neutral", label: "yours" },
  "in progress": { tone: "warn", label: "checking…" },
  failed: { tone: "warn", label: "not checked" },
};

/** Search a name at Njalla, choose how it is served, buy it. */
export function BuyDomain({
  data,
  onChange,
  onClose,
  onDone,
}: {
  data: Domains;
  /** Updates the page without closing (connecting Njalla first). */
  onChange: (v: Domains) => void;
  onClose: () => void;
  onDone: (v: Domains) => void;
}) {
  const njallas = data.dns_providers.filter((p) => p.kind === "njalla");
  const [picked, setProvider] = useState<number | null>(null);
  // Follows the list: a Njalla account connected in this dialog is used at once.
  const provider = njallas.find((p) => p.id === picked)?.id ?? njallas[0]?.id ?? null;
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<NjallaName[] | null>(null);
  const [chosen, setChosen] = useState<NjallaName | null>(null);
  const [years, setYears] = useState(1);
  const [serving, setServing] = useState<Serving>(data.gateways[0] ? `gateway:${data.gateways[0].id}` : "none");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const run = async (f: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await f();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  if (provider === null) {
    return (
      <Modal open onClose={onClose} title="Buy a domain: connect Njalla">
        <NjallaConnect data={data} onChange={onChange} onCancel={onClose} />
      </Modal>
    );
  }
  const shown = (results ?? []).filter((r) => r.status !== "unavailable").slice(0, 40);
  return (
    <Modal open onClose={onClose} title="Buy a domain at Njalla" wide>
      <div className="space-y-4 text-sm">
        {njallas.length > 1 && (
          <Field label="Njalla account">
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={provider}
              onChange={(e) => setProvider(Number(e.target.value))}
            >
              {njallas.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.label}
                </option>
              ))}
            </select>
          </Field>
        )}
        <form
          className="flex items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            void run(async () => {
              setChosen(null);
              setResults((await api.njallaSearch(provider, query.trim())).domains);
            });
          }}
        >
          <div className="flex-1">
            <Field label="Name">
              <Input value={query} onChange={(e) => setQuery(e.target.value)} placeholder="mynode or mynode.com" autoFocus />
            </Field>
          </div>
          <Button type="submit" disabled={busy || !query.trim()}>
            {busy && !chosen ? "Searching…" : "Search"}
          </Button>
        </form>
        {results && shown.length === 0 && <p className="text-muted">Nothing found.</p>}
        {shown.length > 0 && !chosen && (
          <div className="max-h-72 space-y-1 overflow-y-auto">
            {shown.map((r) => {
              const st = NAME_STATUS[r.status] ?? { tone: "neutral" as const, label: r.status };
              return (
                <button
                  key={r.name}
                  type="button"
                  disabled={r.status !== "available"}
                  onClick={() => setChosen(r)}
                  className="flex w-full items-center justify-between gap-2 rounded-lg border border-line px-3 py-2 text-left transition hover:border-accent disabled:opacity-60"
                >
                  <span className="font-mono">{r.name}</span>
                  <span className="flex items-center gap-2">
                    <Badge tone={st.tone}>{st.label}</Badge>
                    <span className="w-20 text-right">€{r.price} / year</span>
                  </span>
                </button>
              );
            })}
            {shown.every((r) => r.status === "failed" || r.status === "in progress") && (
              <p className="text-xs text-warn">
                Njalla has not checked these names yet (or its check is down). Search again in a minute; Allod buys only a
                name Njalla says is available.
              </p>
            )}
          </div>
        )}
        {chosen && (
          <div className="space-y-3 rounded-lg border border-accent p-3">
            <div className="flex items-center justify-between">
              <span className="font-mono text-base">{chosen.name}</span>
              <Button variant="ghost" size="sm" onClick={() => setChosen(null)}>
                Change
              </Button>
            </div>
            <Field label="Years">
              <select
                className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
                value={years}
                onChange={(e) => setYears(Number(e.target.value))}
              >
                {[1, 2, 3, 5].map((y) => (
                  <option key={y} value={y}>
                    {y} year{y > 1 ? "s" : ""} · €{chosen.price * y}
                  </option>
                ))}
              </select>
            </Field>
            <ServingSelect value={serving} onChange={setServing} gateways={data.gateways} />
            <p className="text-xs text-muted">
              Allod tops the Njalla wallet up in bitcoin if it is short (on-chain: €5 or a multiple of €15), registers the
              name once the payment confirms, turns DNSSEC on and keeps its records. Njalla keeps the owner details private.
            </p>
          </div>
        )}
        {error && <Alert tone="danger" title="Njalla">{error}</Alert>}
        <Footer
          onCancel={onClose}
          busy={busy}
          label={chosen ? `Buy for €${chosen.price * years}` : "Buy"}
          disabled={!chosen}
          onSubmit={() =>
            chosen &&
            void run(async () =>
              onDone(
                await api.njallaBuy({ provider_id: provider, domain: chosen.name, years, ...servingBody(serving) }),
              ),
            )
          }
        />
      </div>
    </Modal>
  );
}

/** Njalla first: Allod makes the account itself, or the owner pastes a token of theirs. */
function NjallaConnect({
  data,
  onChange,
  onCancel,
}: {
  data: Domains;
  onChange: (v: Domains) => void;
  onCancel: () => void;
}) {
  const s = data.njalla_signup;
  const reconnect = s?.state === "removed" ? s.email : null;
  const [own, setOwn] = useState(false);
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  if (s?.state === "activating") {
    return (
      <div className="space-y-3 text-sm">
        <p>
          Allod is creating the Njalla account (<span className="font-mono">{s.email}</span>) and waits for its
          confirmation mail, usually a minute or two. The search opens here by itself once it is ready.
        </p>
        {s.error && <p className="text-xs text-warn">{s.error}</p>}
        <div className="flex justify-end">
          <Button variant="ghost" onClick={onCancel}>
            Close
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div className="space-y-4 text-sm">
      <p className="text-muted">
        Allod buys domains at Njalla, a privacy-friendly registrar paid in bitcoin, and keeps their DNS records there. It
        needs a Njalla account first.
      </p>
      <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label="Njalla account">
        {(
          [
            [false, reconnect ? "Use the account Allod made" : "Create an account for me", "Nothing to type: Allod signs up with its own address and confirms it."],
            [true, "I have a Njalla account", "Paste an API token from njal.la → Settings → API."],
          ] as const
        ).map(([k, title, body]) => (
          <button
            key={String(k)}
            type="button"
            role="radio"
            aria-checked={own === k}
            onClick={() => setOwn(k)}
            className={cn(
              "rounded-lg border p-3 text-left transition",
              own === k ? "border-accent bg-accent/5" : "border-line hover:border-accent",
            )}
          >
            <span className="block font-medium">{title}</span>
            <span className="block text-xs text-muted">{body}</span>
          </button>
        ))}
      </div>
      {!own ? (
        <NjallaSignupForm onDone={onChange} onCancel={onCancel} reconnect={reconnect} />
      ) : (
        <form
          className="space-y-3"
          onSubmit={async (e) => {
            e.preventDefault();
            setBusy(true);
            setError(null);
            try {
              onChange(await api.addProvider({ kind: "njalla", token: token.trim() }));
            } catch (err) {
              setError(message(err));
            } finally {
              setBusy(false);
            }
          }}
        >
          <Field label="Njalla API token">
            <Input
              value={token}
              onChange={(e) => setToken(e.target.value)}
              required
              autoComplete="off"
              name="njalla-token"
              data-1p-ignore
              data-lpignore="true"
            />
          </Field>
          {error && <Alert tone="danger" title="Njalla">{error}</Alert>}
          <Footer onCancel={onCancel} busy={busy} label="Connect" disabled={!token.trim()} />
        </form>
      )}
    </div>
  );
}

/** The purchase in progress: the top-up to pay, then the registration. */
export function PurchaseCard({ p, onChange }: { p: NjallaPurchase; onChange: (v: Domains) => void }) {
  const [paying, setPaying] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [txid, setTxid] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  // An old finished purchase starts hidden (read once, at mount).
  const [hidden, setHidden] = useState(
    () => (p.state === "done" || p.state === "failed") && Date.now() / 1000 - p.since > 86_400,
  );
  if (hidden) return null;
  const t = p.topup;
  const payFromNode = async () => {
    if (!t) return;
    setPaying(true);
    setError(null);
    try {
      const r = await http.post<{ txid: string }>("/api/onchain/send", { address: t.address, amount_sat: t.sats });
      setTxid(r.txid);
    } catch (e) {
      setError(message(e));
    } finally {
      setPaying(false);
    }
  };
  return (
    <Card>
      <CardContent className="space-y-3 pt-6 text-sm">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="flex items-center gap-2 font-medium">
            <Globe size={16} /> Buying <span className="font-mono">{p.domain}</span> · €{p.price_eur}
          </span>
          {p.state === "topup" && <Badge tone="warn">waiting for the top-up</Badge>}
          {p.state === "registering" && <Badge tone="warn">registering</Badge>}
          {p.state === "done" && <Badge tone="ok">bought</Badge>}
          {p.state === "failed" && <Badge tone="danger">stopped</Badge>}
        </div>
        {p.state === "topup" && t && (
          <>
            <p className="text-muted">
              The Njalla wallet has €{p.balance_eur ?? 0}. Send <span className="font-medium">{t.amount_btc} BTC</span> (€
              {t.amount_eur}, {t.sats.toLocaleString()} sats) on-chain to top it up; Njalla credits it after the
              transaction confirms, then Allod registers the name.
            </p>
            <div className="grid gap-4 sm:grid-cols-2">
              <QrBlock value={t.uri} caption="Any bitcoin wallet" />
              <div className="space-y-2">
                <p className="font-medium">Or pay from this node</p>
                <p className="text-xs text-muted">
                  Sends {t.sats.toLocaleString()} sats from the node's on-chain balance, plus the network fee.
                </p>
                {txid ? (
                  <p className="text-xs text-ok">
                    Sent: <span className="break-all font-mono">{txid}</span>
                  </p>
                ) : (
                  <Button variant="secondary" disabled={paying} onClick={() => setConfirming(true)}>
                    {paying ? "Sending…" : `Pay ${t.sats.toLocaleString()} sats on-chain`}
                  </Button>
                )}
                {t.status && <p className="text-xs text-muted">Njalla: {t.status}</p>}
              </div>
            </div>
          </>
        )}
        {p.state === "topup" && !t && <p className="text-muted">The Njalla wallet covers it: registering shortly.</p>}
        {p.state === "registering" && <p className="text-muted">Njalla is registering the name; this takes a few minutes.</p>}
        {p.state === "done" && (
          <p className="text-muted">
            {p.domain} is yours and in the list below, with DNSSEC; Allod keeps its records at Njalla.
          </p>
        )}
        {p.error && <p className={p.state === "failed" ? "text-danger" : "text-xs text-warn"}>{p.error}</p>}
        {t && (
          <Modal open={confirming} onClose={() => setConfirming(false)} title="Send the top-up?">
            <div className="space-y-4 text-sm">
              <p>
                Allod sends <span className="font-medium">{t.sats.toLocaleString()} sats</span> (€{t.amount_eur}) from the
                node's on-chain balance to Njalla's address{" "}
                <span className="break-all font-mono text-xs">{t.address}</span>, plus the network fee. An on-chain
                payment cannot be undone; what is not spent on {p.domain} stays in the Njalla wallet.
              </p>
              <div className="flex justify-end gap-2">
                <Button variant="ghost" onClick={() => setConfirming(false)}>
                  Cancel
                </Button>
                <Button
                  onClick={() => {
                    setConfirming(false);
                    void payFromNode();
                  }}
                >
                  Send {t.sats.toLocaleString()} sats
                </Button>
              </div>
            </div>
          </Modal>
        )}
        {error && <Alert tone="danger" title="Payment">{error}</Alert>}
        <div className="flex justify-end gap-2">
          {p.state === "topup" && (
            <Button
              variant="ghost"
              size="sm"
              onClick={async () => {
                try {
                  onChange(await api.njallaBuyCancel());
                } catch (e) {
                  setError(message(e));
                }
              }}
            >
              Stop
            </Button>
          )}
          {(p.state === "done" || p.state === "failed") && (
            <Button
              variant="ghost"
              size="sm"
              onClick={async () => {
                setHidden(true);
                try {
                  // Allod forgets it, so it stays closed after a reload.
                  onChange(await api.njallaBuyCancel());
                } catch (e) {
                  setHidden(false);
                  setError(message(e));
                }
              }}
            >
              Close
            </Button>
          )}
        </div>
      </CardContent>
    </Card>
  );
}

/** The DS record the registrar must publish (deSEC signs the zone with these keys). */
function DsRecord({ ds, signed }: { ds: string[]; signed: boolean }) {
  const [tag, alg, type, digest] = (ds[0] ?? "").split(/\s+/);
  const rows: [string, string][] = [
    ["Key tag", tag ?? ""],
    ["Algorithm", alg === "13" ? "13 (ECDSAP256SHA256)" : (alg ?? "")],
    ["Digest type", type === "2" ? "2 (SHA-256)" : type === "4" ? "4 (SHA-384)" : (type ?? "")],
    ["Digest", digest ?? ""],
  ];
  return (
    <details className="rounded-lg border border-line p-3 text-xs" open={!signed}>
      <summary className="cursor-pointer font-medium">
        DS record for the registrar {signed ? <Badge tone="ok">published</Badge> : <Badge tone="warn">to add</Badge>}
      </summary>
      <p className="mt-2 text-muted">
        In the registrar's DNSSEC settings (where you bought the domain), add one DS record with these values. One record is
        enough; keep TTL as offered.
      </p>
      <dl className="mt-2 space-y-1">
        {rows.map(([k, v]) => (
          <div key={k} className="flex items-center gap-2">
            <dt className="w-24 shrink-0 text-muted">{k}</dt>
            <dd className="min-w-0 flex-1 truncate font-mono">{v}</dd>
            <CopyButton value={k === "Algorithm" || k === "Digest type" ? (v.split(" ")[0] ?? v) : v} />
          </div>
        ))}
      </dl>
    </details>
  );
}

// ------------------------------------------------------------------ DNS providers

function DnsProviders({ data, onChange }: { data: Domains; onChange: (v: Domains) => void }) {
  const [adding, setAdding] = useState(false);
  return (
    <section className="space-y-3" aria-label="DNS providers">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">DNS providers</h2>
          <p className="text-sm text-muted">
            With an API token, Allod adds and updates your domains' records itself. Hosts without an API (MyNymBox and
            others) work too: Allod then lists the records for you to add by hand.
          </p>
        </div>
        <Button size="sm" variant="secondary" onClick={() => setAdding(true)} aria-label="Add a DNS provider">
          <Plus size={16} weight="bold" /> Add
        </Button>
      </div>
      <Card>
        <CardContent className="space-y-2 pt-6 text-sm">
          {data.dns_providers.length === 0 && (
            <p className="text-muted">
              None yet. Supported: <span className="font-medium">deSEC</span> (free, signs with DNSSEC, works with a domain
              from any registrar) and <span className="font-medium">Njalla</span> (registrar, takes bitcoin).
            </p>
          )}
          {data.desec_signup && !["done", "removed"].includes(data.desec_signup.state) && (
            <SignupStatus kind="desec" s={data.desec_signup} />
          )}
          {data.njalla_signup && !["done", "removed"].includes(data.njalla_signup.state) && (
            <SignupStatus kind="njalla" s={data.njalla_signup} />
          )}
          {data.dns_providers.map((p) => (
            <ProviderRow
              key={p.id}
              p={p}
              onChange={onChange}
              account={
                data.desec_signup?.provider_id === p.id || data.njalla_signup?.provider_id === p.id ? p.kind : undefined
              }
            />
          ))}
        </CardContent>
      </Card>
      {adding && (
        <AddProvider
          canSignUp={{
            desec: !data.desec_signup || ["failed", "removed"].includes(data.desec_signup.state),
            njalla: !data.njalla_signup || ["failed", "removed"].includes(data.njalla_signup.state),
          }}
          reconnect={{
            desec: data.desec_signup?.state === "removed" ? data.desec_signup.email : null,
            njalla: data.njalla_signup?.state === "removed" ? data.njalla_signup.email : null,
          }}
          onClose={() => setAdding(false)}
          onDone={(v) => (onChange(v), setAdding(false))}
        />
      )}
    </section>
  );
}

function ProviderRow({
  p,
  onChange,
  account,
}: {
  p: DnsProviderEntry;
  onChange: (v: Domains) => void;
  /** An account Allod made at this provider: its login can be shown. */
  account?: DnsProviderKind;
}) {
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-2 rounded-lg border border-line p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="space-y-0.5">
          <span className="flex items-center gap-2 font-medium">
            <ShieldCheck size={16} weight="duotone" /> {p.label} <Badge>{PROVIDER_KIND[p.kind]}</Badge>
            {p.error ? <Badge tone="warn">problem</Badge> : p.checked_at ? <Badge tone="ok">working</Badge> : null}
          </span>
          <span className="block text-xs text-muted">
            {p.domains.length ? `Keeps records of ${p.domains.join(", ")}` : "No domain uses it yet"}
          </span>
          {p.error && <span className="block text-xs text-danger">{p.error}</span>}
          {account && <AccountLogin kind={account} />}
        </span>
        <span className="flex gap-1">
          <Button variant="ghost" size="sm" onClick={() => setEditing(!editing)}>
            New token
          </Button>
          <Button
            variant="ghost"
            size="sm"
            aria-label={`Remove ${p.label}`}
            onClick={async () => {
              setError(null);
              try {
                onChange(await api.removeProvider(p.id));
              } catch (e) {
                setError(message(e));
              }
            }}
          >
            <Trash size={16} />
          </Button>
        </span>
      </div>
      {editing && (
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            setError(null);
            try {
              onChange(await api.updateProvider(p.id, { token: token.trim() }));
              setEditing(false);
              setToken("");
            } catch (err) {
              setError(message(err));
            }
          }}
        >
          <Field label={`${PROVIDER_KIND[p.kind]} API token`}>
            <Input value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" data-1p-ignore data-lpignore="true" />
          </Field>
          <Button type="submit" size="sm" disabled={!token.trim()}>
            Save
          </Button>
        </form>
      )}
      {error && <Alert tone="danger" title="DNS provider">{error}</Alert>}
    </div>
  );
}

function AddProvider({
  canSignUp,
  reconnect,
  onClose,
  onDone,
}: {
  canSignUp: Record<DnsProviderKind, boolean>;
  /** The address of an account Allod made earlier whose provider was removed. */
  reconnect: Record<DnsProviderKind, string | null>;
  onClose: () => void;
  onDone: (v: Domains) => void;
}) {
  const [kind, setKind] = useState<DnsProviderKind>("desec");
  // deSEC: an account Allod makes itself, or the owner's own token.
  const [own, setOwn] = useState(!canSignUp.desec);
  const [label, setLabel] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const name = PROVIDER_KIND[kind];
  return (
    <Modal open onClose={onClose} title="Add a DNS provider" wide>
      <form
        className="space-y-4 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onDone(await api.addProvider({ kind, token: token.trim(), label: label.trim() || undefined }));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <div className="grid gap-2 sm:grid-cols-2" role="radiogroup" aria-label="DNS provider">
          {(
            [
              ["desec", "deSEC", "Free DNS hosting, signed with DNSSEC. Works with a domain from any registrar: you point the domain at deSEC."],
              ["njalla", "Njalla", "For domains registered at Njalla (paid in bitcoin); DNS and DNSSEC there."],
            ] as const
          ).map(([k, title, body]) => (
            <button
              key={k}
              type="button"
              role="radio"
              aria-checked={kind === k}
              onClick={() => setKind(k)}
              className={cn(
                "rounded-lg border p-3 text-left transition",
                kind === k ? "border-accent bg-accent/5" : "border-line hover:border-accent",
              )}
            >
              <span className="block font-medium">{title}</span>
              <span className="block text-xs text-muted">{body}</span>
            </button>
          ))}
        </div>
        {canSignUp[kind] && (
          <div className="flex gap-2 text-xs">
            <Button type="button" size="sm" variant={own ? "ghost" : "secondary"} onClick={() => setOwn(false)}>
              {reconnect[kind] ? "Use the account Allod made" : "Create an account for me"}
            </Button>
            <Button type="button" size="sm" variant={own ? "secondary" : "ghost"} onClick={() => setOwn(true)}>
              I have a {name} account
            </Button>
          </div>
        )}
        {kind === "desec" && !own && canSignUp.desec ? (
          <DesecSignupForm onDone={onDone} onCancel={onClose} reconnect={reconnect.desec} />
        ) : kind === "njalla" && !own && canSignUp.njalla ? (
          <NjallaSignupForm onDone={onDone} onCancel={onClose} reconnect={reconnect.njalla} />
        ) : kind === "desec" ? (
          <DesecGuide />
        ) : (
          <p className="text-muted">
            In your Njalla account, open Settings → API, create a token and paste it here. Allod checks it right away and
            keeps it on this machine only.
          </p>
        )}
        {(own || !canSignUp[kind]) && (
          <>
            <Field label="Name (optional)">
              <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder={name} />
            </Field>
            <Field label={`${name} API token`}>
              <Input
                value={token}
                onChange={(e) => setToken(e.target.value)}
                required
                autoComplete="off"
                name={`${kind}-token`}
                data-1p-ignore
                data-lpignore="true"
              />
            </Field>
            {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
            <Footer onCancel={onClose} busy={busy} label="Add provider" disabled={!token.trim()} />
          </>
        )}
      </form>
    </Modal>
  );
}

/** Allod makes a deSEC account with its own uid.ovh address: the owner only solves the
 * captcha. Not a form of its own: it sits inside the add-provider form. */
function DesecSignupForm({
  onDone,
  onCancel,
  reconnect,
}: {
  onDone: (v: Domains) => void;
  onCancel: () => void;
  reconnect: string | null;
}) {
  // An account Allod made before: log in to it again (no captcha), unless it is gone.
  const [tryReconnect, setTryReconnect] = useState(!!reconnect);
  const { data: captcha, error: loadError, mutate } = useSWR(
    tryReconnect ? null : "desec-captcha",
    () => api.desecCaptcha(),
    { revalidateOnFocus: false },
  );
  const [solution, setSolution] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const submit = async () => {
    if (!captcha) return;
    setBusy(true);
    setError(null);
    try {
      onDone(await api.desecSignup(captcha.id, solution));
    } catch (e) {
      setError(message(e));
      setSolution("");
      void mutate(); // a captcha works once
    } finally {
      setBusy(false);
    }
  };
  if (tryReconnect) {
    return (
      <div className="space-y-3">
        <p className="text-muted">
          Allod made a deSEC account before (<span className="font-mono">{reconnect}</span>). It logs in to it again and
          creates a new API token; your domains there stay as they are.
        </p>
        {error && <Alert tone="danger" title="deSEC">{error}</Alert>}
        <div className="flex flex-wrap justify-end gap-2">
          <Button type="button" variant="ghost" onClick={() => setTryReconnect(false)}>
            Make a new account instead
          </Button>
          <Button
            type="button"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                onDone(await api.desecSignup("", ""));
              } catch (e) {
                // The account is gone: a new one, with the captcha.
                setError(`${message(e)} — the account seems gone; solve the captcha for a new one.`);
                setTryReconnect(false);
              } finally {
                setBusy(false);
              }
            }}
          >
            {busy ? "Connecting…" : "Reconnect"}
          </Button>
        </div>
      </div>
    );
  }
  return (
    <div className="space-y-3">
      <p className="text-muted">
        Allod signs up at deSEC with its own email address (<span className="font-mono">npub…@uid.ovh</span>, read over
        Nostr), confirms the activation mail itself and keeps the API token. You only type the characters below; deSEC asks
        every new account for this.
      </p>
      <div className="flex flex-wrap items-center gap-3">
        {captcha ? (
          <img
            src={`data:image/png;base64,${captcha.challenge}`}
            alt="deSEC captcha"
            className="h-16 rounded border border-line bg-white"
          />
        ) : (
          <span className={loadError ? "text-danger" : "text-muted"}>
            {loadError ? `Cannot reach deSEC: ${message(loadError)}` : "Loading the captcha…"}
          </span>
        )}
        <Button type="button" variant="ghost" size="sm" onClick={() => void mutate()}>
          <ArrowsClockwise size={14} /> New picture
        </Button>
      </div>
      <Field label="Characters in the picture">
        <Input
          value={solution}
          onChange={(e) => setSolution(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              void submit();
            }
          }}
          autoComplete="off"
          autoCapitalize="characters"
          spellCheck={false}
        />
      </Field>
      <p className="text-xs text-muted">
        Then Allod waits for deSEC's mail (usually a minute or two), activates the account and adds it here. Domains you give
        it are created at deSEC too; you then point the domain's name servers at deSEC and add the DS record at the registrar
        (both shown in the domain's dialog).
      </p>
      {error && <Alert tone="danger" title="deSEC">{error}</Alert>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button type="button" disabled={busy || !captcha || !solution.trim()} onClick={() => void submit()}>
          {busy ? "Signing up…" : "Create the account"}
        </Button>
      </div>
    </div>
  );
}

/** Allod signs up at Njalla itself: no captcha, nothing to type. */
function NjallaSignupForm({
  onDone,
  onCancel,
  reconnect,
}: {
  onDone: (v: Domains) => void;
  onCancel: () => void;
  reconnect: string | null;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-3">
      <p className="text-muted">
        {reconnect ? (
          <>
            Allod made a Njalla account before (<span className="font-mono">{reconnect}</span>). It logs in to it again
            and creates a new API token; if the account is gone, it signs up anew.
          </>
        ) : (
          <>
            Allod signs up at Njalla with its own email address (<span className="font-mono">npub…@uid.ovh</span>, read
            over Nostr), confirms the mail itself and creates an API token. Nothing to type. You can then buy domains at
            Njalla (paid in bitcoin) with this account: Allod shows its login.
          </>
        )}
      </p>
      {error && <Alert tone="danger" title="Njalla">{error}</Alert>}
      <div className="flex justify-end gap-2">
        <Button type="button" variant="ghost" onClick={onCancel}>
          Cancel
        </Button>
        <Button
          type="button"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              onDone(await api.njallaSignup());
            } catch (e) {
              setError(message(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy ? "Signing up…" : reconnect ? "Reconnect" : "Create the account"}
        </Button>
      </div>
    </div>
  );
}

/** A sign-up Allod makes on its way (or failed). */
function SignupStatus({ kind, s }: { kind: DnsProviderKind; s: NonNullable<Domains["desec_signup"]> }) {
  const name = PROVIDER_KIND[kind];
  return (
    <div className="space-y-1 rounded-lg border border-line p-3">
      <span className="flex items-center gap-2 font-medium">
        <ShieldCheck size={16} weight="duotone" /> {name} account
        {s.state === "failed" ? <Badge tone="danger">failed</Badge> : <Badge tone="warn">being activated</Badge>}
      </span>
      <span className="block text-xs text-muted">
        {s.state === "failed"
          ? `Start again with Add → ${name}.`
          : `Signed up as ${s.email}; waiting for ${name}'s confirmation mail (usually a minute or two).`}
      </span>
      {s.error && <span className="block text-xs text-warn">{s.error}</span>}
    </div>
  );
}

/** The login of an account Allod made, to use the provider's website too. */
function AccountLogin({ kind }: { kind: DnsProviderKind }) {
  const [creds, setCreds] = useState<{ email: string; password: string | null } | null>(null);
  const [error, setError] = useState<string | null>(null);
  if (!creds) {
    return (
      <span className="block text-xs">
        <button
          type="button"
          className="underline"
          onClick={async () => {
            try {
              setCreds(await (kind === "desec" ? api.desecCredentials() : api.njallaCredentials()));
            } catch (e) {
              setError(message(e));
            }
          }}
        >
          Show the {kind === "desec" ? "desec.io" : "njal.la"} login
        </button>
        {error && <span className="ml-2 text-danger">{error}</span>}
      </span>
    );
  }
  return (
    <span className="mt-1 block space-y-1 text-xs">
      <span className="flex items-center gap-2">
        <span className="w-16 text-muted">Email</span>
        <span className="min-w-0 truncate font-mono">{creds.email}</span>
        <CopyButton value={creds.email} />
      </span>
      {creds.password && (
        <span className="flex items-center gap-2">
          <span className="w-16 text-muted">Password</span>
          <span className="min-w-0 truncate font-mono">{creds.password}</span>
          <CopyButton value={creds.password} />
        </span>
      )}
      <span className="block text-muted">
        Created by Allod; its mail arrives only in Allod's Nostr mailbox, so keep Allod's backup.
      </span>
    </span>
  );
}

/** How to move a domain's DNS to deSEC, from any registrar, so Allod can keep its records. */
function DesecGuide() {
  const link = (href: string, text: string) => (
    <a className="underline" href={href} target="_blank" rel="noreferrer">
      {text}
    </a>
  );
  return (
    <div className="space-y-2">
      <p className="text-muted">
        Your domain stays at its registrar (MyNymBox, Namecheap, …); only its DNS moves to deSEC. Then Allod writes the A and
        BIP-353 records itself, signed with DNSSEC.
      </p>
      <ol className="list-decimal space-y-2 pl-5">
        <li>
          Create a free account at {link("https://desec.io/signup", "desec.io")} and add your domain under{" "}
          <span className="font-medium">Domain management → +</span>.
        </li>
        <li>
          Copy any records you still need into deSEC first (mail: MX, SPF/DKIM TXT; websites: A/CNAME). In deSEC the{" "}
          <span className="font-medium">Subname</span> is the part before your domain: leave it empty for the domain itself.
          Allod adds its own records later, no need to copy those.
        </li>
        <li>
          At the registrar, set the domain's name servers to{" "}
          <code className="font-mono">ns1.desec.io</code> and <code className="font-mono">ns2.desec.org</code>.
        </li>
        <li>
          At the registrar, in the DNSSEC settings, add the DS record deSEC shows for the domain (key tag, algorithm 13,
          digest type 2, digest). Allod also shows it in the domain's dialog once connected. Do this after step 3 is live,
          never before.
        </li>
        <li>
          In deSEC, open <span className="font-medium">Token management</span>, create a token (you may limit it to this
          domain) and paste it below.
        </li>
        <li>
          Add the domain in <span className="font-medium">Domains</span> with this provider (or pick it in an existing
          domain's settings).
        </li>
      </ol>
      <p className="text-xs text-muted">
        Name server changes take minutes to a few hours to reach everyone. Allod's checks show when the domain answers and
        is signed.
      </p>
    </div>
  );
}

export function Footer({
  onCancel,
  busy,
  label,
  disabled,
  onSubmit,
}: {
  onCancel: () => void;
  busy: boolean;
  label: ReactNode;
  disabled?: boolean;
  /** Outside a form: the button's action. */
  onSubmit?: () => void;
}) {
  return (
    <div className="flex justify-end gap-2">
      <Button variant="ghost" onClick={onCancel}>
        Cancel
      </Button>
      <Button type={onSubmit ? "button" : "submit"} onClick={onSubmit} disabled={busy || disabled}>
        {busy ? "Working…" : label}
      </Button>
    </div>
  );
}
