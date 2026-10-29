import { ArrowDownLeft, ArrowUpRight, ArrowsLeftRight, Clock, Lightning, XCircle } from "@phosphor-icons/react";
import type { Tx } from "../lib/api";
import { ago } from "../lib/format";
import { cn } from "../lib/utils";
import { Amount } from "./Amount";
import { Avatar, personLabel, useProfile } from "./Person";
import { Badge } from "./ui/badge";

export function txTitle(tx: Tx): string {
  if (tx.description) return tx.description;
  if (tx.payment_type === "internal") return tx.direction === "incoming" ? "Transfer in" : "Transfer out";
  return tx.direction === "incoming" ? "Received" : "Sent";
}

export function stateTone(state: Tx["state"]) {
  return state === "SETTLED" ? "ok" : state === "FAILED" || state === "CANCELED" || state === "EXPIRED" ? "danger" : "warn";
}

export function TxRow({ tx, onClick }: { tx: Tx; onClick?: () => void }) {
  const incoming = tx.direction === "incoming";
  const Icon = tx.payment_type === "internal" ? ArrowsLeftRight : incoming ? ArrowDownLeft : ArrowUpRight;
  const cp = tx.counterparty;
  const profile = useProfile(cp?.pubkey);
  const who = personLabel(cp, profile);
  const title = who ? `${incoming ? "From" : "To"} ${who}` : txTitle(tx);
  const sub = cp?.comment ?? (who && tx.description ? tx.description : null);
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex w-full items-center gap-3 px-4 py-3 text-left transition hover:bg-surface-2"
    >
      {cp?.pubkey ? (
        <span className="relative shrink-0">
          <Avatar pubkey={cp.pubkey} picture={profile?.picture} />
          <span className={cn("absolute -bottom-1 -right-1 rounded-full p-0.5", incoming ? "bg-ok text-white" : "bg-fg text-bg")}>
            <Icon size={10} weight="bold" />
          </span>
        </span>
      ) : (
        <span
          className={cn(
            "flex size-9 shrink-0 items-center justify-center rounded-full",
            incoming ? "bg-ok/15 text-ok" : "bg-surface-2 text-fg",
          )}
        >
          <Icon size={18} weight="bold" />
        </span>
      )}
      <span className="min-w-0 flex-1">
        <span className="block truncate text-sm font-medium">{title}</span>
        {sub && <span className="block truncate text-xs text-muted">“{sub}”</span>}
        <span className="flex items-center gap-2 text-xs text-muted">
          {cp?.source === "zap" ? "Zap" : tx.payment_type === "bolt12" ? "Offer" : tx.payment_type === "internal" ? "Internal" : <Lightning size={12} />}
          {ago(tx.settled_at ?? tx.created_at)}
          {tx.self_payment && tx.payment_type !== "internal" && <span>· internal</span>}
        </span>
      </span>
      <span className="flex flex-col items-end gap-1">
        <Amount msat={tx.amount_msat} sign={incoming ? "+" : "-"} className={cn("text-right text-sm", incoming && "text-ok")} />
        {tx.state !== "SETTLED" && (
          <Badge tone={stateTone(tx.state)}>
            {tx.state === "PENDING" || tx.state === "ACCEPTED" ? <Clock size={12} /> : <XCircle size={12} />}
            {tx.state.toLowerCase()}
          </Badge>
        )}
      </span>
    </button>
  );
}
