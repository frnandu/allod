import { useState, type ReactNode } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { ArrowsClockwise, CaretRight, Globe, LockSimple, Plus, ShieldCheck, Trash } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { CopyButton, Field, Modal, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  domains as api,
  DOMAINS_KEY,
  fetcher,
  type DnsProviderEntry,
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
    d.gateways.some((g) => g.state === "ordering" || g.state === "setup")
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

function providerName(d: DomainEntry, providers: DnsProviderEntry[]): string {
  const p = providers.find((x) => x.id === d.dns_provider_id);
  return p ? `${p.label} (Njalla)` : "Manual";
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
          <Button size="sm" onClick={() => setAdding(true)} aria-label="Add a domain">
            <Plus size={16} weight="bold" /> Add
          </Button>
        </div>
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
              <Button onClick={() => setAdding(true)}>
                <Plus size={16} weight="bold" /> Add my domain
              </Button>
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
            Kept by Allod at {p.label} (Njalla)
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
          <p className="text-xs text-muted">Domain at Njalla? Add it under DNS providers below and Allod writes the records itself.</p>
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
        <DomainChecks d={d} />

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
function DomainChecks({ d }: { d: DomainEntry }) {
  const c = d.checks;
  if (!c) return <p className="text-xs text-muted">Checking the domain…</p>;
  const rows: { ok: boolean; label: string; fix: string }[] = [
    { ok: c.dns, label: "DNS answers for it", fix: "Its name servers do not answer: check the domain is registered and delegated." },
    {
      ok: c.dnssec,
      label: "Signed with DNSSEC",
      fix: "Turn DNSSEC on at the registrar (Njalla: the domain's DNSSEC settings). BIP-353 wallets refuse unsigned records.",
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
              None yet. Supported: <span className="font-medium">Njalla</span> (takes bitcoin, DNSSEC).
            </p>
          )}
          {data.dns_providers.map((p) => (
            <ProviderRow key={p.id} p={p} onChange={onChange} />
          ))}
        </CardContent>
      </Card>
      {adding && <AddProvider onClose={() => setAdding(false)} onDone={(v) => (onChange(v), setAdding(false))} />}
    </section>
  );
}

function ProviderRow({ p, onChange }: { p: DnsProviderEntry; onChange: (v: Domains) => void }) {
  const [editing, setEditing] = useState(false);
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  return (
    <div className="space-y-2 rounded-lg border border-line p-3">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="space-y-0.5">
          <span className="flex items-center gap-2 font-medium">
            <ShieldCheck size={16} weight="duotone" /> {p.label} <Badge>Njalla</Badge>
            {p.error ? <Badge tone="warn">problem</Badge> : p.checked_at ? <Badge tone="ok">working</Badge> : null}
          </span>
          <span className="block text-xs text-muted">
            {p.domains.length ? `Keeps records of ${p.domains.join(", ")}` : "No domain uses it yet"}
          </span>
          {p.error && <span className="block text-xs text-danger">{p.error}</span>}
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
          <Field label="Njalla API token">
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

function AddProvider({ onClose, onDone }: { onClose: () => void; onDone: (v: Domains) => void }) {
  const [label, setLabel] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title="Add a DNS provider">
      <form
        className="space-y-4 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onDone(await api.addProvider({ kind: "njalla", token: token.trim(), label: label.trim() || undefined }));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <p className="text-muted">
          In your Njalla account, open Settings → API, create a token and paste it here. Allod checks it right away and
          keeps it on this machine only.
        </p>
        <Field label="Name (optional)">
          <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Njalla" />
        </Field>
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
        {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
        <Footer onCancel={onClose} busy={busy} label="Add provider" disabled={!token.trim()} />
      </form>
    </Modal>
  );
}

export function Footer({
  onCancel,
  busy,
  label,
  disabled,
}: {
  onCancel: () => void;
  busy: boolean;
  label: ReactNode;
  disabled?: boolean;
}) {
  return (
    <div className="flex justify-end gap-2">
      <Button variant="ghost" onClick={onCancel}>
        Cancel
      </Button>
      <Button type="submit" disabled={busy || disabled}>
        {busy ? "Working…" : label}
      </Button>
    </div>
  );
}
