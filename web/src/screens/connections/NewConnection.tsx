import { Warning } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field, QrBlock } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, type AppConn } from "../../lib/api";
import { parseSats } from "../../lib/format";
import { cn } from "../../lib/utils";
import { ScopeForm, type ScopeFormValue } from "./ScopeForm";
import { EXPIRIES, PRESETS, type Preset } from "./scopes";

export function NewConnection() {
  const [name, setName] = useState("");
  const [preset, setPreset] = useState<Preset>("budget");
  const [form, setForm] = useState<ScopeFormValue>({ scopes: PRESETS.find((p) => p.id === "budget")?.scopes ?? [], budgetSats: "10000", renewal: "monthly", isolated: false });
  const [expiry, setExpiry] = useState<number | null>(null);
  const [relays, setRelays] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<{ app: AppConn; pairing_uri: string } | null>(null);

  const choose = (p: Preset) => {
    setPreset(p);
    const def = PRESETS.find((x) => x.id === p)!;
    if (p !== "custom") setForm({ ...form, scopes: def.scopes, budgetSats: def.budgetSats ? String(def.budgetSats) : "" });
  };

  if (created) {
    return (
      <div className="mx-auto max-w-lg space-y-6">
        <h1 className="text-3xl font-semibold">Connect “{created.app.name}”</h1>
        <Alert tone="warn" title="Shown only once">
          This link is a secret: anyone who has it can use the connection within its permissions. Allod does not keep it; if you lose it,
          create a new link from the connection page.
        </Alert>
        <Card>
          <CardContent className="space-y-4 pt-6">
            <QrBlock value={created.pairing_uri} caption="Scan with a Nostr Wallet Connect app" />
            <p className="break-all rounded-lg bg-surface-2 p-3 font-mono text-xs">{created.pairing_uri}</p>
            <CopyButton value={created.pairing_uri} label="Copy connection link" />
          </CardContent>
        </Card>
        <Link to={`/connections/${created.app.id}`} className="text-sm text-accent">
          Done, go to the connection
        </Link>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-2xl space-y-6">
      <h1 className="text-3xl font-semibold">New connection</h1>
      <Card>
        <CardHeader>
          <CardTitle>App</CardTitle>
        </CardHeader>
        <CardContent className="space-y-5">
          <Field label="Name">
            <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="e.g. Damus, Primal, my shop" maxLength={100} />
          </Field>
          <div className="flex flex-wrap gap-2">
            {PRESETS.map((p) => (
              <button
                key={p.id}
                type="button"
                onClick={() => choose(p.id)}
                className={cn("rounded-full border px-4 py-1.5 text-sm", preset === p.id ? "border-accent bg-accent-soft text-accent" : "border-line text-muted hover:text-fg")}
              >
                {p.label}
              </button>
            ))}
          </div>
          <ScopeForm
            value={form}
            onChange={(v) => {
              setForm(v);
              setPreset("custom");
            }}
          />
          <Field label="Expires">
            <div className="flex flex-wrap gap-2">
              {EXPIRIES.map((e) => (
                <button
                  key={e.label}
                  type="button"
                  onClick={() => setExpiry(e.days)}
                  className={cn("rounded-full border px-3 py-1 text-sm", expiry === e.days ? "border-accent text-accent" : "border-line text-muted")}
                >
                  {e.label}
                </button>
              ))}
            </div>
          </Field>
          <details className="text-sm">
            <summary className="cursor-pointer text-muted">Advanced</summary>
            <div className="pt-3">
              <Field label="Relays" hint="One per line. Empty: the node's default relays.">
                <textarea
                  className="min-h-20 w-full rounded-lg border border-line bg-surface p-2 font-mono text-xs"
                  value={relays}
                  onChange={(e) => setRelays(e.target.value)}
                  placeholder="wss://relay.example"
                />
              </Field>
            </div>
          </details>
          {form.scopes.includes("pay_invoice") && !parseSats(form.budgetSats) && (
            <Alert tone="warn" title="No budget">
              <Warning size={14} className="inline" /> This app can spend everything the connection can reach.
            </Alert>
          )}
          {error && <Alert tone="danger" title="Could not create the connection">{error}</Alert>}
          <Button
            className="w-full"
            disabled={busy || !name.trim()}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                const budget = parseSats(form.budgetSats);
                const relayList = relays.split(/\s+/).filter(Boolean);
                const r = await api.post<{ app: AppConn; pairing_uri: string }>("/api/apps", {
                  name: name.trim(),
                  scopes: form.scopes,
                  max_amount_msat: form.scopes.includes("pay_invoice") && budget ? budget * 1000 : 0,
                  budget_renewal: form.renewal,
                  isolated: form.isolated,
                  expires_at: expiry ? Math.floor(Date.now() / 1000) + expiry * 86400 : undefined,
                  relay_urls: relayList.length ? relayList : undefined,
                });
                setCreated(r);
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              } finally {
                setBusy(false);
              }
            }}
          >
            Create connection
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}
