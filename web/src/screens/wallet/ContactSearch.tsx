import { useMemo } from "react";
import useSWR from "swr";
import { Avatar } from "../../components/Person";
import { Badge } from "../../components/ui/badge";
import { fetcher, type PayTarget, type Profile } from "../../lib/api";
import { cn } from "../../lib/utils";

/** Text that is a payment itself (invoice, offer, address, URI) rather than a name. */
export function looksLikePayment(text: string): boolean {
  const t = text.trim().toLowerCase();
  return (
    t.startsWith("ln") ||
    t.startsWith("bitcoin:") ||
    t.startsWith("lightning:") ||
    t.startsWith("₿") ||
    t.startsWith("lnurl") ||
    /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(t) ||
    t.length > 64
  );
}

/** Payable Nostr contacts matching `query` by name, username, NIP-05 or address. */
export function useContactMatches(query: string): { matches: Profile[]; loading: boolean; discovering: boolean } {
  const q = query.trim().toLowerCase();
  const active = q.length > 0 && !looksLikePayment(q);
  // While the node is still asking contacts' own relays (NIP-65), refresh the list.
  const { data: resp, isLoading } = useSWR<{ contacts: Profile[]; discovering: boolean }>(
    active ? "/api/contacts" : null,
    fetcher,
    { revalidateOnFocus: false, refreshInterval: (latest) => (latest?.discovering ? 3000 : 0) },
  );
  const data = resp?.contacts;
  const matches = useMemo(() => {
    if (!active || !data) return [];
    const score = (p: Profile) => {
      const fields = [p.name, p.username, p.nip05, p.lud16].filter(Boolean).map((f) => f!.toLowerCase());
      if (fields.some((f) => f.startsWith(q))) return 0;
      if (fields.some((f) => f.includes(q))) return 1;
      return 2;
    };
    // Payable contacts first; the others are listed too (greyed, with the reason), so a
    // contact without a Lightning address or offer does not look missing.
    return data
      .filter((p) => score(p) < 2)
      .sort((a, b) => Number(!a.pay_target) - Number(!b.pay_target) || score(a) - score(b))
      .slice(0, 8);
  }, [active, data, q]);
  return { matches, loading: active && isLoading, discovering: active && !!resp?.discovering };
}

export function targetLabel(t: PayTarget): string {
  return t.kind === "bolt12" ? "BOLT12 offer" : t.kind === "lightning_address" ? t.value : "LNURL";
}

export function ContactSuggestions({
  matches,
  loading,
  discovering,
  highlighted,
  onPick,
}: {
  matches: Profile[];
  loading: boolean;
  discovering?: boolean;
  highlighted: number;
  onPick: (p: Profile) => void;
}) {
  if (!loading && !discovering && matches.length === 0) return null;
  return (
    <div className="mt-1 overflow-hidden rounded-[var(--radius-card)] border border-line bg-surface shadow-lg" role="listbox" aria-label="Contacts">
      {loading && <p className="px-3 py-2 text-sm text-muted">Searching your Nostr contacts…</p>}
      {!loading && discovering && (
        <p className="px-3 py-1.5 text-xs text-muted">Looking for more contacts on their own relays…</p>
      )}
      {matches.map((p, i) => (
        <button
          key={p.pubkey}
          type="button"
          role="option"
          aria-selected={i === highlighted}
          aria-disabled={!p.pay_target}
          disabled={!p.pay_target}
          // Keep the input focused while choosing.
          onMouseDown={(e) => e.preventDefault()}
          onClick={() => onPick(p)}
          className={cn(
            "flex w-full items-center gap-3 px-3 py-2 text-left",
            p.pay_target ? "hover:bg-surface-2" : "cursor-not-allowed opacity-60",
            i === highlighted && p.pay_target && "bg-surface-2",
          )}
        >
          <Avatar pubkey={p.pubkey} picture={p.picture} size={32} />
          <span className="min-w-0 flex-1">
            <span className="block truncate text-sm font-medium">{p.name ?? p.lud16 ?? "Unnamed"}</span>
            <span className="block truncate text-xs text-muted">
              {p.pay_target
                ? [p.username && `@${p.username}`, p.nip05 ?? p.lud16].filter(Boolean).join(" · ")
                : "no Lightning address or offer in their profile"}
            </span>
          </span>
          {p.offer && <Badge tone="accent">BOLT12</Badge>}
        </button>
      ))}
    </div>
  );
}

/** Chooses between a contact's targets; BOLT12 first. */
export function TargetChoice({ targets, value, onChange }: { targets: PayTarget[]; value: string; onChange: (t: PayTarget) => void }) {
  if (targets.length < 2) return null;
  return (
    <div className="space-y-1.5">
      <p className="text-xs text-muted">Pay with</p>
      <div className="flex flex-wrap gap-2">
        {targets.map((t) => (
          <button
            key={t.value}
            type="button"
            onClick={() => onChange(t)}
            className={cn(
              "max-w-full rounded-full border px-3 py-1.5 text-left text-xs",
              value === t.value ? "border-accent bg-accent-soft/60 text-fg" : "border-line text-muted hover:text-fg",
            )}
            title={t.value}
          >
            <span className="font-medium">{targetLabel(t)}</span>
            {t.kind === "bolt12" && <span className="ml-1 text-muted">· faster, more private</span>}
          </button>
        ))}
      </div>
    </div>
  );
}
