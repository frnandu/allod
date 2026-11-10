import { useState } from "react";
import useSWR, { mutate } from "swr";
import { Alert } from "../../components/ui/alert";
import { NostrIcon } from "../../components/NostrIcon";
import { Button } from "../../components/ui/button";
import { Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { DOMAINS_KEY, PUBLIC_NAME_KEY, domains as domainApi, fetcher, nip05, publicName, type Account, type DomainAddress, type DomainEntry, type Domains, type Nip05Name, type PublicName } from "../../lib/api";
import { suggestIdentityUsername } from "../../lib/identityUsername";
import { NameDialog } from "../settings/Nip05";

type Address =
  | { kind: "own"; address: string; own: DomainAddress; domain: DomainEntry }
  | { kind: "zeus"; address: string }
  | { kind: "alby"; address: string; connectionId: number }
  | { kind: "twelve" | "clank"; address: string };

function errorText(error: unknown) { return error instanceof Error ? error.message : String(error); }

export function AccountIdentities({ account }: { account: Account }) {
  const names = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher);
  const domains = useSWR<Domains>(DOMAINS_KEY, fetcher);
  const publicNames = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher);
  const [editingName, setEditingName] = useState(false);
  const [managingName, setManagingName] = useState(false);
  const [removingName, setRemovingName] = useState(false);
  const [addingAddress, setAddingAddress] = useState(false);
  const [managingAddress, setManagingAddress] = useState<Address | null>(null);
  const [editingAddress, setEditingAddress] = useState<Address | null>(null);
  const [removingAddress, setRemovingAddress] = useState<Address | null>(null);
  const target = String(account.id);
  const name = names.data?.names.find((n) => n.account_id === account.id);
  const addresses: Address[] = [
    ...(domains.data?.domains.flatMap((d) => d.addresses.filter((a) => a.target === target).map((a): Address => ({ kind: "own", address: a.address, own: a, domain: d }))) ?? []),
    ...(publicNames.data?.zeus?.target === target ? [{ kind: "zeus" as const, address: publicNames.data.zeus.address }] : []),
    ...(publicNames.data?.alby.filter((a) => a.target === target).map((a): Address => ({ kind: "alby", address: a.address ?? "Alby address (setup incomplete)", connectionId: a.connection_id })) ?? []),
    ...(publicNames.data?.twelve_cash.filter((a) => a.target === target).map((a): Address => ({ kind: "twelve", address: a.address })) ?? []),
    ...(publicNames.data?.clank.filter((a) => a.target === target).map((a): Address => ({ kind: "clank", address: a.address })) ?? []),
  ];
  const refresh = () => {
    void mutate("/api/nip05");
    void mutate(DOMAINS_KEY);
    void mutate(PUBLIC_NAME_KEY);
  };
  const served = domains.data?.domains.filter((d) => d.served) ?? [];

  return <>
    <div className="ml-auto flex flex-wrap items-center justify-end gap-2 text-xs">
      {name ? <button type="button" className="inline-flex items-center gap-1.5 rounded-full border border-line bg-surface px-3 py-1 font-mono hover:border-accent" onClick={() => setManagingName(true)} aria-label={`Manage NIP-05 ${name.username}@${name.domain}`}><NostrIcon size={30} />{name.username}@{name.domain}</button>
        : account.closed_at == null && <button type="button" className="inline-flex items-center gap-1 rounded-full border border-dashed border-line px-3 py-1 hover:border-accent" onClick={() => setEditingName(true)}><span>+</span><NostrIcon size={30} />Nostr name (NIP-05)</button>}
      {addresses.map((address) => <button key={`${address.kind}-${address.address}`} type="button" className="rounded-full border border-line bg-surface px-3 py-1.5 font-mono hover:border-accent" onClick={() => setManagingAddress(address)} aria-label={`Manage Lightning address ${address.address}`}>⚡ {address.address}</button>)}
      {account.closed_at == null && addresses.length === 0 && <button type="button" className="inline-flex items-center gap-1 rounded-full border border-dashed border-line px-3 py-1.5 hover:border-accent" onClick={() => setAddingAddress(true)}>+ <span aria-hidden="true">⚡</span> Lightning address</button>}
    </div>
    {managingName && name && <ManageIdentity title={`${name.username}@${name.domain}`} onClose={() => setManagingName(false)} onEdit={() => { setManagingName(false); setEditingName(true); }} onRemove={() => { setManagingName(false); setRemovingName(true); }} />}
    {managingAddress && <ManageIdentity title={managingAddress.address} onClose={() => setManagingAddress(null)} onEdit={managingAddress.kind === "twelve" || managingAddress.kind === "clank" ? undefined : () => { setEditingAddress(managingAddress); setManagingAddress(null); }} onRemove={() => { setRemovingAddress(managingAddress); setManagingAddress(null); }} />}
    {editingName && <NameDialog entry={name} names={names.data?.names ?? []} domains={served} accounts={[account]} fixedAccountId={account.id} onClose={() => setEditingName(false)} onDone={() => { refresh(); setEditingName(false); }} />}
    {removingName && name && <IdentityRemoval title={`Remove ${name.username}@${name.domain}?`} disassociate="Keep Nostr name (NIP-05), unlink account" remove="Delete Nostr name (NIP-05) entirely" onClose={() => setRemovingName(false)} onAction={async (all) => {
      if (all) await nip05.remove(name.id); else await nip05.setAccount(name.id, null);
      refresh(); setRemovingName(false);
    }} />}
    {editingAddress && <AddressEditor address={editingAddress} domains={domains.data?.domains ?? []} onClose={() => setEditingAddress(null)} onDone={() => { refresh(); setEditingAddress(null); }} />}
    {addingAddress && <AddAddress account={account} domains={domains.data?.domains ?? []} onClose={() => setAddingAddress(false)} onDone={() => { refresh(); setAddingAddress(false); }} />}
    {removingAddress && <IdentityRemoval title={`Remove ${removingAddress.address}?`}
      disassociate={removingAddress.kind === "own" || removingAddress.kind === "zeus" ? "Keep address, move payments to Main" : undefined}
      remove={removingAddress.kind === "twelve" || removingAddress.kind === "clank" ? "Forget address in Allod" : "Delete address entirely"}
      note={removingAddress.kind === "twelve" || removingAddress.kind === "clank" ? "Provider name stays registered and may still point to this account's offer. Moving it to Main is unsupported." : removingAddress.kind === "alby" ? "Alby address cannot move between accounts. Remove its connection and set up another address for Main if needed." : removingAddress.kind === "own" && removingAddress.domain.dns_provider_id == null ? "If you delete it, remove its TXT record at your DNS host too." : undefined}
      onClose={() => setRemovingAddress(null)} onAction={async (all) => {
        const address = removingAddress;
        if (address.kind === "own") {
          if (all) await domainApi.removeAddress(address.own.id);
          else await domainApi.updateAddress(address.own.id, { target: "main" });
        } else if (address.kind === "zeus") {
          if (all) await publicName.zeusRemove(); else await publicName.zeusTarget("main");
        } else if (address.kind === "alby") await publicName.albyRemove(address.connectionId);
        else if (address.kind === "twelve") await publicName.twelveForget(target);
        else await publicName.clankForget(target);
        refresh(); setRemovingAddress(null);
      }} />}
  </>;
}

