import { CaretRight, Plus } from "@phosphor-icons/react";
import { Link } from "react-router";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { AccountAvatar } from "../../components/AccountAvatar";
import { NostrIcon } from "../../components/NostrIcon";
import { Badge } from "../../components/ui/badge";
import { Card } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { useAccounts } from "../../lib/account";
import { DOMAINS_KEY, PUBLIC_NAME_KEY, fetcher, type Balances, type Domains, type Nip05Name, type PublicName } from "../../lib/api";
import { ago } from "../../lib/format";
import { CoverageWarning } from "./shared";

function lightningAddresses(target: string, domains?: Domains, publicName?: PublicName): string[] {
  const own = domains?.domains.flatMap((d) => d.addresses.filter((a) => a.target === target).map((a) => a.address)) ?? [];
  const external = [
    ...(publicName?.alby.filter((a) => a.target === target && a.address).map((a) => a.address!) ?? []),
    ...(publicName?.zeus?.target === target ? [publicName.zeus.address] : []),
    ...(publicName?.clank.filter((a) => a.target === target && a.active).map((a) => a.address) ?? []),
    ...(publicName?.twelve_cash.filter((a) => a.target === target && a.active).map((a) => a.address) ?? []),
  ];
  return [...new Set([...own, ...external])];
}

function Identities({ nip, addresses, showAllAddresses = false }: { nip?: Nip05Name; addresses: string[]; showAllAddresses?: boolean }) {
  if (!nip && !addresses.length) return null;
  const nipAddress = nip ? `${nip.username}@${nip.domain}` : null;
  const shared = nipAddress && addresses.find((address) => address.toLowerCase() === nipAddress.toLowerCase());
  const remaining = shared ? addresses.filter((address) => address.toLowerCase() !== shared.toLowerCase()) : addresses;
  return <div className="mt-1 flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted">
    {nip && <span className="inline-flex min-w-0 items-center gap-1 truncate"><NostrIcon size={28} />{shared && <span aria-hidden="true">⚡</span>}{nipAddress}</span>}
    {(showAllAddresses ? remaining : remaining.slice(0, 1)).map((address) => <span key={address} className="truncate">⚡ {address}{!showAllAddresses && remaining.length > 1 ? ` +${remaining.length - 1}` : ""}</span>)}
  </div>;
}

export function Accounts() {
  const { data, error, ready } = useAccounts();
  const balances = useSWR<Balances>("/api/balances", fetcher, { refreshInterval: 30000 });
  const summary = useSWR<{ count: number; total_balance_msat: number }>("/api/accounts/summary", fetcher);
  const nipNames = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher);
  const domains = useSWR<Domains>(DOMAINS_KEY, fetcher);
  const publicName = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher);
  return (
    <div className="space-y-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="text-3xl font-semibold">Accounts</h1>
          <p className="text-sm text-muted">Separate balances on your node, each with its own wallet connection.</p>
        </div>
        <Link to="/accounts/new" className="inline-flex h-10 items-center gap-2 rounded-full bg-accent px-5 text-sm font-medium text-accent-fg">
          <Plus size={16} weight="bold" /> New account
        </Link>
      </div>
      <CoverageWarning accountsMsat={summary.data?.total_balance_msat} balances={balances.data} />
      {!ready && <p className="text-sm text-muted">Accounts are listed once the Lightning node is ready.</p>}
      {error && <p className="text-sm text-muted">{String(error.message)}</p>}
      <Card className="divide-y divide-line">
        <div className="flex items-center gap-3 p-4">
          <AccountAvatar accountId={null} seed="main" size={36} />
          <div className="min-w-0 flex-1">
            <p className="font-medium">Main</p>
            <p className="text-xs text-muted">Node funds not in an account</p>
            <Identities addresses={lightningAddresses("main", domains.data, publicName.data)} showAllAddresses />
          </div>
          <Amount msat={balances.data?.main_spendable_msat ?? null} className="text-right" />
        </div>
        {data?.items.map((a) => (
          <Link key={a.id} to={`/accounts/${a.id}`} className="flex items-center gap-3 p-4 hover:bg-surface-2">
            <AccountAvatar accountId={a.id} seed={a.app_pubkey} size={36} />
            <div className="min-w-0 flex-1">
              <p className="flex items-center gap-2 font-medium">
                <span className="truncate">{a.name}</span>
                {a.closed_at != null && <Badge tone="warn">closed</Badge>}
              </p>
              <p className="text-xs text-muted">
                {a.closed_at != null ? "Received a payment after closing: withdraw it to Main" : a.last_used_at ? `Used ${ago(a.last_used_at)}` : "Never used"}
              </p>
              <Identities nip={nipNames.data?.names.find((n) => n.account_id === a.id)} addresses={lightningAddresses(String(a.id), domains.data, publicName.data)} />
            </div>
            <Amount msat={a.balance_msat} className="text-right" />
            <CaretRight size={16} className="text-muted" />
          </Link>
        ))}
      </Card>
      {data && !data.items.length && (
        <Empty>No accounts yet. Create one for a family member, a device or an app, and fund it from Main.</Empty>
      )}
      {summary.data && summary.data.count > 0 && (
        <p className="text-sm text-muted">
          {summary.data.count} account{summary.data.count === 1 ? "" : "s"} holding <Amount msat={summary.data.total_balance_msat} className="text-fg" />
        </p>
      )}
    </div>
  );
}
