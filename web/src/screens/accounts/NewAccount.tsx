import { ArrowLeft } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import { mutate } from "swr";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, type CreatedAccount } from "../../lib/api";
import { SecretLink, SetupTips } from "./shared";

export function NewAccount() {
  const [name, setName] = useState("");
  const [username, setUsername] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<CreatedAccount | null>(null);

  if (created) return <Created c={created} />;
  return (
    <div className="mx-auto max-w-lg space-y-5">
      <Link to="/accounts" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Accounts</Link>
      <h1 className="text-3xl font-semibold">New account</h1>
      <Card>
        <CardContent className="space-y-5 p-6">
          <Field label="Name">
            <Input value={name} maxLength={100} onChange={(e) => setName(e.target.value)} placeholder="e.g. Kids, Phone, Shop" autoFocus />
          </Field>
          <Field label="Username (optional)" hint="Reserves name@your-domain for a Lightning address and a BIP-353 name. Letters, digits, '.', '_' and '-'.">
            <Input value={username} maxLength={64} onChange={(e) => setUsername(e.target.value.toLowerCase())} spellCheck={false} />
          </Field>
          <p className="text-sm text-muted">
            An account can receive, pay and see only its own payments. It starts empty: top it up from Main.
          </p>
          {error && <Alert tone="danger" title="Could not create the account">{error}</Alert>}
          <Button
            size="lg"
            className="w-full"
            disabled={!name.trim() || busy}
            onClick={async () => {
              setError(null);
              setBusy(true);
              try {
                setCreated(await api.post<CreatedAccount>("/api/accounts", { name: name.trim(), lud16_username: username.trim() || undefined }));
                void mutate((k) => typeof k === "string" && k.startsWith("/api/accounts"));
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              } finally {
                setBusy(false);
              }
            }}
          >
            Create account
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}

function Created({ c }: { c: CreatedAccount }) {
  return (
    <div className="mx-auto max-w-lg space-y-5">
      <h1 className="text-3xl font-semibold">{c.account.name} is ready</h1>
      <Alert tone="warn" title="Shown once">
        Copy the connection link now. Allod does not keep it; a lost link can only be replaced by a new one.
      </Alert>
      <Card>
        <CardHeader>
          <CardTitle>Connection link</CardTitle>
        </CardHeader>
        <CardContent className="space-y-4">
          <SecretLink value={c.pairing_uri} />
          <SetupTips />
        </CardContent>
      </Card>
      {c.bip353 && (
        <Card>
          <CardHeader>
            <CardTitle>BIP-353 name</CardTitle>
          </CardHeader>
          <CardContent className="space-y-3 text-sm">
            <p className="text-muted">Add this DNS TXT record (with DNSSEC) to let people pay the account by name. It points at a reusable offer owned by the account.</p>
            <p className="break-all rounded-lg bg-surface-2 p-2 font-mono text-xs">
              {c.bip353.name} IN TXT "{c.bip353.txt}"
            </p>
            <CopyButton value={`${c.bip353.name} IN TXT "${c.bip353.txt}"`} label="Copy record" />
          </CardContent>
        </Card>
      )}
      {c.account.lud16_username && !c.bip353 && (
        <p className="text-sm text-muted">Username <span className="font-mono">{c.account.lud16_username}</span> is reserved for this account.</p>
      )}
      <div className="flex gap-3">
        <Link to={`/accounts/${c.account.id}`} className="inline-flex h-10 items-center rounded-full bg-accent px-5 text-sm font-medium text-accent-fg">
          Open account
        </Link>
        <Link to="/accounts" className="inline-flex h-10 items-center rounded-full border border-line px-5 text-sm">
          All accounts
        </Link>
      </div>
    </div>
  );
}
