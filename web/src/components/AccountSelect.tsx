import { CaretDown, Check, Stack } from "@phosphor-icons/react";
import { useEffect, useRef, useState } from "react";
import { Link } from "react-router";
import { selectAccount, useAccounts, useSelectedAccount, type AccountId } from "../lib/account";
import { sats } from "../lib/format";
import { cn } from "../lib/utils";
import { Sigil } from "./Sigil";

/** A form select of Main and the open accounts, with their balances. */
export function AccountSelect({
  value,
  onChange,
  mainBalanceMsat,
  label,
}: {
  value: AccountId;
  onChange: (id: AccountId) => void;
  mainBalanceMsat?: number | null;
  label: string;
}) {
  const { open } = useAccounts();
  if (!open.length) return null;
  return (
    <label className="block space-y-1.5">
      <span className="text-sm font-medium">{label}</span>
      <select
        className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
        value={String(value)}
        onChange={(e) => onChange(e.target.value === "main" ? "main" : Number(e.target.value))}
      >
        <option value="main">Main{mainBalanceMsat != null ? ` · ${sats(mainBalanceMsat)} sats` : ""}</option>
        {open.map((a) => (
          <option key={a.id} value={a.id}>
            {a.name} · {sats(a.balance_msat)} sats
          </option>
        ))}
      </select>
    </label>
  );
}

/** Top-bar account switcher (SPEC §13.2): Send, Receive and Activity follow it. */
export function AccountSwitcher() {
  const { open } = useAccounts();
  const { id, account } = useSelectedAccount();
  const [shown, setShown] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!shown) return;
    const close = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setShown(false);
    };
    window.addEventListener("mousedown", close);
    return () => window.removeEventListener("mousedown", close);
  }, [shown]);
  const pick = (next: AccountId) => {
    selectAccount(next);
    setShown(false);
  };
  return (
    <div className="relative" ref={ref}>
      <button
        type="button"
        className="flex items-center gap-2 rounded-full border border-line px-2 py-1 text-sm"
        aria-haspopup="menu"
        aria-expanded={shown}
        onClick={() => setShown((s) => !s)}
      >
        <Sigil seed={account?.app_pubkey ?? "main"} size={22} />
        <span className="max-w-32 truncate font-medium">{account?.name ?? "Main"}</span>
        <CaretDown size={14} className="text-muted" />
      </button>
      {shown && (
        <div role="menu" className="absolute right-0 z-30 mt-2 w-64 rounded-[var(--radius-card)] border border-line bg-surface p-1 shadow-lg">
          {[{ key: "main" as AccountId, name: "Main", seed: "main", balance: null as number | null }, ...open.map((a) => ({ key: a.id as AccountId, name: a.name, seed: a.app_pubkey, balance: a.balance_msat }))].map(
            (o) => (
              <button
                key={String(o.key)}
                role="menuitemradio"
                aria-checked={id === o.key}
                type="button"
                onClick={() => pick(o.key)}
                className={cn("flex w-full items-center gap-2 rounded-lg px-2 py-2 text-left text-sm hover:bg-surface-2", id === o.key && "text-accent")}
              >
                <Sigil seed={o.seed} size={22} />
                <span className="min-w-0 flex-1 truncate">{o.name}</span>
                {o.balance != null && <span className="font-mono text-xs text-muted">{sats(o.balance)}</span>}
                {id === o.key && <Check size={14} />}
              </button>
            ),
          )}
          <Link
            to="/accounts"
            onClick={() => setShown(false)}
            className="mt-1 flex items-center gap-2 border-t border-line px-2 pt-2 pb-1 text-sm text-muted hover:text-fg"
          >
            <Stack size={16} /> Manage accounts
          </Link>
        </div>
      )}
    </div>
  );
}
