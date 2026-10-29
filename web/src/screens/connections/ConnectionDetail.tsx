import { useState } from "react";
import { useNavigate, useParams } from "react-router";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Empty, Field, Modal, QrBlock } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type AppConn } from "../../lib/api";
import { ago, dateTime, parseSats } from "../../lib/format";
import { useNow } from "../../lib/useNow";
import { shorten } from "../../lib/utils";
import { BudgetBar } from "./BudgetBar";
import { ScopeForm, type ScopeFormValue } from "./ScopeForm";
import { scopeLabel } from "./scopes";

export function ConnectionDetail() {
  const { id } = useParams();
  const nav = useNavigate();
  const { data: a, mutate, error } = useSWR<AppConn>(`/api/apps/${id}`, fetcher, { refreshInterval: 15000 });
  const [editing, setEditing] = useState(false);
  const [revoking, setRevoking] = useState(false);
  const [uri, setUri] = useState<string | null>(null);
  const [err, setErr] = useState<string | null>(null);
  const now = useNow();

  if (error) return <p className="text-sm text-muted">{String(error.message)}</p>;
  if (!a) return <p className="text-sm text-muted">Loading…</p>;
  const expired = a.expires_at != null && a.expires_at <= now;

  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center gap-3">
        <h1 className="text-3xl font-semibold">{a.name}</h1>
        {a.isolated && <Badge tone="accent">isolated</Badge>}
        {expired && <Badge tone="danger">expired</Badge>}
      </div>
      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader className="flex-row items-center justify-between">
            <CardTitle>Permissions</CardTitle>
            <Button size="sm" variant="ghost" onClick={() => setEditing(true)}>
              Edit
            </Button>
          </CardHeader>
          <CardContent className="space-y-4 text-sm">
            <div className="flex flex-wrap gap-2">
              {a.scopes.length ? a.scopes.map((s) => <Badge key={s}>{scopeLabel(s)}</Badge>) : <span className="text-muted">None</span>}
            </div>
            <BudgetBar budget={a.budget} />
            {a.balance_msat != null && (
              <p>
                Balance <Amount msat={a.balance_msat} />
              </p>
            )}
            <p className="text-muted">
              {a.last_used_at ? `Last used ${ago(a.last_used_at)}` : "Never used"} · created {dateTime(a.created_at)}
              {a.expires_at != null && ` · ${expired ? "expired" : "expires"} ${dateTime(a.expires_at)}`}
            </p>
            <p className="text-xs text-muted">
              Wallet key <span className="font-mono">{shorten(a.wallet_pubkey ?? "", 10, 6)}</span> · app key{" "}
              <span className="font-mono">{shorten(a.app_pubkey, 10, 6)}</span>
            </p>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>Connection link</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            <p className="text-muted">The link was shown once. A new link replaces the old one, and the app using it stops working.</p>
            {uri ? (
              <div className="space-y-3">
                <QrBlock value={uri} />
                <CopyButton value={uri} label="Copy connection link" />
              </div>
            ) : (
              <Button
                variant="secondary"
                onClick={async () => {
                  setErr(null);
                  try {
                    const r = await api.post<{ pairing_uri: string }>(`/api/apps/${a.id}/regenerate`, {});
                    setUri(r.pairing_uri);
                    void mutate();
                  } catch (e) {
                    setErr(e instanceof Error ? e.message : String(e));
                  }
                }}
              >
                Create a new link
              </Button>
            )}
            <Button variant="danger" onClick={() => setRevoking(true)}>
              Revoke connection
            </Button>
            {err && <Alert tone="danger" title="Error">{err}</Alert>}
          </CardContent>
        </Card>
      </div>
      <Card>
        <CardHeader>
          <CardTitle>Recent requests</CardTitle>
        </CardHeader>
        <div className="divide-y divide-line border-t border-line">
          {a.requests?.length ? (
            a.requests.map((r) => (
              <div key={r.id} className="flex items-center justify-between px-5 py-2 text-sm">
                <span className="font-mono text-xs">{r.method ?? "—"}</span>
                <span className="flex items-center gap-3">
                  <Badge tone={r.state === "executed" ? "ok" : r.state === "error" ? "danger" : "neutral"}>{r.state}</Badge>
                  <span className="text-xs text-muted">{ago(r.created_at)}</span>
                </span>
              </div>
            ))
          ) : (
            <div className="p-4">
              <Empty>No requests yet.</Empty>
            </div>
          )}
        </div>
      </Card>
      {editing && <EditConnection app={a} onClose={() => setEditing(false)} onDone={() => void mutate()} />}
      <Modal open={revoking} onClose={() => setRevoking(false)} title="Revoke this connection?">
        <div className="space-y-4">
          <p className="text-sm text-muted">The app loses access immediately. Its payment history stays in your wallet.</p>
          {err && <Alert tone="danger" title="Could not revoke">{err}</Alert>}
          <Button
            variant="danger"
            className="w-full"
            onClick={async () => {
              try {
                await api.del(`/api/apps/${a.id}`);
                nav("/connections");
              } catch (e) {
                setErr(e instanceof Error ? e.message : String(e));
              }
            }}
          >
            Revoke
          </Button>
        </div>
      </Modal>
    </div>
  );
}

function EditConnection({ app, onClose, onDone }: { app: AppConn; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(app.name);
  const [form, setForm] = useState<ScopeFormValue>({
    scopes: app.scopes,
    budgetSats: app.max_amount_msat ? String(app.max_amount_msat / 1000) : "",
    renewal: app.budget_renewal,
    isolated: app.isolated,
  });
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title="Edit connection">
      <div className="space-y-4">
        <Field label="Name">
          <Input value={name} onChange={(e) => setName(e.target.value)} />
        </Field>
        <ScopeForm value={form} onChange={setForm} lockIsolated />
        {error && <Alert tone="danger" title="Could not save">{error}</Alert>}
        <Button
          className="w-full"
          onClick={async () => {
            try {
              const budget = parseSats(form.budgetSats);
              await api.patch(`/api/apps/${app.id}`, {
                name,
                scopes: form.scopes,
                max_amount_msat: budget ? budget * 1000 : 0,
                budget_renewal: form.renewal,
              });
              onDone();
              onClose();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          Save
        </Button>
      </div>
    </Modal>
  );
}
