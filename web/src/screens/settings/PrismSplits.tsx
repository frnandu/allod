import { ArrowsClockwise, ArrowSquareOut, Plus, Trash, Warning } from "@phosphor-icons/react";
import { useState } from "react";
import { PrismLogo } from "../../components/PrismLogo";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { domains as domainsApi, type DomainAddress, type Domains, type PrismSplit } from "../../lib/api";
import { sats } from "../../lib/format";
import { cn } from "../../lib/utils";

/** The prism article: background on what splitting a payment at the sender's wallet means. */
const PRISM_ARTICLE = "https://dergigi.com/2023/03/12/lightning-prisms/";

/** One split being edited. `key` only tells rows apart in React. */
interface Row {
  key: number;
  destination: string;
  /** Percent as typed, e.g. "2.5"; parsed into basis points on save. */
  percent: string;
}

/** A bar colour per split, taken from the beams of the prism mark. */
const BEAMS = ["bg-[#ff5f7a]", "bg-[#ffe077]", "bg-[#b8eb7c]", "bg-[#7ad7ee]", "bg-[#a78bfa]", "bg-[#f0a3c8]"];

type Kind = PrismSplit["kind"];
/** A plain `user@domain` may be either; the server knows which one only for our own names. */
type ShownKind = Kind | "address";

const KIND_LABEL: Record<ShownKind, string> = {
  lnurl: "LNURL",
  bip353: "BIP-353",
  bolt12: "BOLT12",
  address: "BIP-353 or LNURL",
};

/**
 * What a destination looks like, as far as the browser can tell (the node validates it for
 * real when the splits are saved). `null`: not something an address can forward to.
 */
function kindOf(destination: string): ShownKind | null {
  const d = destination.trim().replace(/^lightning:/i, "");
  if (!d) return null;
  if (d.startsWith("₿")) return d.includes("@") ? "bip353" : null;
  const lower = d.toLowerCase();
  if (lower.startsWith("lno1")) return lower.length > 8 ? "bolt12" : null;
  if (lower.startsWith("lnurl1") || lower.startsWith("lnurlp://")) return "lnurl";
  const [user, domain] = d.split("@");
  // Without `₿`, only asking the name tells BIP-353 from LNURL: the server checks it when
  // the splits are saved, and this row then shows the saved kind.
  if (user && domain && domain.includes(".")) return "address";
  return null;
}

/** "2.5" -> 250 basis points. `null` when it is not a percentage. */
function toBps(input: string): number | null {
  const v = Number(input.replace(/[\s,%]/g, ""));
  if (!Number.isFinite(v) || v <= 0) return null;
  return Math.round(v * 100);
}

