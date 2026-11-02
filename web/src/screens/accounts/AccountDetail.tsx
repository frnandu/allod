import { ArrowDownLeft, ArrowLeft, ArrowUpRight } from "@phosphor-icons/react";
import { useState } from "react";
import { Link, useNavigate, useParams } from "react-router";
import useSWR, { mutate } from "swr";
import { Amount } from "../../components/Amount";
import { Sigil } from "../../components/Sigil";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { Empty, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, closeAccount, fetcher, type Account, type Balances, type Statement } from "../../lib/api";
import { ago, dateTime, parseSats, sats } from "../../lib/format";
import { restoreSigner } from "../../lib/signer";
import { cn } from "../../lib/utils";
import { SecretLink, SetupTips } from "./shared";

const PAGE = 25;

/** Revalidates everything that shows account or Main balances. */
function refreshBalances() {
  void mutate((k) => typeof k === "string" && (k.startsWith("/api/accounts") || k.startsWith("/api/balances") || k.startsWith("/api/transactions")));
}

function errText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function AccountDetail() {
  const { id } = useParams();
  const { data: a, error, mutate: reload } = useSWR<Account>(`/api/accounts/${id}`, fetcher, { refreshInterval: 15000 });
  const [move, setMove] = useState<"topup" | "withdraw" | null>(null);
  const [renaming, setRenaming] = useState(false);
  const [regenerating, setRegenerating] = useState(false);
  const [closing, setClosing] = useState(false);
  const [uri, setUri] = useState<string | null>(null);

  if (error) return <p className="text-sm text-muted">{String(error.message)}</p>;
  if (!a) return <p className="text-sm text-muted">Loading…</p>;
  const closed = a.closed_at != null;

  return (
    <div className="space-y-6">
      <Link to="/accounts" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Accounts</Link>
      <section className="v-hero rounded-[var(--radius-card)] border border-line p-6 sm:p-8">
        <div className="flex flex-wrap items-center gap-3">
          <Sigil seed={a.app_pubkey} size={36} />
          <h1 className="text-2xl font-semibold">{a.name}</h1>
          {closed && <Badge tone="warn">closed {dateTime(a.closed_at)}</Badge>}
        </div>
        <div className="mt-4">
          <Amount msat={a.balance_msat} big />
        </div>
        <p className="mt-2 text-sm text-muted">
          {a.last_used_at ? `Last used ${ago(a.last_used_at)}` : "Never used"} · created {dateTime(a.created_at)}
          {a.lud16_username && <> · username <span className="font-mono">{a.lud16_username}</span></>}
        </p>
        <div className="mt-6 flex flex-wrap gap-3">
          {!closed && (
            <Button onClick={() => setMove("topup")}>
              <ArrowDownLeft size={18} weight="bold" /> Top up from Main
            </Button>
          )}
          <Button variant="secondary" disabled={a.balance_msat <= 0} onClick={() => setMove("withdraw")}>
            <ArrowUpRight size={18} weight="bold" /> Withdraw to Main
          </Button>
        </div>
      </section>

      {closed && a.balance_msat > 0 && (
        <Alert tone="warn" title="A payment arrived after closing">
          Someone paid an invoice or offer this account had shared. Withdraw it to Main.
        </Alert>
      )}

      {!closed && (
        <div className="grid gap-4 lg:grid-cols-2">
          <Card>
            <CardHeader>
              <CardTitle>Connection</CardTitle>
            </CardHeader>
            <CardContent className="space-y-3 text-sm">
              {uri ? (
                <>
                  <SecretLink value={uri} />
                  <SetupTips />
                </>
              ) : (
                <>
                  <p className="text-muted">
                    The connection link was shown when the account was created. A new link replaces it: the wallet using the old
                    one stops working, and the balance stays.
                  </p>
                  <Button variant="secondary" onClick={() => setRegenerating(true)}>
                    Create a new link
                  </Button>
                </>
              )}
              <p className="text-xs text-muted">
                Wallet key <span className="font-mono">{a.wallet_pubkey?.slice(0, 12)}…</span>
              </p>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>Manage</CardTitle>
            </CardHeader>
            <CardContent className="flex flex-wrap gap-3">
              <Button variant="secondary" onClick={() => setRenaming(true)}>
                Rename
              </Button>
              <Button variant="danger" onClick={() => setClosing(true)}>
                Close account
              </Button>
            </CardContent>
          </Card>
        </div>
      )}

      <StatementCard id={a.id} />

      {move && <MoveFunds account={a} kind={move} onClose={() => setMove(null)} />}
      {renaming && <Rename account={a} onClose={() => setRenaming(false)} onDone={() => void reload()} />}
      {regenerating && (
        <TypedConfirm
          title="Replace the connection link?"
          name={a.name}
          action="Create a new link"
          onClose={() => setRegenerating(false)}
          body="The wallet app using the current link stops working at once. The balance and history stay with the account."
          run={async () => {
            const r = await api.post<{ pairing_uri: string }>(`/api/apps/${a.id}/regenerate`, {});
            setUri(r.pairing_uri);
            void reload();
          }}
        />
      )}
      {closing && <CloseAccount account={a} onClose={() => setClosing(false)} />}
    </div>
  );
}

