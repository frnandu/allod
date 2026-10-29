import { Amount } from "../../components/Amount";
import type { Budget } from "../../lib/api";

export function BudgetBar({ budget }: { budget: Budget | null }) {
  if (!budget) return <p className="text-xs text-muted">No budget limit</p>;
  const pct = Math.min(100, (budget.used_budget / Math.max(1, budget.total_budget)) * 100);
  return (
    <div className="space-y-1">
      <div className="h-2 w-full overflow-hidden rounded-full bg-surface-2">
        <div className={pct > 90 ? "h-full bg-danger" : "h-full bg-accent"} style={{ width: `${pct}%` }} />
      </div>
      <p className="text-xs text-muted">
        <Amount msat={budget.used_budget} /> of <Amount msat={budget.total_budget} /> {budget.renewal_period !== "never" && `· ${budget.renewal_period}`}
      </p>
    </div>
  );
}
