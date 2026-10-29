import useSWR from "swr";
import { fetcher, type Tx } from "../lib/api";
import { dateTime, sats } from "../lib/format";
import { Amount } from "./Amount";
import { npub, personLabel, useProfile } from "./Person";
import { stateTone, txTitle } from "./TxRow";
import { Badge } from "./ui/badge";
import { CopyButton, Modal } from "./ui/extras";

function Row({ k, v, copy }: { k: string; v?: string | number | null; copy?: boolean }) {
  if (v == null || v === "") return null;
  return (
    <div className="grid grid-cols-[8rem_1fr] gap-2 py-2 text-sm">
      <span className="text-muted">{k}</span>
      <span className="flex min-w-0 items-start gap-2">
        <span className="break-all font-mono text-xs">{String(v)}</span>
        {copy && <CopyButton value={String(v)} label="" />}
      </span>
    </div>
  );
}

export function TxDetail({ id, onClose }: { id: string | null; onClose: () => void }) {
  const { data: tx } = useSWR<Tx>(id ? `/api/transactions/${id}` : null, fetcher);
  const cp = tx?.counterparty;
  const profile = useProfile(cp?.pubkey);
  return (
    <Modal open={!!id} onClose={onClose} title={tx ? txTitle(tx) : "Payment"}>
      {tx && (
        <div className="space-y-4">
          <div className="flex items-center justify-between">
            <Amount msat={tx.amount_msat} sign={tx.direction === "incoming" ? "+" : "-"} big />
            <Badge tone={stateTone(tx.state)}>{tx.state.toLowerCase()}</Badge>
          </div>
          <div className="divide-y divide-line">
            <Row k={tx.direction === "incoming" ? "From" : "To"} v={personLabel(cp, profile)} />
            <Row k="Nostr" v={cp?.pubkey ? npub(cp.pubkey) : null} copy />
            <Row k="Address" v={cp?.identifier} />
            <Row k={cp?.source === "zap" ? "Zap comment" : "Comment"} v={cp?.comment} />
            <Row k="Type" v={`${tx.payment_type}${tx.hold ? " (hold)" : ""}${tx.self_payment ? " · internal" : ""}`} />
            <Row k="Fee" v={tx.direction === "outgoing" ? `${sats(tx.fee_msat)} sats` : null} />
            <Row k="Created" v={dateTime(tx.created_at)} />
            <Row k="Settled" v={tx.settled_at ? dateTime(tx.settled_at) : null} />
            <Row k="Expires" v={tx.state === "PENDING" && tx.expires_at ? dateTime(tx.expires_at) : null} />
            <Row k="Settle by block" v={tx.settle_deadline} />
            <Row k="Failure" v={tx.failure_reason} />
            <Row k="Payer note" v={tx.payer_note} />
            <Row k="Payment hash" v={tx.payment_hash} copy />
            <Row k="Preimage" v={tx.preimage} copy />
            <Row k="Payer proof" v={tx.payer_proof} copy />
            <Row k="Invoice" v={tx.payment_request} copy />
            <Row k="Offer" v={tx.offer} copy />
            <Row k="Labels" v={tx.labels.length ? tx.labels.join(", ") : null} />
            <Row k="ID" v={tx.public_id} />
          </div>
        </div>
      )}
    </Modal>
  );
}
