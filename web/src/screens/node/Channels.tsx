import { Plus, Spinner } from "@phosphor-icons/react";
import { useState } from "react";
import useSWR from "swr";
import { Amount } from "../../components/Amount";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card } from "../../components/ui/card";
import { Empty, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type Channel, type NodeInfo } from "../../lib/api";
import { parseSats } from "../../lib/format";
import { shorten } from "../../lib/utils";

function Bar({ c }: { c: Channel }) {
  const total = c.outbound_capacity_msat + c.inbound_capacity_msat || 1;
  const out = (c.outbound_capacity_msat / total) * 100;
  return (
    <div className="h-2 w-full overflow-hidden rounded-full bg-surface-2">
      <div className="h-full bg-accent" style={{ width: `${out}%` }} />
    </div>
  );
}

export function Channels() {
  const { data, mutate } = useSWR<Channel[]>("/api/channels", fetcher, { refreshInterval: 15000 });
  const { data: node } = useSWR<NodeInfo>("/api/node", fetcher);
  const [openForm, setOpenForm] = useState(false);
  const [editing, setEditing] = useState<Channel | null>(null);
  const [closing, setClosing] = useState<Channel | null>(null);

  return (
    <div className="space-y-4">
      <div className="flex justify-end">
        <Button onClick={() => setOpenForm(true)}><Plus size={16} /> Open channel</Button>
      </div>
      {data?.length ? (
        <Card className="divide-y divide-line">
          {data.map((c) => (
            <div key={c.user_channel_id} className="space-y-2 p-4">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <div className="flex items-center gap-2">
                  <span className="font-mono text-xs">{shorten(c.counterparty_node_id, 10, 6)}</span>
                  {c.is_usable ? <Badge tone="ok">active</Badge> : c.is_channel_ready ? <Badge tone="warn">offline</Badge> : <Badge tone="warn">opening {c.confirmations ?? 0}/{c.confirmations_required ?? 6}</Badge>}
                  {c.is_announced ? <Badge>public</Badge> : <Badge>private</Badge>}
                </div>
                <div className="flex gap-2">
                  <Button size="sm" variant="ghost" onClick={() => setEditing(c)}>Fees</Button>
                  <Button size="sm" variant="ghost" onClick={() => setClosing(c)}>Close</Button>
                </div>
              </div>
              <Bar c={c} />
              <div className="flex justify-between text-xs text-muted">
                <span>Send <Amount msat={c.outbound_capacity_msat} /></span>
                <span className="text-right">Receive <Amount msat={c.inbound_capacity_msat} /></span>
              </div>
              <p className="text-xs text-muted">
                Capacity {c.capacity_sat.toLocaleString()} sats · fee {c.fee_base_msat ?? 0} msat + {c.fee_ppm ?? 0} ppm
              </p>
            </div>
          ))}
        </Card>
      ) : (
        <Empty>{data ? "No channels yet. Open one to send and receive over Lightning." : "Loading…"}</Empty>
      )}
      <OpenChannel open={openForm} onClose={() => setOpenForm(false)} onDone={() => void mutate()} />
      {editing && <EditFees channel={editing} onClose={() => setEditing(null)} onDone={() => void mutate()} />}
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
