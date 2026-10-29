// Amount and time formatting. Amounts are millisatoshis internally (SPEC §6.1).

export function sats(msat: number | null | undefined): string {
  if (msat == null) return "—";
  return Math.floor(msat / 1000).toLocaleString();
}

export function fiat(msat: number | null | undefined, rate?: { currency: string; btc_price?: number }): string | null {
  if (msat == null || !rate?.btc_price || rate.currency === "none") return null;
  const value = (msat / 1e11) * rate.btc_price;
  try {
    return new Intl.NumberFormat(undefined, { style: "currency", currency: rate.currency, maximumFractionDigits: value < 10 ? 2 : 0 }).format(value);
  } catch {
    return null;
  }
}

export function ago(unix: number | null | undefined): string {
  if (!unix) return "";
  const s = Math.floor(Date.now() / 1000) - unix;
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  return new Date(unix * 1000).toLocaleDateString();
}

export function dateTime(unix: number | null | undefined): string {
  return unix ? new Date(unix * 1000).toLocaleString() : "—";
}

/** Parses a sats input ("21,000" -> 21000). */
export function parseSats(input: string): number | null {
  const v = Number(input.replace(/[\s,_]/g, ""));
  return Number.isFinite(v) && v > 0 ? Math.floor(v) : null;
}
