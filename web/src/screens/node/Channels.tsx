import { ArrowsSplit, Plus, Spinner } from "@phosphor-icons/react";
import { NodeLabel } from "../../components/NodeLabel";
import { useState } from "react";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { AnchorReserveWarning } from "../../components/AnchorReserveWarning";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { Empty, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type Balances, type Channel, type NodeInfo } from "../../lib/api";
import { parseSats } from "../../lib/format";
import { feeRate, useFeeEstimates } from "../../components/FeeEstimates";

/** The channel's capacity from our side to theirs: what we can send, our reserve, what is
 * neither (commitment fee, anchors, payments in flight), their reserve, what we can receive. */
function Bar({ c }: { c: Channel }) {
  const cap = c.capacity_sat * 1000 || 1;
  const mine = (c.reserve_sat ?? 0) * 1000;
  const theirs = (c.counterparty_reserve_sat ?? 0) * 1000;
  const other = Math.max(0, cap - c.outbound_capacity_msat - mine - theirs - c.inbound_capacity_msat);
  const pct = (msat: number) => `${(msat / cap) * 100}%`;
  const hatch = "repeating-linear-gradient(135deg, transparent 0 3px, rgb(0 0 0 / 0.18) 3px 5px)";
  return (
    <div className="flex h-2 w-full overflow-hidden rounded-full bg-surface-2" aria-hidden>
      <div className="h-full bg-accent" style={{ width: pct(c.outbound_capacity_msat) }} />
      <div className="h-full bg-accent/45" style={{ width: pct(mine), backgroundImage: hatch }} title="Your reserve" />
      <div className="h-full bg-line" style={{ width: pct(other) }} />
      <div className="h-full bg-muted/30" style={{ width: pct(theirs), backgroundImage: hatch }} title="Their reserve" />
      <div className="h-full bg-ok/40" style={{ width: pct(c.inbound_capacity_msat) }} />
    </div>
  );
}

/** mempool.space's transaction page on the node's network; none on regtest. */
export function txUrl(network: string | undefined, txid: string): string | null {
  const prefix: Record<string, string> = { bitcoin: "", signet: "signet/", testnet4: "testnet4/", testnet: "testnet/" };
  const p = network ? prefix[network] : undefined;
  return p == null ? null : `https://mempool.space/${p}tx/${txid}`;
}

/** The splice still waiting to confirm: the newest one, not yet the channel's funding. */
function pendingSplice(c: Channel) {
  const last = c.fundings?.[c.fundings.length - 1];
  return last?.kind === "splice" && last.txid !== c.funding_txid ? last : null;
}

/** A splice waiting to confirm, and a way to pay a higher fee for it (the node picks the rate). */
function SpliceConfirming({ c, onDone }: { c: Channel; onDone: () => void }) {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ tone: "ok" | "danger"; text: string } | null>(null);
  return (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      <Badge tone="warn">splice confirming</Badge>
      <Button
        size="sm"
        variant="ghost"
        disabled={busy}
        onClick={async () => {
          setBusy(true);
          setMsg(null);
          try {
            await api.post(`/api/channels/${c.user_channel_id}/splice-bump`, { counterparty: c.counterparty_node_id });
            setMsg({ tone: "ok", text: "A higher-fee replacement was sent; its transaction appears here once agreed." });
            onDone();
          } catch (e) {
            setMsg({ tone: "danger", text: e instanceof Error ? e.message : String(e) });
          } finally {
            setBusy(false);
          }
        }}
      >
        {busy && <Spinner size={14} className="animate-spin" />} Speed up
      </Button>
      {msg && <span className={msg.tone === "ok" ? "text-ok" : "text-danger"}>{msg.text}</span>}
    </div>
  );
}

