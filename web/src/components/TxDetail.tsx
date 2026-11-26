import { nip19 } from "nostr-tools";
import useSWR from "swr";
import { fetcher, type Tx } from "../lib/api";
import { dateTime, sats } from "../lib/format";
import { BOLT12_ZAP_NOTE } from "../lib/zap";
import { Amount } from "./Amount";
import { npub, personLabel, useProfile } from "./Person";
import { stateTone, txTitle } from "./TxRow";
import { Badge } from "./ui/badge";
import { CopyButton, Modal } from "./ui/extras";
import { ZapPublication } from "./ZapPublication";

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

/** What is known about a settled zap's receipt. */
function zapStatus(tx: Tx): string | null {
  if (!tx.zap || tx.state !== "SETTLED") return null;
  if (tx.zap.receipt) return "Published";
  // A NIP-57 receipt of a payment to someone else comes from their service.
  const theirs = tx.direction === "outgoing" && !tx.self_payment && tx.payment_type === "bolt11";
  return theirs ? "Published by the recipient's service (not checked by Allod)" : "Not published yet";
}

export function TxDetail({ id, onClose }: { id: string | null; onClose: () => void }) {
  const { data: tx, mutate } = useSWR<Tx>(id ? `/api/transactions/${id}` : null, fetcher);
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
            <Row k="Type" v={`${tx.payment_type}${tx.hold ? " (hold)" : ""}${tx.jit ? " (just-in-time)" : ""}${tx.self_payment ? " · internal" : ""}`} />
            <Row k="Zap receipt" v={zapStatus(tx)} />
            <Row k="Zap event" v={tx.zap?.receipt ? nip19.noteEncode(tx.zap.receipt) : null} copy />
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
          {tx.direction === "outgoing" && tx.state === "SETTLED" && tx.payer_note?.startsWith(BOLT12_ZAP_NOTE) && !tx.zap?.receipt && (
            <ZapPublication key={tx.public_id} transaction={tx.public_id} onPublished={() => void mutate()} />
          )}
        </div>
      )}
    </Modal>
  );
}
