import { useEffect, useState } from "react";
import { Link } from "react-router";
import { CaretRight, CloudArrowUp, DownloadSimple, HardDrives, Plus, Trash } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { CopyButton, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { domains as api, type Domains, type GatewayEntry, type GatewayState, type LnvpsQuote } from "../../lib/api";
import { cn } from "../../lib/utils";
import { Footer, message, useDomains } from "./Domains";

/**
 * Settings → Gateways (SPEC §12.7, §12.8): small public servers that pass HTTPS through
 * to this node over WireGuard, so domains answer every wallet without showing the home
 * IP. The owner's own server, or one Allod rents at LNVPS.
 */

const STATE: Record<GatewayState, { tone: "ok" | "warn" | "danger" | "neutral"; label: string }> = {
  active: { tone: "ok", label: "Active" },
  setup: { tone: "warn", label: "Waiting for the first connection" },
  offline: { tone: "danger", label: "Offline" },
  ordering: { tone: "warn", label: "Being bought" },
  expired: { tone: "danger", label: "Expired" },
  error: { tone: "danger", label: "Problem" },
};

function ago(t: number | null): string {
  if (!t) return "never";
  const s = Math.max(0, Math.floor(Date.now() / 1000) - t);
  if (s < 90) return `${s} s ago`;
  if (s < 5400) return `${Math.round(s / 60)} min ago`;
  if (s < 172_800) return `${Math.round(s / 3600)} h ago`;
  return `${Math.round(s / 86_400)} days ago`;
}

function date(t: number | null): string {
  return t ? new Date(t * 1000).toLocaleDateString() : "—";
}

export function GatewaySettings() {
  const { data, error, mutate } = useDomains();
  const [adding, setAdding] = useState(false);
  const [open, setOpen] = useState<number | null>(null);
  if (error) return <Alert tone="danger" title="Could not load gateways">{message(error)}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  const update = (v: Domains) => void mutate(v, { revalidate: false });
  const current = data.gateways.find((g) => g.id === open);
  return (
    <div className="space-y-6">
      <section className="space-y-3" aria-label="Gateways">
        <div className="flex items-start justify-between gap-3">
          <div>
            <h2 className="text-lg font-semibold">Gateways</h2>
            <p className="text-sm text-muted">
              A small public server that gives your domains a fixed address and passes HTTPS through to this node over
              WireGuard, without decrypting it. Your home IP stays hidden, and the server cannot read or change invoices: it
              can only block them.
            </p>
          </div>
          <Button size="sm" onClick={() => setAdding(true)} aria-label="Add a gateway">
            <Plus size={16} weight="bold" /> Add
          </Button>
        </div>
        {!data.tunnel && (
          <Alert tone="warn" title="No tunnel service here">
            This installation does not run the tunnel service (the Compose bundle's https profile), so gateways cannot
            reach this node yet.
          </Alert>
        )}
        {data.gateways.length === 0 ? (
          <Card>
            <CardContent className="space-y-3 pt-6 text-sm">
              <p className="text-base font-semibold">No gateway yet</p>
              <p className="text-muted">
                Without one, a domain answers BIP-353 wallets only, or every wallet from this machine's public IP. Use a
                server you already have, or let Allod rent one at LNVPS and pay it in sats.
              </p>
              <Button onClick={() => setAdding(true)}>
                <Plus size={16} weight="bold" /> Add a gateway
              </Button>
            </CardContent>
          </Card>
        ) : (
          <Card>
            <CardContent className="space-y-2 pt-6">
              {data.gateways.map((g) => (
                <button
                  key={g.id}
                  type="button"
                  onClick={() => setOpen(g.id)}
                  className={cn(
                    "flex w-full flex-wrap items-center justify-between gap-2 rounded-lg border p-3 text-left text-sm transition hover:border-accent",
                    g.state === "active" ? "border-line" : "border-warn/60",
                  )}
                >
                  <span className="min-w-0 space-y-1">
                    <span className="flex flex-wrap items-center gap-2">
                      {g.provider === "lnvps" ? <CloudArrowUp size={16} weight="duotone" /> : <HardDrives size={16} weight="duotone" />}
                      <span className="font-medium">{g.label}</span>
                      <span className="font-mono text-xs text-muted">{g.ipv4 ?? "no IP yet"}</span>
                      <Badge tone={STATE[g.state].tone}>{STATE[g.state].label}</Badge>
                    </span>
                    <span className="flex flex-wrap items-center gap-1.5 text-xs text-muted">
                      <span>{g.provider === "lnvps" ? "LNVPS" : "Your server"}</span>
                      <span aria-hidden>·</span>
                      <span>{g.domains.length ? g.domains.join(", ") : "no domain uses it"}</span>
                      {g.expires_at && (
                        <>
                          <span aria-hidden>·</span>
                          <span>paid until {date(g.expires_at)}</span>
                        </>
                      )}
                    </span>
                    {g.error && <span className="block text-xs text-danger">{g.error}</span>}
                  </span>
                  <CaretRight size={16} className="text-muted" />
                </button>
              ))}
              <p className="pt-1 text-xs text-muted">
                Serve a domain through a gateway in{" "}
                <Link className="underline" to="/settings/domains">
                  Domains
                </Link>
                .
              </p>
            </CardContent>
          </Card>
        )}
      </section>
      <section className="space-y-2" aria-label="Gateway providers">
        <h2 className="text-lg font-semibold">Gateway providers</h2>
        <Card>
          <CardContent className="space-y-3 pt-6 text-sm">
            <div>
              <p className="flex items-center gap-2 font-medium">
                <CloudArrowUp size={16} weight="duotone" /> LNVPS <Badge>paid in sats</Badge>
              </p>
              <p className="text-muted">
                Allod signs in with its own Nostr key, buys the smallest monthly server, pays it from Main, sets it up over
                SSH and renews it within the limit you set. LNVPS can see the server's traffic (encrypted) and could switch
                it off; it cannot read payments.
              </p>
              {data.lnvps.account_pubkey && (
                <p className="mt-1 flex items-center gap-2 text-xs text-muted">
                  Account key <span className="truncate font-mono">{data.lnvps.account_pubkey}</span>
                  <CopyButton value={data.lnvps.account_pubkey} />
                </p>
              )}
            </div>
            <div>
              <p className="flex items-center gap-2 font-medium">
                <HardDrives size={16} weight="duotone" /> Your own server
              </p>
              <p className="text-muted">
                Any Debian or Ubuntu server with a public IPv4 (mikr.us, a VPS you already rent, …). Allod gives you a setup
                script to run there once.
              </p>
            </div>
          </CardContent>
        </Card>
      </section>
      {adding && <AddGateway onClose={() => setAdding(false)} onDone={(v) => (update(v), setAdding(false))} />}
      {current && <GatewayDetail g={current} onChange={update} onClose={() => setOpen(null)} />}
    </div>
  );
}

// ------------------------------------------------------------------ adding

function AddGateway({ onClose, onDone }: { onClose: () => void; onDone: (v: Domains) => void }) {
  const [kind, setKind] = useState<"manual" | "lnvps" | null>(null);
  return (
    <Modal open onClose={onClose} title="Add a gateway">
      {kind === null && (
        <div className="space-y-2 text-sm">
          {[
            {
              id: "manual" as const,
              icon: HardDrives,
              label: "A server I already have",
              body: "Debian or Ubuntu with a public IPv4. You run Allod's setup script on it once.",
            },
            {
              id: "lnvps" as const,
              icon: CloudArrowUp,
              label: "Rent one at LNVPS",
              body: "Allod buys a small server, pays it in sats from Main, sets it up and renews it.",
            },
          ].map((o) => (
            <button
              key={o.id}
              type="button"
              onClick={() => setKind(o.id)}
              className="flex w-full items-center gap-3 rounded-lg border border-line p-4 text-left transition hover:border-accent"
            >
              <o.icon size={22} weight="duotone" className="shrink-0" />
              <span>
                <span className="block font-medium">{o.label}</span>
                <span className="block text-muted">{o.body}</span>
              </span>
            </button>
          ))}
        </div>
      )}
      {kind === "manual" && <ManualForm onCancel={() => setKind(null)} onDone={onDone} />}
      {kind === "lnvps" && <LnvpsForm onCancel={() => setKind(null)} onDone={onDone} />}
    </Modal>
  );
}

function ManualForm({ onCancel, onDone }: { onCancel: () => void; onDone: (v: Domains) => void }) {
  const [label, setLabel] = useState("");
  const [ipv4, setIpv4] = useState("");
  const [mode, setMode] = useState<"dedicated" | "shared">("dedicated");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          onDone(await api.addGateway({ provider: "manual", ipv4: ipv4.trim(), mode, label: label.trim() || undefined }));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <Field label="Name (optional)">
        <Input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="Dublin VPS" />
      </Field>
      <Field label="Public IPv4 of the server">
        <Input value={ipv4} onChange={(e) => setIpv4(e.target.value)} placeholder="203.0.113.10" required />
      </Field>
      <Field label="Port 443 on that server" hint="Shared: other websites keep working; HTTPS is routed by domain name.">
        <select
          className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
          value={mode}
          onChange={(e) => setMode(e.target.value as "dedicated" | "shared")}
        >
          <option value="dedicated">Only for Allod (all of port 443 comes here)</option>
          <option value="shared">Shared with other websites (routed by domain name)</option>
        </select>
      </Field>
      {error && <Alert tone="danger" title="Could not add it">{error}</Alert>}
      <Footer onCancel={onCancel} busy={busy} label="Add and get the setup script" disabled={!ipv4.trim()} />
    </form>
  );
}

/** Money in cents of `currency`, e.g. "€6.15". */
function fiat(cents: number, currency: string): string {
  try {
    return new Intl.NumberFormat(undefined, { style: "currency", currency }).format(cents / 100);
  } catch {
    return `${(cents / 100).toFixed(2)} ${currency}`;
  }
}

function LnvpsForm({ onCancel, onDone }: { onCancel: () => void; onDone: (v: Domains) => void }) {
  const [quote, setQuote] = useState<LnvpsQuote | null>(null);
  const [quoteError, setQuoteError] = useState<string | null>(null);
  // The cap, in LNVPS's currency (VAT and fees included), converted when an invoice arrives.
  const [cap, setCap] = useState("10");
  const [autoRenew, setAutoRenew] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    api
      .lnvpsQuote()
      .then(setQuote)
      .catch((e) => setQuoteError(message(e)));
  }, []);
  const currency = quote?.total_fiat.currency || "EUR";
  const capAmount = Number(cap.replace(",", ".")) || 0;
  const tooLow = !!quote && capAmount * 100 < quote.total_fiat.amount;
  return (
    <form
      className="space-y-4 text-sm"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          onDone(
            await api.addGateway({
              provider: "lnvps",
              cap: { currency, amount: capAmount },
              auto_renew: autoRenew,
            }),
          );
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      {quoteError && <Alert tone="danger" title="LNVPS">{quoteError}</Alert>}
      {!quote && !quoteError && <p className="text-muted">Asking LNVPS for its price…</p>}
      {quote && (
        <div className="space-y-1 rounded-lg bg-surface-2 p-3">
          <p className="text-base font-semibold">
            {quote.total_sats.toLocaleString()} sats a {quote.interval}{" "}
            <span className="font-normal text-muted">
              ({fiat(quote.total_fiat.amount, quote.total_fiat.currency)})
            </span>
          </p>
          <p className="text-xs text-muted">
            Including {quote.vat_rate}% VAT{quote.fee_rate ? ` and a ${quote.fee_rate}% payment fee` : ""}; the server
            itself is {quote.price_sats.toLocaleString()} sats.
          </p>
          <p className="text-muted">
            {quote.plan.cpu} CPU, {Math.round(quote.plan.memory / 2 ** 30)} GiB memory,{" "}
            {Math.round(quote.plan.disk / 2 ** 30)} GB disk, {quote.plan.image}, in {quote.plan.region}.
          </p>
        </div>
      )}
      <Field
        label={`Pay at most (${currency}, VAT and fees included)`}
        hint="Converted to sats when each invoice arrives, so it follows the exchange rate. Allod refuses an invoice above it."
      >
        <Input value={cap} onChange={(e) => setCap(e.target.value.replace(/[^\d.,]/g, ""))} inputMode="decimal" required />
      </Field>
      {tooLow && <p className="text-xs text-danger">Below the price: Allod would refuse the invoice.</p>}
      <label className="flex items-start gap-2">
        <input type="checkbox" className="mt-1" checked={autoRenew} onChange={(e) => setAutoRenew(e.target.checked)} />
        <span>
          Renew every {quote?.interval ?? "month"} automatically, 3 days before it ends, if it costs at most this.
          <span className="block text-xs text-muted">Without renewal the server stops when the paid time ends.</span>
        </span>
      </label>
      <p className="text-xs text-muted">
        LNVPS wants a verified email: Allod verifies one by itself, its account key's address at uid.ovh (email over Nostr).
      </p>
      {error && <Alert tone="danger" title="Could not buy it">{error}</Alert>}
      <Footer
        onCancel={onCancel}
        busy={busy}
        label={quote ? `Buy for ${quote.total_sats.toLocaleString()} sats` : "Buy"}
        disabled={!quote || tooLow || capAmount <= 0}
      />
    </form>
  );
}

