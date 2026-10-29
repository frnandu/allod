import { useMemo, useState } from "react";
import useSWR, { mutate } from "swr";
import { currencyName, fetcher, RATE_SOURCES, saveSettings, type RateSource, type Settings } from "../lib/api";
import { cn } from "../lib/utils";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Input, Label } from "./ui/input";

const POPULAR = ["USD", "EUR", "GBP", "BRL", "ARS", "NGN", "INR", "JPY", "CHF"];

/**
 * Display currency and rate source (SPEC §13). Amounts are always in sats; the fiat
 * value is shown next to them. Allod uses USD until the owner picks another here.
 */
export function CurrencySettings({ onSaved }: { onSaved?: (s: Settings) => void }) {
  const current = useSWR<Settings>("/api/settings", fetcher);
  const [source, setSource] = useState<RateSource>(current.data?.fiat_source ?? "yadio");
  const [currency, setLocal] = useState<string>(current.data?.fiat_currency ?? "USD");
  const [saved, setSaved] = useState(false);
  // Show the stored choice once it loads.
  const [loadedFrom, setLoadedFrom] = useState<Settings | undefined>(undefined);
  if (current.data && loadedFrom !== current.data) {
    setLoadedFrom(current.data);
    setSource(current.data.fiat_source);
    setLocal(current.data.fiat_currency);
  }
  const [query, setQuery] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const list = useSWR<{ currencies: string[]; host: string }>(`/api/fiat/currencies?source=${source}`, fetcher);
  const host = RATE_SOURCES.find((s) => s.id === source)!.host;

  const shown = useMemo(() => {
    const all = list.data?.currencies ?? [];
    const q = query.trim().toLowerCase();
    const match = (c: string) => !q || c.toLowerCase().includes(q) || currencyName(c).toLowerCase().includes(q);
    const popular = POPULAR.filter((c) => all.includes(c) && match(c));
    const rest = all.filter((c) => !POPULAR.includes(c) && match(c));
    return [...popular, ...rest];
  }, [list.data, query]);

  async function save() {
    setError(null);
    setBusy(true);
    try {
      const s = await saveSettings({ fiat_currency: currency, fiat_source: source });
      await mutate("/api/settings", s, { revalidate: false });
      await mutate((key) => typeof key === "string" && key.startsWith("/api/fiat/rate"));
      setSaved(true);
      onSaved?.(s);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="space-y-6">

      <button
        type="button"
        onClick={() => setLocal("none")}
        className={cn(
          "w-full rounded-lg border px-4 py-3 text-left text-sm",
          currency === "none" ? "border-accent bg-accent-soft/50 font-medium" : "border-line",
        )}
      >
        Sats only <span className="block text-xs font-normal text-muted">No exchange rates are fetched</span>
      </button>

      <div className="space-y-2">
        <Label>Rate source</Label>
        <div className="grid grid-cols-3 gap-2">
          {RATE_SOURCES.map((s) => (
            <button
              key={s.id}
              type="button"
              onClick={() => setSource(s.id)}
              className={cn(
                "rounded-lg border px-3 py-2 text-left text-sm",
                source === s.id ? "border-accent bg-accent-soft/50" : "border-line",
              )}
            >
              <span className="block font-medium">{s.name}</span>
              <span className="block text-xs text-muted">{s.note}</span>
            </button>
          ))}
        </div>
      </div>

      <div className="space-y-2">
        <Label htmlFor="currency-search">Currency</Label>
        <Input
          id="currency-search"
          placeholder="Search, e.g. peso or NGN"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
        <div className="max-h-56 overflow-y-auto rounded-lg border border-line" role="listbox" aria-label="Currencies">
          {list.error && <p className="p-3 text-sm text-danger">Could not load currencies from {host}.</p>}
          {!list.data && !list.error && <p className="p-3 text-sm text-muted">Loading currencies…</p>}
          {shown.map((c) => (
            <button
              key={c}
              type="button"
              role="option"
              aria-selected={currency === c}
              onClick={() => setLocal(c)}
              className={cn(
                "flex w-full items-center justify-between px-3 py-2 text-left text-sm hover:bg-surface-2",
                currency === c && "bg-accent-soft/60 font-medium",
              )}
            >
              <span>{currencyName(c)}</span>
              <span className="font-mono text-xs text-muted">{c}</span>
            </button>
          ))}
          {list.data && shown.length === 0 && <p className="p-3 text-sm text-muted">No match.</p>}
        </div>
      </div>

      {currency !== "none" && (
        <p className="text-xs text-muted">
          Your node fetches rates from {host}; your browser never contacts it. {host} sees your node&apos;s IP
          address.
        </p>
      )}
      {error && <Alert tone="danger" title="Could not save">{error}</Alert>}
      {saved && <p className="text-sm text-ok">Saved.</p>}
      <Button disabled={busy} onClick={() => void save()}>
        Save
      </Button>
    </div>
  );
}
