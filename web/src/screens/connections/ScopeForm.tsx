import { Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { RENEWALS, SCOPES } from "./scopes";

export interface ScopeFormValue {
  scopes: string[];
  budgetSats: string;
  renewal: string;
  isolated: boolean;
}

export function ScopeForm({
  value,
  onChange,
  lockIsolated,
}: {
  value: ScopeFormValue;
  onChange: (v: ScopeFormValue) => void;
  lockIsolated?: boolean;
}) {
  const toggle = (id: string) =>
    onChange({ ...value, scopes: value.scopes.includes(id) ? value.scopes.filter((s) => s !== id) : [...value.scopes, id] });
  return (
    <div className="space-y-5">
      <fieldset className="space-y-2">
        <legend className="mb-2 text-sm font-medium">Permissions</legend>
        {SCOPES.map((s) => {
          const disabled = s.id === "sign_message" && value.isolated;
          return (
            <label key={s.id} className={`flex items-start gap-3 rounded-lg p-2 ${disabled ? "opacity-50" : "hover:bg-surface-2"}`}>
              <input
                type="checkbox"
                className="mt-1"
                disabled={disabled}
                checked={value.scopes.includes(s.id) && !disabled}
                onChange={() => toggle(s.id)}
              />
              <span>
                <span className="block text-sm">{s.label}</span>
                <span className="block text-xs text-muted">{s.hint}</span>
              </span>
            </label>
          );
        })}
      </fieldset>
      {value.scopes.includes("pay_invoice") && (
        <div className="grid gap-4 sm:grid-cols-2">
          <Field label="Budget (sats)" hint="Leave empty for no limit.">
            <Input inputMode="numeric" value={value.budgetSats} onChange={(e) => onChange({ ...value, budgetSats: e.target.value })} />
          </Field>
          <Field label="Renews">
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={value.renewal}
              onChange={(e) => onChange({ ...value, renewal: e.target.value })}
            >
              {RENEWALS.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
          </Field>
        </div>
      )}
      {!lockIsolated && (
        <label className="flex items-start gap-3 text-sm">
          <input
            type="checkbox"
            className="mt-1"
            checked={value.isolated}
            onChange={(e) => onChange({ ...value, isolated: e.target.checked, scopes: e.target.checked ? value.scopes.filter((s) => s !== "sign_message") : value.scopes })}
          />
          <span>
            Isolated balance
            <span className="block text-xs text-muted">The connection gets its own balance and sees only its own payments.</span>
          </span>
        </label>
      )}
    </div>
  );
}
