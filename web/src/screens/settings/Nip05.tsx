import { Plus } from "@phosphor-icons/react";
import { nip19 } from "nostr-tools";
import { useState, type FormEvent } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { Avatar, npub, useProfile } from "../../components/Person";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { useAccounts } from "../../lib/account";
import { ApiError, fetcher, nip05, type Account, type Domains, type Nip05Name } from "../../lib/api";
import { suggestIdentityUsername } from "../../lib/identityUsername";
import { useSession } from "../../lib/session";

function profileUsername(name?: string): string {
  return (name ?? "")
    .normalize("NFKD")
    .replace(/[\u0300-\u036f]/g, "")
    .toLowerCase()
    .trim()
    .replace(/\s+/g, "-")
    .replace(/[^a-z0-9._-]/g, "")
    .slice(0, 64);
}

function Person({ entry, account, onEdit, onRemove }: {
  entry: Nip05Name;
  account?: Account;
  onEdit: () => void;
  onRemove: () => void;
}) {
  const profile = useProfile(entry.pubkey);
  return (
    <div className="flex items-center gap-3 py-3">
      <Avatar pubkey={entry.pubkey} picture={profile?.picture} size={40} />
      <div className="min-w-0 flex-1">
        <div className="font-medium">{entry.username}@{entry.domain}</div>
        <div className="truncate text-sm text-muted">{profile?.name ? `${profile.name} · ` : ""}{entry.npub}</div>
        {entry.account_id != null && <div className="text-xs text-muted">Account: {account?.name ?? `#${entry.account_id}`}</div>}
      </div>
      <Button variant="secondary" size="sm" onClick={onEdit}>Edit</Button>
      <Button variant="ghost" size="sm" onClick={onRemove} aria-label={`Remove ${entry.username}@${entry.domain}`}>Remove</Button>
    </div>
  );
}

export function Nip05Settings() {
  const { data, error, mutate } = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher);
  const { data: domains } = useSWR<Domains>("/api/domains", fetcher);
  const { data: accounts } = useAccounts();
  const [adding, setAdding] = useState(false);
  const [editing, setEditing] = useState<Nip05Name | null>(null);
  const [message, setMessage] = useState("");
  const served = domains?.domains.filter((d) => d.served) ?? [];

  async function remove(id: number) {
    setMessage("");
    try {
      await nip05.remove(id);
      await mutate();
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err));
    }
  }

  return (
    <section className="space-y-3" aria-label="Nostr names (NIP-05)">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h2 className="text-lg font-semibold">Nostr names (NIP-05)</h2>
          <p className="text-sm text-muted">Names on your own domains, mapped to Nostr public keys (NIP-05).</p>
        </div>
        <Button size="sm" onClick={() => setAdding(true)} disabled={!served.length} aria-label="Add a Nostr name (NIP-05)">
          <Plus size={16} weight="bold" /> Add
        </Button>
      </div>
      {message && <Alert tone="danger">{message}</Alert>}
      {error && <Alert tone="danger">{error instanceof ApiError && error.status === 404
        ? "NIP-05 API returned 404. Restart Allod backend to load this feature."
        : `Could not load Nostr names (NIP-05): ${error instanceof Error ? error.message : String(error)}`}</Alert>}
      <Card><CardContent className="divide-y divide-line">
        {data?.names.length ? data.names.map((entry) => (
          <Person
            key={entry.id}
            entry={entry}
            account={accounts?.items.find((a) => a.id === entry.account_id)}
            onEdit={() => setEditing(entry)}
            onRemove={() => void remove(entry.id)}
          />
        )) : <p className="py-4 text-sm text-muted">No Nostr names (NIP-05) yet.</p>}
      </CardContent></Card>
      {!served.length && <p className="text-sm text-muted">Add and serve <Link to="/settings/domains" className="underline">your domain</Link> over HTTPS to use NIP-05.</p>}
      <p className="text-xs text-muted">Set matching NIP-05 address in each person&apos;s Nostr profile so clients can display it.</p>
      {adding && <NameDialog names={data?.names ?? []} domains={served} accounts={accounts?.items ?? []} onClose={() => setAdding(false)} onDone={() => { void mutate(); setAdding(false); }} />}
      {editing && <NameDialog entry={editing} names={data?.names ?? []} domains={served} accounts={accounts?.items ?? []} onClose={() => setEditing(null)} onDone={() => { void mutate(); setEditing(null); }} />}
    </section>
  );
}