function ManageIdentity({ title, onClose, onEdit, onRemove }: { title: string; onClose: () => void; onEdit?: () => void; onRemove: () => void }) {
  return <Modal open onClose={onClose} title={title}><div className="flex flex-wrap justify-end gap-2">
    <Button variant="ghost" onClick={onClose}>Cancel</Button>
    {onEdit && <Button variant="secondary" onClick={onEdit}>Edit</Button>}
    <Button variant="danger" onClick={onRemove}>Remove or unlink</Button>
  </div></Modal>;
}

function AddAddress({ account, domains, onClose, onDone }: { account: Account; domains: DomainEntry[]; onClose: () => void; onDone: () => void }) {
  const [domainId, setDomainId] = useState(domains[0]?.id ?? 0);
  const [username, setUsername] = useState("");
  const [usernameEdited, setUsernameEdited] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const domain = domains.find((d) => d.id === domainId);
  const usernameValue = usernameEdited ? username : suggestIdentityUsername(account.name, domain?.addresses.map((a) => a.username) ?? []);
  return <Modal open onClose={onClose} title="Add Lightning address"><div className="space-y-4">
    {domains.length ? <>
      <Field label="Your domain"><select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={domainId} onChange={(e) => setDomainId(Number(e.target.value))}>{domains.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}</select></Field>
      <Field label="Username"><Input value={usernameValue} onChange={(e) => { setUsername(e.target.value); setUsernameEdited(true); }} maxLength={64} placeholder="alice" /></Field>
      <p className="text-xs text-muted">Address will pay into {account.name}. LNURL works when domain is served; BIP-353 uses DNS.</p>
    </> : <p className="text-sm text-muted">Add own domain in Settings → Domains first.</p>}
    {error && <Alert tone="danger">{error}</Alert>}
    <div className="flex justify-end gap-2"><Button variant="ghost" onClick={onClose}>Cancel</Button><Button disabled={!domain || !usernameValue.trim() || busy} onClick={async () => {
      setBusy(true); setError("");
      try { await domainApi.addAddress(domainId, usernameValue.trim(), String(account.id), { lnurl: !!domain?.served, bip353: true }); onDone(); }
      catch (e) { setError(errorText(e)); }
      finally { setBusy(false); }
    }}>{busy ? "Adding…" : "Add address"}</Button></div>
  </div></Modal>;
}

