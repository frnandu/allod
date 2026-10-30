// Plain-language scope labels and presets (SPEC §9.1, §13 Connections).

export const SCOPES: { id: string; label: string; hint: string }[] = [
  { id: "pay_invoice", label: "Send payments", hint: "Pay invoices and offers, within the budget" },
  { id: "get_balance", label: "Read balance", hint: "See the balance it can spend" },
  { id: "make_invoice", label: "Receive payments", hint: "Create invoices and offers" },
  { id: "lookup_invoice", label: "Look up payments", hint: "Check the status of a payment" },
  { id: "list_transactions", label: "Read history", hint: "List its transactions" },
  { id: "get_info", label: "Read node info", hint: "Node alias, public key and network" },
  { id: "notifications", label: "Payment notifications", hint: "Be told when payments arrive or leave" },
  { id: "sign_message", label: "Sign messages", hint: "Sign with the node key (not for isolated connections)" },
];

export type Preset = "readonly" | "budget" | "full" | "custom";

export const PRESETS: { id: Preset; label: string; scopes: string[]; budgetSats?: number }[] = [
  { id: "readonly", label: "Read-only", scopes: ["get_balance", "lookup_invoice", "list_transactions", "get_info"] },
  {
    id: "budget",
    label: "Payments with budget",
    scopes: ["pay_invoice", "get_balance", "make_invoice", "lookup_invoice", "list_transactions", "notifications"],
    budgetSats: 10_000,
  },
  {
    id: "full",
    label: "Full access",
    scopes: ["pay_invoice", "get_balance", "make_invoice", "lookup_invoice", "list_transactions", "get_info", "notifications"],
  },
  { id: "custom", label: "Custom", scopes: [] },
];

export const RENEWALS = ["daily", "weekly", "monthly", "yearly", "never"] as const;

export const EXPIRIES: { label: string; days: number | null }[] = [
  { label: "Never", days: null },
  { label: "1 week", days: 7 },
  { label: "1 month", days: 30 },
  { label: "1 year", days: 365 },
];

export function scopeLabel(id: string): string {
  if (id === "superuser") return "Create app connections";
  return SCOPES.find((s) => s.id === id)?.label ?? id;
}