/** 250 -> "2.5", 6500 -> "65". */
export function fromBps(bps: number): string {
  return (bps / 100).toFixed(2).replace(/\.?0+$/, "");
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

interface Draft {
  splits: { destination: string; percent_bps: number }[];
  errors: (string | null)[];
  /** Sum of the valid rows, in basis points. */
  totalBps: number;
}

/** Checks the rows as typed; `splits` holds only the rows that are ready to save. */
function check(rows: Row[]): Draft {
  const errors: (string | null)[] = [];
  const splits: { destination: string; percent_bps: number }[] = [];
  let totalBps = 0;
  rows.forEach((r, i) => {
    const destination = r.destination.trim();
    const bps = toBps(r.percent);
    if (!destination) {
      // An untouched extra row is not an error, it is simply not a split yet.
      errors.push(r.percent.trim() ? "Enter where this split goes." : null);
      return;
    }
    if (!kindOf(destination)) {
      errors.push("Use a Lightning address, LNURL or BOLT12 offer.");
      return;
    }
    if (bps === null) {
      errors.push("Enter a percentage, at least 0.01%.");
      return;
    }
    if (bps > 10_000) {
      errors.push("A split cannot be more than 100%.");
      return;
    }
    const taken = rows
      .slice(0, i)
      .some((o) => o.destination.trim().toLowerCase() === destination.toLowerCase());
    if (taken) {
      errors.push("This destination is already a split.");
      return;
    }
    totalBps += bps;
    errors.push(null);
    splits.push({ destination, percent_bps: bps });
  });
  const tooMuch = totalBps > 10_000;
  if (tooMuch) {
    errors.forEach((e, i) => {
      if (e === null) errors[i] = "The splits add up to more than 100%.";
    });
  }
  return { splits, errors, totalBps };
}

/** What this split did with the payments it was part of (matched by destination). */
function splitStats(saved: PrismSplit[] | undefined, destination: string): PrismSplit | undefined {
  return saved?.find((s) => s.destination.toLowerCase() === destination.trim().toLowerCase());
}

/** One split's record so far: forwarded, waiting, or never sent. */
function SplitHistory({ split }: { split: PrismSplit }) {
  return (
    <div className="space-y-1 text-xs text-muted">
      <div className="flex flex-wrap gap-x-3 gap-y-1">
        {split.forwarded_msat > 0 && <span className="text-ok">{sats(split.forwarded_msat)} sats forwarded</span>}
        {split.pending_count > 0 && (
          <span>
            {sats(split.pending_msat)} sats on their way ({split.pending_count})
          </span>
        )}
        {split.failed_count > 0 && (
          <span className="text-danger">
            {sats(split.failed_msat)} sats not sent ({split.failed_count})
          </span>
        )}
        {split.skipped_msat > 0 && <span>{sats(split.skipped_msat)} sats under 1 sat, skipped</span>}
      </div>
      {split.last_error && <p className="break-words text-danger/80">Last error: {split.last_error}</p>}
    </div>
  );
}

/**
 * The prism of an address: percentages of every payment forwarded to other Lightning
 * targets, with the rest staying in this address's wallet. Past payouts keep their state,
 * so a split that could not be sent shows its amount and can be retried.
 */
export function PrismSplits({ a, onDomains }: { a: DomainAddress; onDomains: (v: Domains) => void }) {
  const saved = a.prism?.splits ?? [];
  const stats = a.prism?.stats;
  const savedSig = saved.map((s) => `${s.destination}:${s.percent_bps}`).join("|");
  // Keys for new rows: never reused, so a row keeps its state while others change.
  const [nextKey, setNextKey] = useState(saved.length + 1);
  const [rows, setRows] = useState<Row[]>(() =>
    saved.map((s, i) => ({ key: i, destination: s.destination, percent: fromBps(s.percent_bps) })),
  );
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<null | "save" | "retry">(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<string | null>(null);

  // Splits changed elsewhere (another tab, or a save): follow them, unless the owner is in
  // the middle of editing. Adjusted while rendering, so no extra render is needed.
  const [seenSig, setSeenSig] = useState(savedSig);
  if (seenSig !== savedSig) {
    setSeenSig(savedSig);
    if (!dirty) {
      setRows(saved.map((s, i) => ({ key: i, destination: s.destination, percent: fromBps(s.percent_bps) })));
      setNextKey((n) => Math.max(n, saved.length + 1));
    }
  }

  const draft = check(rows);
  const remainderBps = Math.max(0, 10_000 - draft.totalBps);
  const unchanged =
    !dirty &&
    draft.splits.length === saved.length &&
    draft.splits.every((s, i) => {
      const was = saved[i];
      return was !== undefined && s.destination === was.destination && s.percent_bps === was.percent_bps;
    });
  const ready = draft.errors.every((e) => e === null);

  const edit = (key: number, patch: Partial<Row>) => {
    setDirty(true);
    setNote(null);
    setRows((rs) => rs.map((r) => (r.key === key ? { ...r, ...patch } : r)));
  };

  const add = () => {
    setDirty(true);
    setNote(null);
    setRows((rs) => [...rs, { key: nextKey, destination: "", percent: "" }]);
    setNextKey((n) => n + 1);
  };

  const remove = (key: number) => {
    setDirty(true);
    setNote(null);
    setRows((rs) => rs.filter((r) => r.key !== key));
  };

  const save = async () => {
    setBusy("save");
    setError(null);
    setNote(null);
    try {
      onDomains(await domainsApi.setPrism(a.id, draft.splits));
      setDirty(false);
      setNote(draft.splits.length ? "Splits saved. The next payment is divided by them." : "Splits removed; everything stays in this wallet again.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };

  const retry = async () => {
    setBusy("retry");
    setError(null);
    setNote(null);
    try {
      onDomains(await domainsApi.retryPrism(a.id));
      setNote("Sending the failed splits again.");
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };

  const hasSplits = saved.length > 0 || rows.length > 0;

  return (
    <section className="space-y-3 rounded-xl border border-line bg-surface-2/40 p-4">
      <div className="flex items-start gap-3">
        <PrismLogo className="mt-0.5 size-8" />
        <div className="space-y-1">
          <h3 className="flex flex-wrap items-center gap-2 font-medium">
            Prism
            {saved.length > 0 && (
              <Badge tone="accent">
                {fromBps(saved.reduce((n, s) => n + s.percent_bps, 0))}% split
              </Badge>
            )}
            {stats && stats.failed_count > 0 && (
              <Badge tone="danger">
                <Warning size={12} weight="fill" /> {sats(stats.failed_msat)} sats not sent
              </Badge>
            )}
          </h3>
          <p className="text-xs text-muted">
            Send part of every payment this address receives on to other wallets. Percentages need not add
            up to 100%: whatever is left stays here. A split under 1 sat is skipped.{" "}
            <a
              href={PRISM_ARTICLE}
              target="_blank"
              rel="noreferrer"
              className="inline-flex items-center gap-1 underline hover:text-fg"
            >
              What is a prism? <ArrowSquareOut size={12} />
            </a>
          </p>
        </div>
      </div>

      {hasSplits && (
        <div className="space-y-1.5">
          {/* Where a payment goes at a glance: one segment per split, then the remainder. */}
          <div className="flex h-3 w-full overflow-hidden rounded-full border border-line bg-surface">
            {draft.splits.map((s, i) => (
              <div
                key={`${s.destination}-${i}`}
                className={cn("h-full", BEAMS[i % BEAMS.length])}
                style={{ width: `${s.percent_bps / 100}%` }}
                title={`${s.destination}: ${fromBps(s.percent_bps)}%`}
              />
            ))}
            <div className="h-full flex-1 bg-surface" title={`Stays here: ${fromBps(remainderBps)}%`} />
          </div>
          <div className="flex flex-wrap items-center justify-between gap-2 text-xs text-muted">
            <span>
              {fromBps(draft.totalBps)}% forwarded{draft.splits.length ? ` in ${draft.splits.length} split${draft.splits.length === 1 ? "" : "s"}` : ""}
            </span>
            <span className={cn(draft.totalBps > 10_000 && "font-medium text-danger")}>
              {fromBps(remainderBps)}% stays in this wallet
            </span>
          </div>
        </div>
      )}

      <div className="space-y-2">
        {rows.map((r, i) => {
          const typed = kindOf(r.destination);
          const history = typed ? splitStats(saved, r.destination) : undefined;
          // What the server found when it saved a plain address ("BIP-353" or "LNURL");
          // `address` means it has not checked this one yet.
          const kind =
            typed === "address" && history && history.kind !== "address" ? history.kind : typed;
          const err = draft.errors[i];
          const bps = toBps(r.percent);
          const max = Math.max(0, Math.min(100, 100 - (draft.totalBps - (bps ?? 0)) / 100));
          return (
            <div key={r.key} className="space-y-1.5 rounded-lg border border-line bg-surface p-3">
              <div className="flex flex-wrap items-start gap-2">
                <div className="min-w-48 flex-1">
                  <Field label="Sends to">
                    <Input
                      value={r.destination}
                      placeholder="you@wallet.com, you@domain or lno1…"
                      spellCheck={false}
                      autoCapitalize="none"
                      autoComplete="off"
                      className={cn("font-mono text-xs", err && "border-danger")}
                      onChange={(e) => edit(r.key, { destination: e.target.value })}
                      aria-label={`Destination of split ${i + 1}`}
                    />
                  </Field>
                </div>
                <div className="w-24">
                  <Field label="Share">
                    <div className="relative">
                      <Input
                        value={r.percent}
                        inputMode="decimal"
                        placeholder="0"
                        className={cn("pr-6 text-right font-mono", err && "border-danger")}
                        onChange={(e) => edit(r.key, { percent: e.target.value })}
                        aria-label={`Percentage of split ${i + 1}`}
                      />
                      <span className="pointer-events-none absolute inset-y-0 right-2 flex items-center text-xs text-muted">%</span>
                    </div>
                  </Field>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="mt-6 text-danger hover:bg-danger/10"
                  onClick={() => remove(r.key)}
                  aria-label={`Remove split ${i + 1}`}
                  title="Remove this split"
                >
                  <Trash size={16} />
                </Button>
              </div>
              <div className="flex items-center gap-2">
                <input
                  type="range"
                  min={0}
                  max={max}
                  step={0.01}
                  value={Math.min(Number(r.percent) || 0, max)}
                  onChange={(e) => edit(r.key, { percent: e.target.value })}
                  className="h-1 flex-1 accent-[var(--v-accent)]"
                  aria-label={`Adjust split ${i + 1}`}
                />
                <span className={cn("h-2.5 w-2.5 shrink-0 rounded-full", BEAMS[i % BEAMS.length])} aria-hidden />
                {kind ? (
                  <Badge tone="neutral">{KIND_LABEL[kind]}</Badge>
                ) : (
                  <span className="text-xs text-muted">not recognised</span>
                )}
              </div>
              {err && <p className="text-xs text-danger">{err}</p>}
              {history && <SplitHistory split={history} />}
            </div>
          );
        })}
        {rows.length === 0 && (
          <p className="rounded-lg border border-dashed border-line p-4 text-center text-xs text-muted">
            Nothing is split: every payment stays in this wallet.
          </p>
        )}
        <Button variant="secondary" size="sm" onClick={add} disabled={rows.length >= 32}>
          <Plus size={16} /> Add a split
        </Button>
      </div>

      {stats && (stats.failed_count > 0 || stats.pending_count > 0 || stats.forwarded_msat > 0) && (
        <div className="space-y-2 rounded-lg border border-line bg-surface p-3">
          <div className="flex flex-wrap items-center justify-between gap-2 text-sm">
            <span className="flex items-center gap-2">
              <PrismLogo className="size-5" />
              {stats.failed_count > 0 ? (
                <span>
                  <span className="font-medium text-danger">{sats(stats.failed_msat)} sats</span> could not be
                  sent, in {stats.failed_count} {stats.failed_count === 1 ? "payout" : "payouts"}.
                </span>
              ) : (
                <span>{sats(stats.pending_msat)} sats on their way to their splits.</span>
              )}
            </span>
            {stats.failed_count > 0 && (
              <Button variant="secondary" size="sm" disabled={busy !== null} onClick={() => void retry()}>
                <ArrowsClockwise size={16} className={cn(busy === "retry" && "animate-spin")} /> Retry now
              </Button>
            )}
          </div>
          {stats.failed_count > 0 && (
            <p className="text-xs text-muted">
              Allod tries again on its own with a growing delay, up to eight times. After that the amount stays
              here, recorded, until you retry it.
            </p>
          )}
          {stats.forwarded_msat > 0 && (
            <p className="text-xs text-muted">
              {sats(stats.forwarded_msat)} sats forwarded so far, over {stats.payments}{" "}
              {stats.payments === 1 ? "payment" : "payments"}.
            </p>
          )}
        </div>
      )}

      {error && <Alert tone="danger" title="Could not save the splits">{error}</Alert>}
      {note && <Alert tone="info">{note}</Alert>}

      <div className="flex flex-wrap items-center gap-2 border-t border-line pt-3">
        <Button size="sm" disabled={busy !== null || unchanged || !ready} onClick={() => void save()}>
          {busy === "save" ? "Saving…" : "Save splits"}
        </Button>
        {dirty && (
          <Button
            variant="ghost"
            size="sm"
            disabled={busy !== null}
            onClick={() => {
              setDirty(false);
              setNote(null);
              setRows(saved.map((s, i) => ({ key: i, destination: s.destination, percent: fromBps(s.percent_bps) })));
            }}
          >
            Discard
          </Button>
        )}
        <span className="ml-auto text-xs text-muted">
          {saved.length === 0 ? "Nothing is forwarded yet." : "Saved splits apply from the next payment."}
        </span>
      </div>
    </section>
  );
}