/** The transactions that funded the channel: its opening, then each splice. */
function Fundings({ c, network }: { c: Channel; network?: string }) {
  const list = c.fundings?.length ? c.fundings : c.funding_txid ? [{ txid: c.funding_txid, kind: "open" as const, seen_at: 0 }] : [];
  if (!list.length) return null;
  return (
    <p className="flex flex-wrap gap-x-3 gap-y-0.5 text-xs text-muted">
      {list.map((f) => {
        const url = txUrl(network, f.txid);
        const short = `${f.txid.slice(0, 8)}…${f.txid.slice(-6)}`;
        const change = "change_sat" in f && f.change_sat ? Math.abs(f.change_sat) : null;
        const direction = "direction" in f ? f.direction : null;
        const label =
          f.kind === "open"
            ? "Opened"
            : direction === "out"
              ? "Splice out"
              : direction === "in"
                ? "Splice in"
                : "Splice";
        return (
          <span key={f.txid}>
            {label}
            {change != null && (
              <>
                {" "}
                {direction === "out" ? "−" : "+"}
                {change.toLocaleString()} sats
              </>
            )}{" "}
            ·{" "}
            {url ? (
              <a href={url} target="_blank" rel="noreferrer" className="font-mono text-accent underline-offset-2 hover:underline" title={f.txid}>
                {short}
              </a>
            ) : (
              <span className="font-mono" title={f.txid}>
                {short}
              </span>
            )}
          </span>
        );
      })}
    </p>
  );
}

export function Channels() {
  const { data, mutate } = useSWR<Channel[]>("/api/channels", fetcher, { refreshInterval: 15000 });
  const { data: node } = useSWR<NodeInfo>("/api/node", fetcher);
  const { data: balances } = useSWR<Balances>("/api/balances", fetcher, { refreshInterval: 30000 });
  const [openForm, setOpenForm] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [closing, setClosing] = useState<Channel | null>(null);
  const [splicing, setSplicing] = useState<{ channel: Channel; direction: "in" | "out" } | null>(null);
  // Splicing needs both sides: this node, and the peer (it said so when it connected).
  const canSplice = (c: Channel) => !!node?.capabilities.splicing && c.is_usable && !!c.counterparty_supports_splicing;

  return (
    <div className="space-y-4">
      {data && balances && <AnchorReserveWarning channels={data} spendableSat={balances.onchain.spendable_sat} />}
      <div className="flex justify-end">
        <Button onClick={() => setOpenForm(true)}><Plus size={16} /> Open channel</Button>
      </div>
      {data?.length ? (
        <Card className="divide-y divide-line">
          {data.map((c) => (
            <div key={c.user_channel_id} className="space-y-2 p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <NodeLabel pubkey={c.counterparty_node_id} alias={c.alias} head={10} tail={6} />
                  {c.counterparty_supports_splicing && (
                    <span
                      className="inline-flex text-accent"
                      title="Supports splicing: sats can be added to this channel from on-chain (splice in) or moved out to an on-chain address (splice out) without closing it."
                      aria-label="Supports splicing"
                      role="img"
                    >
                      <ArrowsSplit size={16} weight="bold" />
                    </span>
                  )}
                  {c.is_usable ? <Badge tone="ok">active</Badge> : c.is_channel_ready ? <Badge tone="warn">offline</Badge> : <Badge tone="warn">opening {c.confirmations ?? 0}/{c.confirmations_required ?? 6}</Badge>}
                  {c.is_announced ? <Badge>public</Badge> : <Badge>private</Badge>}
                </div>
                <div className="flex gap-2">
                  {canSplice(c) && (
                    <>
                      <Button size="sm" variant="ghost" onClick={() => setSplicing({ channel: c, direction: "in" })}>Splice in</Button>
                      <Button size="sm" variant="ghost" onClick={() => setSplicing({ channel: c, direction: "out" })}>Splice out</Button>
                    </>
                  )}
                  <Button size="sm" variant="ghost" onClick={() => setEditing(c)}>Fees</Button>
                  <Button size="sm" variant="ghost" onClick={() => setClosing(c)}>Close</Button>
                </div>
              </div>
              <Bar c={c} />
              <div className="flex justify-between gap-3 text-xs text-muted">
                <span>
                  Send <Amount noFiat msat={c.outbound_capacity_msat} />
                  {!!c.reserve_sat && (
                    <span title="Your balance the peer requires you to keep in this channel: it cannot be sent, and comes back to you when the channel closes.">
                      {" "}+ <Amount noFiat msat={c.reserve_sat * 1000} /> your reserve
                    </span>
                  )}
                </span>
                <span className="text-right">
                  Receive <Amount noFiat msat={c.inbound_capacity_msat} />
                  {!!c.counterparty_reserve_sat && (
                    <span title="The peer's balance your node requires it to keep in this channel: it can never come your way.">
                      {" "}+ <Amount noFiat msat={c.counterparty_reserve_sat * 1000} /> their reserve
                    </span>
                  )}
                </span>
              </div>
              <p className="text-xs text-muted">
                Capacity {c.capacity_sat.toLocaleString()} sats · fee {c.fee_base_msat ?? 0} msat + {c.fee_ppm ?? 0} ppm
              </p>
              <Fundings c={c} network={node?.network} />
              {pendingSplice(c) && <SpliceConfirming c={c} onDone={() => void mutate()} />}
            </div>
          ))}
        </Card>
      ) : (
        <Empty>{data ? "No channels yet. Open one to send and receive over Lightning." : "Loading…"}</Empty>
      )}
      <OpenChannel open={openForm} onClose={() => setOpenForm(false)} onDone={() => void mutate()} />
      {editing && <EditFees channel={editing} onClose={() => setEditing(null)} onDone={() => void mutate()} />}
      {splicing && (
        <SpliceChannel
          channel={splicing.channel}
          direction={splicing.direction}
          onchainSat={balances?.onchain.spendable_sat ?? null}
          onClose={() => setSplicing(null)}
          onDone={() => void mutate()}
        />
      )}
      {closing && <CloseChannel channel={closing} canForce={!!node?.capabilities.force_close} onClose={() => setClosing(null)} onDone={() => void mutate()} />}
    </div>
  );
}

