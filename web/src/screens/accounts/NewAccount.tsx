import { ArrowLeft } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import useSWR, { mutate } from "swr";
import { nip19 } from "nostr-tools";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, domains as domainsApi, fetcher, nip05, type CreatedAccount, type Domains, type Nip05Name } from "../../lib/api";
import { suggestIdentityUsername } from "../../lib/identityUsername";
import { SecretLink, SetupTips } from "./shared";

export function NewAccount() {
  const [name, setName] = useState("");
  const { data: domains } = useSWR<Domains>("/api/domains", fetcher);
  const { data: nostrNames } = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher);
  const served = domains?.domains.filter((d) => d.served) ?? [];
  const availableNames = nostrNames?.names.filter((n) => n.account_id == null) ?? [];
  const [nostrMode, setNostrMode] = useState<"none" | "existing" | "new">("none");
  const [existingId, setExistingId] = useState(0);
  const [nostrDomainId, setNostrDomainId] = useState(0);
  const [nostrUsername, setNostrUsername] = useState("");
  const [nostrUsernameEdited, setNostrUsernameEdited] = useState(false);
  const [nostrNpub, setNostrNpub] = useState("");
  const [addLightning, setAddLightning] = useState(false);
  const [lightningDomainId, setLightningDomainId] = useState(0);
  const [lightningUsername, setLightningUsername] = useState("");
  const [lightningUsernameEdited, setLightningUsernameEdited] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<{ result: CreatedAccount; nostr?: string; lightning?: string; txt?: { name: string; value: string }; warnings: string[] } | null>(null);
  const selectedNostrDomain = served.find((d) => d.id === (nostrDomainId || served[0]?.id));
  const selectedLightningDomain = domains?.domains.find((d) => d.id === (lightningDomainId || domains.domains[0]?.id));
  const nostrUsernameValue = nostrUsernameEdited ? nostrUsername : name.trim() ? suggestIdentityUsername(name, nostrNames?.names.filter((n) => n.domain_id === selectedNostrDomain?.id).map((n) => n.username) ?? []) : "";
  const lightningUsernameValue = lightningUsernameEdited ? lightningUsername : name.trim() ? suggestIdentityUsername(name, selectedLightningDomain?.addresses.map((a) => a.username) ?? []) : "";

  if (created) return <Created {...created} />;

  async function create() {
    setError(null);
    if (nostrMode === "new") {
      try {
        if (nip19.decode(nostrNpub.trim()).type !== "npub") throw new Error("Enter a valid Nostr npub.");
      } catch {
        setError("Enter a valid Nostr npub.");
        return;
      }
    }
    setBusy(true);
    let result: CreatedAccount;
    try {
      result = await api.post<CreatedAccount>("/api/accounts", { name: name.trim() });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBusy(false);
      return;
    }
    void mutate((k) => typeof k === "string" && k.startsWith("/api/accounts"));
    const warnings: string[] = [];
    let nostr: string | undefined;
    let lightning: string | undefined;
    let txt: { name: string; value: string } | undefined;
    try {
      if (nostrMode === "existing") {
        const chosen = availableNames.find((n) => n.id === (existingId || availableNames[0]?.id));
        if (!chosen) throw new Error("Choose an available Nostr name (NIP-05).");
        await nip05.setAccount(chosen.id, result.account.id);
        nostr = `${chosen.username}@${chosen.domain}`;
      } else if (nostrMode === "new") {
        const domain = served.find((d) => d.id === (nostrDomainId || served[0]?.id));
        if (!domain) throw new Error("Choose a served domain for Nostr name (NIP-05).");
        await nip05.add(domain.id, nostrUsernameValue.trim(), nostrNpub.trim(), result.account.id);
        nostr = `${nostrUsernameValue.trim().toLowerCase()}@${domain.name}`;
      }
    } catch (e) {
      warnings.push(`Nostr name (NIP-05): ${e instanceof Error ? e.message : String(e)}`);
    }
    try {
      if (addLightning) {
        const domain = domains?.domains.find((d) => d.id === (lightningDomainId || domains.domains[0]?.id));
        if (!domain) throw new Error("Choose a domain for Lightning address.");
        const updated = await domainsApi.addAddress(domain.id, lightningUsernameValue.trim(), String(result.account.id), { lnurl: domain.served, bip353: true });
        const address = updated.domains.find((d) => d.id === domain.id)?.addresses.find((a) => a.username === lightningUsernameValue.trim().toLowerCase());
        lightning = address?.address ?? `${lightningUsernameValue.trim().toLowerCase()}@${domain.name}`;
        if (address?.txt) txt = { name: address.record_name, value: address.txt };
      }
    } catch (e) {
      warnings.push(`Lightning address: ${e instanceof Error ? e.message : String(e)}`);
    }
    void mutate("/api/nip05");
    void mutate("/api/domains");
    setCreated({ result, nostr, lightning, txt, warnings });
    setBusy(false);
  }
  return (
    <div className="mx-auto max-w-lg space-y-5">
      <Link to="/accounts" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Accounts</Link>
      <h1 className="text-3xl font-semibold">New account</h1>
      <Card>
        <CardContent className="space-y-5 p-6">
          <Field label="Name">
            <Input value={name} maxLength={100} onChange={(e) => setName(e.target.value)} placeholder="e.g. Kids, Phone, Shop" autoFocus />
          </Field>
          <div className="space-y-3 border-t border-line pt-4">
            <h2 className="font-semibold">Nostr name (NIP-05)</h2>
            <select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={nostrMode} onChange={(e) => setNostrMode(e.target.value as typeof nostrMode)} aria-label="Nostr name (NIP-05) choice">
              <option value="none">No Nostr name (NIP-05)</option>
              {availableNames.length > 0 && <option value="existing">Use an existing name</option>}
              {served.length > 0 && <option value="new">Add a new name</option>}
            </select>
            {nostrMode === "existing" && (
              <Field label="Existing Nostr name (NIP-05)">
                <select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={existingId || availableNames[0]?.id} onChange={(e) => setExistingId(Number(e.target.value))}>
                  {availableNames.map((n) => <option key={n.id} value={n.id}>{n.username}@{n.domain}</option>)}
                </select>
              </Field>
            )}
            {nostrMode === "new" && (
              <>
                <Field label="Your domain">
                  <select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={nostrDomainId || served[0]?.id} onChange={(e) => setNostrDomainId(Number(e.target.value))}>
                    {served.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
                  </select>
                </Field>
                <Field label="Username"><Input value={nostrUsernameValue} maxLength={64} onChange={(e) => { setNostrUsername(e.target.value.toLowerCase()); setNostrUsernameEdited(true); }} placeholder="alice" /></Field>
                <Field label="Nostr public key"><Input value={nostrNpub} onChange={(e) => setNostrNpub(e.target.value)} placeholder="npub1…" /></Field>
              </>
            )}
          </div>
          <div className="space-y-3 border-t border-line pt-4">
            <label className="flex items-center gap-2 font-semibold">
              <input type="checkbox" checked={addLightning} onChange={(e) => setAddLightning(e.target.checked)} /> Add Lightning address
            </label>
            {addLightning && (
              domains?.domains.length ? <>
                <Field label="Your domain">
                  <select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={lightningDomainId || domains.domains[0]?.id} onChange={(e) => setLightningDomainId(Number(e.target.value))}>
                    {domains.domains.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
                  </select>
                </Field>
                <Field label="Username" hint="BIP-353 works on any domain; LNURL also works when domain serves HTTPS.">
                  <Input value={lightningUsernameValue} maxLength={64} onChange={(e) => { setLightningUsername(e.target.value.toLowerCase()); setLightningUsernameEdited(true); }} placeholder="alice" />
                </Field>
              </> : <p className="text-sm text-muted">Add <Link to="/settings/domains" className="underline">a domain</Link> first.</p>
            )}
          </div>
          <p className="text-sm text-muted">
            An account can receive, pay and see only its own payments. It starts empty: top it up from Main.
          </p>
          {error && <Alert tone="danger" title="Could not create the account">{error}</Alert>}
          <Button
            size="lg"
            className="w-full"
            disabled={!name.trim() || busy || (nostrMode === "new" && (!nostrUsernameValue.trim() || !nostrNpub.trim())) || (nostrMode === "existing" && !availableNames.length) || (addLightning && (!domains?.domains.length || !lightningUsernameValue.trim()))}
            onClick={() => void create()}
          >
            Create account
          </Button>
        </CardContent>
      </Card>
    </div>
  );
}

function Created({ result: c, nostr, lightning, txt, warnings }: {
  result: CreatedAccount;
  nostr?: string;
  lightning?: string;
  txt?: { name: string; value: string };
  warnings: string[];
}) {
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
      {warnings.length > 0 && <Alert tone="warn" title="Account created; finish identity setup">
        {warnings.map((warning) => <p key={warning}>{warning}</p>)}
        <p>Finish in <Link to="/settings/nip05" className="underline">Nostr names (NIP-05)</Link> or <Link to="/settings/lightning-addresses" className="underline">Lightning addresses</Link>.</p>
      </Alert>}
      {(nostr || lightning) && <Card><CardContent className="space-y-2 pt-6 text-sm">
        {nostr && <p>Nostr name (NIP-05): <span className="font-mono">{nostr}</span></p>}
        {lightning && <p>Lightning address: <span className="font-mono">{lightning}</span></p>}
      </CardContent></Card>}
      {txt && <Card><CardHeader><CardTitle>BIP-353 name</CardTitle></CardHeader><CardContent className="space-y-3 text-sm">
        <p className="text-muted">Add this DNS TXT record (with DNSSEC) to let people pay the account by name.</p>
        <p className="break-all rounded-lg bg-surface-2 p-2 font-mono text-xs">{txt.name} IN TXT "{txt.value}"</p>
        <CopyButton value={`${txt.name} IN TXT "${txt.value}"`} label="Copy record" />
      </CardContent></Card>}
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