function IdentityRemoval({ title, disassociate, remove, note, onClose, onAction }: {
  title: string; disassociate?: string; remove: string; note?: string; onClose: () => void; onAction: (all: boolean) => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function run(all: boolean) {
    setBusy(true); setError("");
    try { await onAction(all); } catch (e) { setError(errorText(e)); setBusy(false); }
  }
  return <Modal open onClose={onClose} title={title}><div className="space-y-4 text-sm">
    {note && <p className="text-muted">{note}</p>}
    {error && <Alert tone="danger">{error}</Alert>}
    <div className="flex flex-wrap justify-end gap-2">
      <Button variant="ghost" onClick={onClose} disabled={busy}>Cancel</Button>
      {disassociate && <Button variant="secondary" onClick={() => void run(false)} disabled={busy}>{disassociate}</Button>}
      <Button variant="danger" onClick={() => void run(true)} disabled={busy}>{remove}</Button>
    </div>
  </div></Modal>;
}

function AddressEditor({ address, domains, onClose, onDone }: { address: Address; domains: DomainEntry[]; onClose: () => void; onDone: () => void }) {
  const own = address.kind === "own" ? address.own : null;
  const [domainId, setDomainId] = useState(own?.domain_id ?? 0);
  const [username, setUsername] = useState(own?.username ?? "");
  const [lnurl, setLnurl] = useState(own?.lnurl_enabled ?? true);
  const [bip353, setBip353] = useState(own?.bip353 ?? true);
  const [zeusHandle, setZeusHandle] = useState(address.kind === "zeus" ? (address.address.split("@")[0] ?? "") : "");
  const [zeusDomain, setZeusDomain] = useState(address.kind === "zeus" ? (address.address.split("@")[1] ?? "zeuspay.com") : "zeuspay.com");
  const [albyAddress, setAlbyAddress] = useState(address.kind === "alby" ? address.address : "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  async function save() {
    setBusy(true); setError("");
    try {
      if (address.kind === "own") await domainApi.updateAddress(address.own.id, { domain_id: domainId, username: username.trim(), lnurl, bip353 });
      else if (address.kind === "zeus") await publicName.zeusRename(zeusHandle.trim(), zeusDomain);
      else if (address.kind === "alby") await publicName.albyLink(address.connectionId, albyAddress.trim());
      onDone();
    } catch (e) { setError(errorText(e)); } finally { setBusy(false); }
  }
  return <Modal open onClose={onClose} title={`Edit ${address.address}`}><div className="space-y-4">
    {address.kind === "own" && <>
      <Field label="Username"><Input value={username} onChange={(e) => setUsername(e.target.value)} maxLength={64} /></Field>
      <Field label="Your domain"><select className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm" value={domainId} onChange={(e) => setDomainId(Number(e.target.value))}>{domains.map((d) => <option key={d.id} value={d.id}>{d.name}</option>)}</select></Field>
      <label className="flex gap-2 text-sm"><input type="checkbox" checked={lnurl} onChange={(e) => setLnurl(e.target.checked)} /> LNURL on served domain</label>
      <label className="flex gap-2 text-sm"><input type="checkbox" checked={bip353} onChange={(e) => setBip353(e.target.checked)} /> BIP-353 DNS record</label>
      <p className="text-xs text-muted">Changing name or domain replaces its offer and DNS record.</p>
    </>}
    {address.kind === "zeus" && <><Field label="Handle"><Input value={zeusHandle} onChange={(e) => setZeusHandle(e.target.value)} /></Field><Field label="Domain"><Input value={zeusDomain} onChange={(e) => setZeusDomain(e.target.value)} /></Field></>}
    {address.kind === "alby" && <Field label="Alby address"><Input value={albyAddress} onChange={(e) => setAlbyAddress(e.target.value)} /></Field>}
    {error && <Alert tone="danger">{error}</Alert>}
    <div className="flex justify-end gap-2"><Button variant="ghost" onClick={onClose}>Cancel</Button><Button onClick={() => void save()} disabled={busy || (address.kind === "own" && (!username.trim() || (!lnurl && !bip353)))}>{busy ? "Saving…" : "Save changes"}</Button></div>
  </div></Modal>;
}