function OpenChannel({ open, onClose, onDone }: { open: boolean; onClose: () => void; onDone: () => void }) {
  const [peer, setPeer] = useState("");
  const [amount, setAmount] = useState("");
  const [isPublic, setPublic] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open={open} onClose={onClose} title="Open a channel">
      <div className="space-y-4">
        <Field label="Peer" hint="node_id@host:port">
          <Input value={peer} onChange={(e) => setPeer(e.target.value)} placeholder="03abc…@203.0.113.1:9735" spellCheck={false} />
        </Field>
        <Field label="Amount (sats)" hint="At least 20,000 sats, paid from your on-chain balance.">
          <Input inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} />
        </Field>
        <label className="flex items-center gap-2 text-sm">
          <input type="checkbox" checked={isPublic} onChange={(e) => setPublic(e.target.checked)} /> Public channel (can route payments for others)
        </label>
        {error && <Alert tone="danger" title="Could not open the channel">{error}</Alert>}
        <Button
          className="w-full"
          disabled={busy || !peer.includes("@") || !parseSats(amount)}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              await api.post("/api/channels", { pubkey: peer.trim(), amount_sat: parseSats(amount), public: isPublic });
              onDone();
              onClose();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy && <Spinner size={16} className="animate-spin" />} Open channel
        </Button>
      </div>
    </Modal>
  );
}