export function NameDialog({ entry, names, domains, accounts, fixedAccountId, onClose, onDone }: {
  entry?: Nip05Name;
  names: Nip05Name[];
  domains: Domains["domains"];
  accounts: Account[];
  fixedAccountId?: number;
  onClose: () => void;
  onDone: () => void;
}) {
  const session = useSession();
  const ownerProfile = useProfile(session?.pubkey);
  const ownerHasName = !!session && names.some((name) => name.pubkey === session.pubkey.toLowerCase());
  const suggestOwner = !entry && !!session && !ownerHasName;
  const [domainId, setDomainId] = useState(entry?.domain_id ?? domains[0]?.id ?? 0);
  const [username, setUsername] = useState(entry?.username ?? "");
  const [usernameEdited, setUsernameEdited] = useState(!!entry);
  const [pubkey, setPubkey] = useState(entry?.npub ?? "");
  const [pubkeyEdited, setPubkeyEdited] = useState(!!entry);
  const [accountId, setAccountId] = useState(entry?.account_id == null ? "" : String(entry.account_id));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const chosenAccount = accounts.find((a) => a.id === (fixedAccountId ?? Number(accountId)));
  const suggestedBase = chosenAccount?.name ?? (suggestOwner ? profileUsername(ownerProfile?.username || ownerProfile?.name) : "");
  const usernameValue = usernameEdited ? username : suggestedBase
    ? suggestIdentityUsername(suggestedBase, names.filter((n) => n.domain_id === domainId && n.id !== entry?.id).map((n) => n.username))
    : "";
  const pubkeyValue = pubkeyEdited ? pubkey : suggestOwner ? npub(session.pubkey) : "";
  const available = accounts.filter((a) => a.closed_at == null && !names.some((n) => n.account_id === a.id && n.id !== entry?.id));
  const currentAccount = entry?.account_id != null && !available.some((a) => a.id === entry.account_id)
    ? accounts.find((a) => a.id === entry.account_id)
    : undefined;

  async function save(e: FormEvent) {
    e.preventDefault();
    setError("");
    try {
      if (nip19.decode(pubkeyValue.trim()).type !== "npub") throw new Error("Enter a valid npub.");
      setBusy(true);
      if (entry) {
        await nip05.update(entry.id, {
          domain_id: domainId,
          username: usernameValue.trim(),
          npub: pubkeyValue.trim(),
          account_id: fixedAccountId ?? (accountId ? Number(accountId) : null),
        });
      } else {
        await nip05.add(domainId, usernameValue.trim(), pubkeyValue.trim(), fixedAccountId ?? (accountId ? Number(accountId) : undefined));
      }
      onDone();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open onClose={onClose} title={entry ? `Edit ${entry.username}@${entry.domain}` : "Add a Nostr name (NIP-05)"}>
      <form onSubmit={save} className="space-y-4">
        <div>
          <label htmlFor="nip05-username" className="text-sm font-medium">Username</label>
          <Input id="nip05-username" value={usernameValue} onChange={(e) => { setUsernameEdited(true); setUsername(e.target.value); }} placeholder="alice" required maxLength={64} />
          <p className="text-xs text-muted">Use _ for domain itself.</p>
        </div>
        <div>
          <label htmlFor="nip05-domain" className="text-sm font-medium">Your domain</label>
          <select id="nip05-domain" className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={domainId} onChange={(e) => setDomainId(Number(e.target.value))} required>
            {domains.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}
          </select>
        </div>
        <div>
          <label htmlFor="nip05-npub" className="text-sm font-medium">Nostr public key</label>
          <Input id="nip05-npub" value={pubkeyValue} onChange={(e) => { setPubkeyEdited(true); setPubkey(e.target.value); }} placeholder="npub1…" required />
        </div>
        {fixedAccountId == null && <div>
          <label htmlFor="nip05-account" className="text-sm font-medium">Account</label>
          <select id="nip05-account" className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={accountId} onChange={(e) => setAccountId(e.target.value)}>
            <option value="">No account</option>
            {available.map((a) => <option key={a.id} value={a.id}>{a.name}</option>)}
            {currentAccount && <option value={currentAccount.id}>{currentAccount.name} (closed)</option>}
          </select>
        </div>}
        {error && <Alert tone="danger">{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="secondary" onClick={onClose}>Cancel</Button>
          <Button type="submit" disabled={busy || !domains.length}>{busy ? "Saving…" : entry ? "Save changes" : "Add name"}</Button>
        </div>
      </form>
    </Modal>
  );
}
