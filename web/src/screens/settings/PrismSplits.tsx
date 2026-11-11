import {
  ArrowsClockwise,
  ArrowSquareOut,
  CaretRight,
  ClockCounterClockwise,
  PencilSimple,
  Plus,
  Trash,
  Warning,
} from "@phosphor-icons/react";
import { useState } from "react";
import { PrismLogo } from "../../components/PrismLogo";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  domains as domainsApi,
  type DomainAddress,
  type Domains,
  type PrismPayout,
  type PrismSplit,
} from "../../lib/api";
import { ago, dateTime, sats } from "../../lib/format";
import { cn } from "../../lib/utils";

/** The prism article: background on what splitting a payment at the sender's wallet means. */
const PRISM_ARTICLE = "https://dergigi.com/2023/03/12/lightning-prisms/";

/** One split being edited. `key` only tells rows apart in React. */
interface Row {
  key: number;
  /** The stored split this row edits; absent on a row that is not saved yet. */
  id?: number;
  destination: string;
  /** The owner's own note: what or who this destination is. */
  note: string;
  /** Whether the note field is open; the note itself stays hidden otherwise. */
  noteOpen?: boolean;
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
  splits: { id?: number; destination: string; percent_bps: number; note: string }[];
  errors: (string | null)[];
  /** Sum of the valid rows, in basis points. */
  totalBps: number;
}