function EditFees({ channel, onClose, onDone }: { channel: Channel; onClose: () => void; onDone: () => void }) {
  const [base, setBase] = useState(String(channel.fee_base_msat ?? 0));
  const [ppm, setPpm] = useState(String(channel.fee_ppm ?? 0));
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal open onClose={onClose} title="Routing fees">
      <div className="space-y-4">
        <Field label="Base fee (msat)"><Input inputMode="numeric" value={base} onChange={(e) => setBase(e.target.value)} /></Field>
        <Field label="Proportional fee (ppm)"><Input inputMode="numeric" value={ppm} onChange={(e) => setPpm(e.target.value)} /></Field>
        {error && <Alert tone="danger" title="Could not update">{error}</Alert>}
        <Button
          className="w-full"
          onClick={async () => {
            try {
              await api.patch(`/api/channels/${channel.user_channel_id}`, {
                counterparty: channel.counterparty_node_id,
                fee_base_msat: Number(base) || 0,
                fee_ppm: Number(ppm) || 0,
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

function CloseChannel({ channel, canForce, onClose, onDone }: { channel: Channel; canForce: boolean; onClose: () => void; onDone: () => void }) {
  const [force, setForce] = useState(false);
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const word = channel.user_channel_id.slice(0, 6);
  return (
    <Modal open onClose={onClose} title="Close channel">
      <div className="space-y-4">
        <p className="text-sm text-muted">The channel balance returns to your on-chain wallet.</p>
        {canForce && (
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={force} onChange={(e) => setForce(e.target.checked)} /> Force close (peer offline)
          </label>
        )}
        {force && (
          <Alert tone="danger" title="Force close locks your funds for days">
            Your share is only spendable after a time lock, and the closing fee is higher. Type <b className="font-mono">{word}</b> to confirm.
            <Input className="mt-2" value={confirm} onChange={(e) => setConfirm(e.target.value)} />
          </Alert>
        )}
        {error && <Alert tone="danger" title="Could not close">{error}</Alert>}
        <Button
          variant="danger"
          className="w-full"
          disabled={force && confirm !== word}
          onClick={async () => {
            try {
              await api.del(`/api/channels/${channel.user_channel_id}?counterparty=${channel.counterparty_node_id}&force=${force}`);
              onDone();
              onClose();
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            }
          }}
        >
          {force ? "Force close" : "Close channel"}
        </Button>
      </div>
    </Modal>
  );
}

/** Adds on-chain funds to a channel (in) or moves channel funds on-chain (out) without
 * closing it: one on-chain transaction replaces the channel's funding, and the channel
 * stays usable meanwhile. */
function SpliceChannel({
  channel,
  direction,
  onchainSat,
  onClose,
  onDone,
}: {
  channel: Channel;
  direction: "in" | "out";
  onchainSat: number | null;
  onClose: () => void;
  onDone: () => void;
}) {
  const [amount, setAmount] = useState("");
  const [address, setAddress] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState(false);
  const sats = parseSats(amount);
  const max = direction === "in" ? onchainSat : Math.floor(channel.outbound_capacity_msat / 1000);
  // ldk-node funds a splice at Bitcoin Core's 12-block estimate, up to 1.5× if the peer
  // asks; the rate cannot be chosen.
  const { data: fees } = useFeeEstimates();
  const rate = fees?.targets.find((t) => t.blocks === 12)?.sat_per_vb ?? null;
  const tooMuch = !!sats && max != null && sats > max;
  if (done)
    return (
      <Modal open onClose={onClose} title={direction === "in" ? "Splice in started" : "Splice out started"}>
        <div className="space-y-4 text-sm">
          <p>
            The new funding transaction is on its way. The channel stays usable meanwhile; its new capacity counts once
            the transaction confirms.
          </p>
          <Button className="w-full" onClick={onClose}>
            Done
          </Button>
        </div>
      </Modal>
    );
  return (
    <Modal open onClose={onClose} title={direction === "in" ? "Splice in" : "Splice out"}>
      <div className="space-y-4">
        <p className="text-sm text-muted">
          {direction === "in"
            ? "Adds sats from your on-chain balance to this channel, so you can send more over it. No new channel: one on-chain transaction, its fee paid from your on-chain balance."
            : "Moves sats from this channel to an on-chain address, without closing it. One on-chain transaction, its fee taken from the channel."}
        </p>
        <Field
          label="Amount (sats)"
          hint={
            max != null
              ? direction === "in"
                ? `Up to ${max.toLocaleString()} sats on-chain, less the fee.`
                : `Up to ${max.toLocaleString()} sats (what you can send over it; the reserve stays).`
              : undefined
          }
        >
          <Input inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} />
        </Field>
        {direction === "out" && (
          <Field label="Address (optional)" hint="Empty: your node's own on-chain wallet.">
            <Input value={address} onChange={(e) => setAddress(e.target.value)} placeholder="bc1…" spellCheck={false} />
          </Field>
        )}
        <p className="rounded-lg bg-surface-2 p-3 text-xs text-muted">
          Fee rate:{" "}
          <span className="font-mono text-fg">{rate != null ? `about ${feeRate(rate)} sat/vB` : "no estimate yet"}</span>, Bitcoin
          Core&apos;s estimate for confirming within about 2 hours (Low priority). The node picks it, up to 1.5× if the peer
          asks for more; if fees rise before it confirms, use Speed up on the channel.
        </p>
        {tooMuch && <p className="text-xs text-warn">More than is available.</p>}
        {error && <Alert tone="danger" title="Could not splice">{error}</Alert>}
        <Button
          className="w-full"
          disabled={busy || !sats || tooMuch}
          onClick={async () => {
            setBusy(true);
            setError(null);
            try {
              await api.post(`/api/channels/${channel.user_channel_id}/splice-${direction}`, {
                counterparty: channel.counterparty_node_id,
                amount_sat: sats,
                ...(direction === "out" && address.trim() ? { address: address.trim() } : {}),
              });
              onDone();
              setDone(true);
            } catch (e) {
              setError(e instanceof Error ? e.message : String(e));
            } finally {
              setBusy(false);
            }
          }}
        >
          {busy && <Spinner size={16} className="animate-spin" />} {direction === "in" ? "Splice in" : "Splice out"}
        </Button>
      </div>
    </Modal>
  );
}
