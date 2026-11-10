import { useAccounts, type AccountId } from "../lib/account";
import { sats } from "../lib/format";

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