/** Checks the rows as typed; `splits` holds only the rows that are ready to save. */
function check(rows: Row[]): Draft {
  const errors: (string | null)[] = [];
  const splits: { id?: number; destination: string; percent_bps: number; note: string }[] = [];
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
    splits.push({ id: r.id, destination, percent_bps: bps, note: r.note.trim() });
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

/** Saved splits as editable rows. */
function toRows(saved: PrismSplit[]): Row[] {
  return saved.map((s, i) => ({
    key: i,
    id: s.id,
    destination: s.destination,
    note: s.note ?? "",
    noteOpen: false,
    percent: fromBps(s.percent_bps),
  }));
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

/** How one payout ended, for its badge. */
const PAYOUT_TONE = {
  PAID: "ok",
  FAILED: "danger",
  PENDING: "warn",
  PAYING: "warn",
  CANCELED: "neutral",
  SKIPPED: "neutral",
} as const;

/** One line of the payout history: what a split did with one received payment. */
function PayoutRow({ p }: { p: PrismPayout }) {
  return (
    <div className="flex items-start justify-between gap-3 border-b border-line/60 py-1.5 text-xs last:border-0">
      <div className="min-w-0 space-y-0.5">
        <div className="flex flex-wrap items-center gap-2">
          <Badge tone={PAYOUT_TONE[p.state]}>{p.state.toLowerCase()}</Badge>
          <span className="font-medium">{sats(p.amount_msat)} sats</span>
          {p.fee_msat > 0 && <span className="text-muted">fee {sats(p.fee_msat)}</span>}
          <span className="text-muted">{fromBps(p.percent_bps)}%</span>
        </div>
        <p className="truncate text-muted" title={p.destination}>
          {p.note ? `${p.note} · ${p.destination}` : p.destination}
        </p>
        {p.state === "FAILED" && p.last_error && (
          <p className="break-words text-danger/80">{p.last_error}</p>
        )}
      </div>
      <div className="shrink-0 text-right text-muted" title={dateTime(p.created_at)}>
        <div>{ago(p.created_at)}</div>
        {p.attempts > 0 && (
          <div>
            {p.attempts} {p.attempts === 1 ? "try" : "tries"}
          </div>
        )}
      </div>
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
  const savedSig = saved.map((s) => `${s.destination}:${s.percent_bps}:${s.note ?? ""}`).join("|");
  // Keys for new rows: never reused, so a row keeps its state while others change.
  const [nextKey, setNextKey] = useState(saved.length + 1);
  const [rows, setRows] = useState<Row[]>(() => toRows(saved));
  const [dirty, setDirty] = useState(false);
  const [busy, setBusy] = useState<null | "save" | "retry">(null);
  const [error, setError] = useState<string | null>(null);
  const [flash, setFlash] = useState<string | null>(null);
  // Clearing a split's failed total asks once: it is never paid, only forgotten.
  const [confirmClear, setConfirmClear] = useState<number | null>(null);
  const [clearing, setClearing] = useState<number | null>(null);
  const [historyOpen, setHistoryOpen] = useState(false);
  const [payouts, setPayouts] = useState<PrismPayout[]>([]);
  const [historyNext, setHistoryNext] = useState<number | null>(null);
  const [historyBusy, setHistoryBusy] = useState(false);
  const [historyError, setHistoryError] = useState<string | null>(null);

  // Splits changed elsewhere (another tab, or a save): follow them, unless the owner is in
  // the middle of editing. Adjusted while rendering, so no extra render is needed.
  const [seenSig, setSeenSig] = useState(savedSig);
  if (seenSig !== savedSig) {
    setSeenSig(savedSig);
    if (!dirty) {
      setRows(toRows(saved));
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
      return (
        was !== undefined &&
        s.destination === was.destination &&
        s.percent_bps === was.percent_bps &&
        s.note === (was.note ?? "")
      );
    });
  const ready = draft.errors.every((e) => e === null);

  const edit = (key: number, patch: Partial<Row>) => {
    setDirty(true);
    setFlash(null);
    setRows((rs) => rs.map((r) => (r.key === key ? { ...r, ...patch } : r)));
  };

  const add = () => {
    setDirty(true);
    setFlash(null);
    setRows((rs) => [...rs, { key: nextKey, destination: "", note: "", percent: "" }]);
    setNextKey((n) => n + 1);
  };

  const remove = (key: number) => {
    setDirty(true);
    setFlash(null);
    setRows((rs) => rs.filter((r) => r.key !== key));
  };

  // The note stays out of the way: a line of muted text when set, a small button
  // otherwise. Opening it does not make the splits dirty.
  const openNote = (key: number, open: boolean) => {
    setRows((rs) => rs.map((r) => (r.key === key ? { ...r, noteOpen: open } : r)));
  };

  const save = async () => {
    setBusy("save");
    setError(null);
    setFlash(null);
    try {
      onDomains(await domainsApi.setPrism(a.id, draft.splits));
      setDirty(false);
      setFlash(draft.splits.length ? "Splits saved. The next payment is divided by them." : "Splits removed; everything stays in this wallet again.");
      if (historyOpen) void loadHistory();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };

  const retry = async () => {
    setBusy("retry");
    setError(null);
    setFlash(null);
    try {
      onDomains(await domainsApi.retryPrism(a.id));
      setFlash("Sending the failed splits again.");
      if (historyOpen) void loadHistory();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(null);
    }
  };

  // Payout history is loaded only when the owner opens it, and a page at a time.
  const loadHistory = async (before?: number | null) => {
    setHistoryBusy(true);
    setHistoryError(null);
    try {
      const page = await domainsApi.payoutHistory(a.id, before);
      setPayouts((h) => (before ? [...h, ...page.payouts] : page.payouts));
      setHistoryNext(page.next_before_id);
    } catch (e) {
      setHistoryError(message(e));
    } finally {
      setHistoryBusy(false);
    }
  };

  const toggleHistory = () => {
    const open = !historyOpen;
    setHistoryOpen(open);
    if (open) void loadHistory();
  };

  // A split that never sent its share keeps the amount recorded. Clearing it does not
  // pay anything: those payouts stop counting as failed and are not retried again.
  const clearFailed = async (splitId: number) => {
    setClearing(splitId);
    setError(null);
    setFlash(null);
    try {
      onDomains(await domainsApi.resetPrismFailed(a.id, splitId));
      setConfirmClear(null);
      setFlash("The failed amount of this split is cleared; nothing was sent.");
      if (historyOpen) void loadHistory();
    } catch (e) {
      setError(message(e));
    } finally {
      setClearing(null);
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
                title={`${s.destination}${s.note ? ` (${s.note})` : ""}: ${fromBps(s.percent_bps)}%`}
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
              {r.noteOpen ? (
                <Input
                  autoFocus
                  value={r.note}
                  placeholder="Only you see this: whose wallet or what it pays"
                  maxLength={200}
                  autoComplete="off"
                  className="h-7 text-xs"
                  onChange={(e) => edit(r.key, { note: e.target.value })}
                  onBlur={() => openNote(r.key, false)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") openNote(r.key, false);
                  }}
                  aria-label={`Note for split ${i + 1}`}
                />
              ) : r.note.trim() ? (
                <button
                  type="button"
                  className="flex min-w-0 items-center gap-1 text-left text-[11px] text-muted hover:text-fg"
                  title={r.note}
                  aria-label={`Edit note for split ${i + 1}`}
                  onClick={() => openNote(r.key, true)}
                >
                  <PencilSimple size={11} className="shrink-0 opacity-60" aria-hidden />
                  <span className="truncate">{r.note}</span>
                </button>
              ) : (
                <button
                  type="button"
                  className="inline-flex items-center gap-1 text-[11px] text-muted/70 hover:text-fg"
                  aria-label={`Add note for split ${i + 1}`}
                  onClick={() => openNote(r.key, true)}
                >
                  <PencilSimple size={11} /> Note
                </button>
              )}
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
              {history && history.failed_count > 0 && (
                <div className="flex flex-wrap items-center gap-1">
                  {confirmClear === history.id ? (
                    <>
                      <span className="text-xs text-muted">
                        Clear {sats(history.failed_msat)} sats?
                      </span>
                      <Button
                        variant="ghost"
                        size="sm"
                        className="text-danger hover:bg-danger/10"
                        disabled={clearing !== null}
                        onClick={() => void clearFailed(history.id)}
                      >
                        {clearing === history.id ? "Clearing…" : "Yes, clear"}
                      </Button>
                      <Button
                        variant="ghost"
                        size="sm"
                        disabled={clearing !== null}
                        onClick={() => setConfirmClear(null)}
                      >
                        Cancel
                      </Button>
                    </>
                  ) : (
                    <Button
                      variant="ghost"
                      size="sm"
                      className="text-danger hover:bg-danger/10"
                      title="Mark these failed payouts as cleared; nothing is sent"
                      disabled={busy !== null || clearing !== null}
                      onClick={() => setConfirmClear(history.id)}
                    >
                      Clear failed
                    </Button>
                  )}
                </div>
              )}
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

      <div className="rounded-lg border border-line bg-surface">
        <button
          type="button"
          className="flex w-full items-center justify-between gap-2 px-3 py-2 text-sm"
          onClick={toggleHistory}
          aria-expanded={historyOpen}
          aria-label="Payout history"
        >
          <span className="flex items-center gap-2">
            <ClockCounterClockwise size={16} className="text-muted" /> Payout history
          </span>
          <span className="flex items-center gap-2 text-xs text-muted">
            {historyOpen ? "Hide" : "Show"}
            <CaretRight
              size={14}
              className={cn("transition-transform", historyOpen && "rotate-90")}
              aria-hidden
            />
          </span>
        </button>
        {historyOpen && (
          <div className="space-y-2 border-t border-line px-3 py-2">
            {historyBusy && payouts.length === 0 ? (
              <p className="text-xs text-muted">Loading…</p>
            ) : payouts.length === 0 ? (
              <p className="text-xs text-muted">No payout recorded yet.</p>
            ) : (
              <div>
                {payouts.map((p) => (
                  <PayoutRow key={p.id} p={p} />
                ))}
              </div>
            )}
            {historyNext !== null && (
              <Button
                variant="ghost"
                size="sm"
                disabled={historyBusy}
                onClick={() => void loadHistory(historyNext)}
              >
                {historyBusy ? "Loading…" : "Show older"}
              </Button>
            )}
            {historyError && <p className="text-xs text-danger">{historyError}</p>}
          </div>
        )}
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
              Each failed payout keeps its own amount and destination; nothing is accumulated into one
              payment. Allod tries again on its own with a growing delay, up to eight times. After that the
              amount stays here, recorded, until you retry it. Editing where a split sends keeps these
              amounts with it; clearing them marks them canceled without sending anything.
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
      {flash && <Alert tone="info">{flash}</Alert>}

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
              setFlash(null);
              setRows(toRows(saved));
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