// ------------------------------------------------------------------ one gateway

function GatewayDetail({ g, onChange, onClose }: { g: GatewayEntry; onChange: (v: Domains) => void; onClose: () => void }) {
  const [script, setScript] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [removing, setRemoving] = useState(false);
  const [email, setEmail] = useState("");
  const [label, setLabel] = useState(g.label);
  // The login on the server; the script needs root, so other users run it with sudo.
  const sshKey = `allod.gateway.${g.id}.ssh-user`;
  const [sshUser, setSshUser] = useState(() => {
    try {
      return localStorage.getItem(sshKey) || "root";
    } catch {
      return "root";
    }
  });
  const user = sshUser.trim() || "root";
  const sshCommand = `ssh ${user}@${g.ipv4} '${user === "root" ? "sh -s" : "sudo sh -s"}' < allod-gateway.sh`;
  const [capInput, setCapInput] = useState(String(g.lnvps?.cap?.amount ?? ""));
  const run = async (f: () => Promise<Domains>, after?: () => void) => {
    setError(null);
    try {
      onChange(await f());
      after?.();
    } catch (e) {
      setError(message(e));
    }
  };
  const loadScript = async () => {
    setError(null);
    try {
      setScript(await api.setupScript(g.id));
    } catch (e) {
      setError(message(e));
    }
  };
  const download = () => {
    if (!script) return;
    const url = URL.createObjectURL(new Blob([script], { type: "text/x-shellscript" }));
    const a = document.createElement("a");
    a.href = url;
    a.download = "allod-gateway.sh";
    a.click();
    URL.revokeObjectURL(url);
  };
  const p = g.lnvps;
  return (
    <Modal open onClose={onClose} title={g.label} wide>
      <div className="space-y-5 text-sm">
        <div className="flex flex-wrap items-center gap-2 text-xs text-muted">
          <Badge tone={STATE[g.state].tone}>{STATE[g.state].label}</Badge>
          <span className="font-mono">{g.ipv4 ?? "no IP yet"}</span>
          <span aria-hidden>·</span>
          <span>last tunnel handshake {ago(g.last_handshake_at)}</span>
          <span aria-hidden>·</span>
          <span>{g.mode === "shared" ? "port 443 shared, routed by name" : "port 443 only for Allod"}</span>
        </div>
        {g.error && <Alert tone="danger" title="Problem">{g.error}</Alert>}
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={(e) => {
            e.preventDefault();
            if (label.trim()) void run(() => api.updateGateway(g.id, { label: label.trim() }));
          }}
        >
          <div className="min-w-48 flex-1">
            <Field label="Name">
              <Input value={label} onChange={(e) => setLabel(e.target.value)} maxLength={60} />
            </Field>
          </div>
          <Button type="submit" size="sm" variant="secondary" disabled={!label.trim() || label.trim() === g.label}>
            Rename
          </Button>
        </form>
        <p className="text-muted">
          Domains: {g.domains.length ? g.domains.join(", ") : "none yet"}. Tunnel {g.tunnel.gateway} ↔ {g.tunnel.home}, UDP{" "}
          {g.tunnel.port} open on the server.
        </p>

        {p && (
          <section className="space-y-2 rounded-lg bg-surface-2 p-3">
            <h3 className="font-medium">LNVPS</h3>
            <ol className="space-y-1 text-xs">
              {(["order", "pay", "provision", "setup", "done"] as const).map((s, i, all) => {
                const at = all.indexOf(p.step === "renew" ? "done" : p.step);
                return (
                  <li key={s} className={i < at ? "text-ok" : i === at ? "font-medium" : "text-muted"}>
                    {i < at ? "✓" : i === at ? "→" : "·"}{" "}
                    {
                      {
                        order: "Order the server",
                        pay: "Pay it from Main",
                        provision: "Wait for its IP address",
                        setup: "Set it up over SSH",
                        done: g.state === "active" ? "Running" : "Wait for the tunnel",
                      }[s]
                    }
                  </li>
                );
              })}
            </ol>
            <p className="text-xs text-muted">
              {p.plan.image}, {p.plan.region}. Paid so far: {(p.paid_sats ?? 0).toLocaleString()} sats. Paid until{" "}
              {date(g.expires_at)}.
            </p>
            {p.email_needed && (
              <p className="text-xs text-muted">
                {p.email?.endsWith("@uid.ovh")
                  ? `Verifying the email LNVPS asked for: ${p.email} (email over Nostr). This takes a minute or two.`
                  : p.email
                    ? `Click the link LNVPS sent to ${p.email}; Allod continues by itself.`
                    : "LNVPS asked for a verified email; Allod is setting one up."}
              </p>
            )}
            {p.email_needed && (
              <form
                className="flex flex-wrap items-end gap-2"
                onSubmit={(e) => {
                  e.preventDefault();
                  void run(() => api.lnvpsEmail(email.trim()));
                }}
              >
                <Field label="Or use your own email" hint="Then click the link LNVPS sends; Allod continues by itself.">
                  <Input value={email} onChange={(e) => setEmail(e.target.value)} type="email" required />
                </Field>
                <Button type="submit" size="sm">
                  Send
                </Button>
              </form>
            )}
            <label className="flex items-center gap-2">
              <input
                type="checkbox"
                checked={g.auto_renew}
                onChange={(e) =>
                  void run(() =>
                    api.updateGateway(g.id, { auto_renew: e.target.checked }),
                  )
                }
              />
              Renew automatically
            </label>
<form
              className="flex flex-wrap items-end gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                const amount = Number(capInput.replace(",", ".")) || 0;
                void run(() =>
                  api.updateGateway(g.id, {
                    cap: amount > 0 ? { currency: p.cap?.currency ?? "EUR", amount } : null,
                  }),
                );
              }}
            >
              <Field
                label={`Pay at most per ${p.plan.interval} (${p.cap?.currency ?? "EUR"}, VAT and fees included)`}
                hint={
                  p.cap
                    ? "Converted to sats when each invoice arrives."
                    : `Now a sats limit: ${(g.renew_max_sats ?? 0).toLocaleString()} sats for renewals.`
                }
              >
                <Input value={capInput} onChange={(e) => setCapInput(e.target.value.replace(/[^\d.,]/g, ""))} inputMode="decimal" />
              </Field>
              <Button type="submit" size="sm" variant="secondary" disabled={String(p.cap?.amount ?? "") === capInput}>
                Save
              </Button>
            </form>
          </section>
        )}

        {g.provider === "manual" && (
          <section className="space-y-2">
            <h3 className="font-medium">Setup script</h3>
            <p className="text-muted">
              Run it once as root on the server (Debian or Ubuntu); run it again after changing this gateway
              {g.mode === "shared" ? " or the domains it serves" : ""}. It contains the gateway's tunnel key: keep it private.
            </p>
            {script ? (
              <>
                <Field label="SSH user on the server" hint="Not root? The command runs the script with sudo.">
                  <Input
                    value={sshUser}
                    onChange={(e) => {
                      setSshUser(e.target.value);
                      try {
                        localStorage.setItem(sshKey, e.target.value);
                      } catch {
                        // Private window: not remembered.
                      }
                    }}
                    autoComplete="off"
                    placeholder="root"
                  />
                </Field>
                <div className="flex flex-wrap items-center gap-2 rounded-lg bg-surface-2 p-2 font-mono text-xs">
                  <span className="min-w-0 flex-1 truncate">{sshCommand}</span>
                  <CopyButton value={sshCommand} />
                </div>
                <div className="flex gap-2">
                  <Button size="sm" onClick={download}>
                    <DownloadSimple size={16} /> Download allod-gateway.sh
                  </Button>
                  <CopyButton value={script} label="Copy script" />
                </div>
                <pre className="max-h-64 overflow-auto rounded-lg bg-surface-2 p-2 text-xs">{script}</pre>
              </>
            ) : (
              <Button size="sm" variant="secondary" onClick={() => void loadScript()}>
                Show the setup script
              </Button>
            )}
          </section>
        )}

        {error && <Alert tone="danger" title="Gateway">{error}</Alert>}
        <div className="flex justify-end border-t border-line pt-3">
          <Button variant="ghost" size="sm" onClick={() => setRemoving(true)}>
            <Trash size={16} /> Remove gateway
          </Button>
        </div>
      </div>
      <Modal open={removing} onClose={() => setRemoving(false)} title={`Remove ${g.label}?`}>
        <div className="space-y-4 text-sm">
          <p>
            {g.provider === "lnvps"
              ? "Allod stops renewing it and forgets it; LNVPS deletes the server a week after the paid time ends."
              : "Allod forgets it and closes its tunnel. Remove the setup from the server yourself if you no longer need it."}{" "}
            Domains it serves must be moved first.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setRemoving(false)}>
              Cancel
            </Button>
            <Button variant="danger" onClick={() => void run(() => api.removeGateway(g.id), onClose)}>
              Remove
            </Button>
          </div>
        </div>
      </Modal>
    </Modal>
  );
}