function StatementCard({ id }: { id: number }) {
  const [page, setPage] = useState(0);
  const { data } = useSWR<Statement>(`/api/accounts/${id}/statement?limit=${PAGE}&offset=${page * PAGE}`, fetcher);
  const pages = data ? Math.ceil(data.total / PAGE) : 0;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Statement</CardTitle>
      </CardHeader>
      {data?.items.length ? (
        <div className="overflow-x-auto border-t border-line">
          <table className="w-full text-sm">
            <thead className="text-left text-xs text-muted">
              <tr>
                <th className="px-5 py-2 font-medium">Date</th>
                <th className="px-2 py-2 font-medium">Description</th>
                <th className="px-2 py-2 text-right font-medium">Amount</th>
                <th className="px-5 py-2 text-right font-medium">Balance</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-line">
              {data.items.map((l) => (
                <tr key={l.posting_id}>
                  <td className="whitespace-nowrap px-5 py-2 text-xs text-muted">{dateTime(l.created_at)}</td>
                  <td className="px-2 py-2">
                    <span className="block max-w-xs truncate">{l.description || entryLabel(l.entry_kind)}</span>
                    {l.description && <span className="block text-xs text-muted">{entryLabel(l.entry_kind)}</span>}
                  </td>
                  <td className={cn("whitespace-nowrap px-2 py-2 text-right font-mono tabular-nums", l.amount_msat > 0 ? "text-ok" : "")}>
                    {l.amount_msat > 0 ? "+" : "−"}
                    {sats(Math.abs(l.amount_msat))}
                  </td>
                  <td className="whitespace-nowrap px-5 py-2 text-right font-mono tabular-nums">{sats(l.balance_msat)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="p-4">
          <Empty>{data ? "No movements yet." : "Loading…"}</Empty>
        </div>
      )}
      {pages > 1 && (
        <div className="flex items-center justify-center gap-3 p-3 text-sm">
          <Button size="sm" variant="secondary" disabled={page === 0} onClick={() => setPage(page - 1)}>Newer</Button>
          <span className="text-muted">{page + 1} / {pages}</span>
          <Button size="sm" variant="secondary" disabled={page + 1 >= pages} onClick={() => setPage(page + 1)}>Older</Button>
        </div>
      )}
    </Card>
  );
}

/** Plain-language name of a journal entry kind (SPEC §6.3). */
export function entryLabel(kind: string): string {
  switch (kind) {
    case "payment_reserve":
      return "Payment sent (reserved with fee)";
    case "payment_settle":
      return "Unused fee reserve returned";
    case "payment_release":
      return "Failed payment returned";
    case "receive_settle":
      return "Payment received";
    case "transfer":
      return "Internal transfer";
    case "reconcile":
      return "Node balance adjustment";
    case "reversal":
      return "Correction";
    default:
      return kind;
  }
}

function MoveFunds({ account, kind, onClose }: { account: Account; kind: "topup" | "withdraw"; onClose: () => void }) {
  const balances = useSWR<Balances>("/api/balances", fetcher);
  const [amount, setAmount] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const available = kind === "topup" ? balances.data?.main_spendable_msat ?? null : account.balance_msat;
  return (
    <Modal open onClose={onClose} title={kind === "topup" ? `Top up ${account.name}` : `Withdraw from ${account.name}`}>
      <div className="space-y-4">
        <Field label="Amount (sats)" hint={available != null ? `Available: ${sats(available)} sats` : undefined}>
          <Input inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} autoFocus />
        </Field>
        {available != null && available > 0 && (
          <Button size="sm" variant="ghost" onClick={() => setAmount(String(Math.floor(available / 1000)))}>
            Use all
          </Button>
        )}
        <p className="text-xs text-muted">Moves happen inside Allod: no Lightning payment, no fee.</p>
        {error && <Alert tone="danger" title="Could not move the funds">{error}</Alert>}
        <Button
          className="w-full"
          disabled={!parseSats(amount) || busy}
          onClick={async () => {
            setError(null);
            setBusy(true);
            try {
              await api.post(`/api/accounts/${account.id}/${kind}`, { amount_msat: (parseSats(amount) ?? 0) * 1000 });
              refreshBalances();
              onClose();
            } catch (e) {
              setError(errText(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {kind === "topup" ? "Top up" : "Withdraw"}
        </Button>
      </div>
    </Modal>
  );
}

function Rename({ account, onClose, onDone }: { account: Account; onClose: () => void; onDone: () => void }) {
  const [name, setName] = useState(account.name);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title="Rename account">
      <div className="space-y-4">
        <Field label="Name">
          <Input value={name} maxLength={100} onChange={(e) => setName(e.target.value)} autoFocus />
        </Field>
        {error && <Alert tone="danger" title="Could not rename">{error}</Alert>}
        <Button
          className="w-full"
          disabled={!name.trim()}
          onClick={async () => {
            try {
              await api.patch(`/api/apps/${account.id}`, { name: name.trim() });
              onDone();
              refreshBalances();
              onClose();
            } catch (e) {
              setError(errText(e));
            }
          }}
        >
          Save
        </Button>
      </div>
    </Modal>
  );
}

/** Irreversible action confirmed by typing the account name (SPEC §13.4). */
function TypedConfirm({
  title,
  name,
  body,
  action,
  run,
  onClose,
}: {
  title: string;
  name: string;
  body: string;
  action: string;
  run: () => Promise<void>;
  onClose: () => void;
}) {
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title={title}>
      <div className="space-y-4">
        <p className="text-sm text-muted">{body}</p>
        <Field label={`Type ${name} to confirm`}>
          <Input value={typed} onChange={(e) => setTyped(e.target.value)} autoFocus />
        </Field>
        {error && <Alert tone="danger" title="Failed">{error}</Alert>}
        <Button
          variant="danger"
          className="w-full"
          disabled={typed !== name || busy}
          onClick={async () => {
            setError(null);
            setBusy(true);
            try {
              await run();
              onClose();
            } catch (e) {
              setError(errText(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {action}
        </Button>
      </div>
    </Modal>
  );
}

function CloseAccount({ account, onClose }: { account: Account; onClose: () => void }) {
  const nav = useNavigate();
  const funded = account.balance_msat > 0;
  return (
    <TypedConfirm
      title={`Close ${account.name}?`}
      name={account.name}
      action={funded ? "Sign, sweep to Main and close" : "Close account"}
      onClose={onClose}
      body={
        funded
          ? `The account still holds ${sats(account.balance_msat)} sats. They move to Main, and your Nostr signer is asked to confirm. The connection stops working; the history stays.`
          : "The connection stops working and the account disappears from the list. Its history stays in Activity."
      }
      run={async () => {
        let signer = null;
        if (funded) {
          signer = restoreSigner();
          if (!signer) throw new Error("Your Nostr signer is not connected in this browser. Sign out and sign in again, then retry.");
        }
        await closeAccount(account.id, signer);
        refreshBalances();
        nav("/accounts");
      }}
    />
  );
}
