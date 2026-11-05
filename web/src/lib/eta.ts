// Time-left estimates for progress rows: values are recorded as the status is polled
// (outside render), and the estimate is the remaining amount over the recent rate.
// Samples survive a page reload (localStorage), so the estimate does not start over.
import { useSyncExternalStore } from "react";

type Sample = { t: number; v: number };
type Series = {
  samples: Sample[];
  windowMs: number;
  /** Last positive rate (per ms), used across stalls and while the window is flat. */
  rate?: number;
};
const series = new Map<string, Series>();
const listeners = new Set<() => void>();
let version = 0;

const MIN_SPAN_MS = 20_000;
/** A drop this small (relative) is jitter, not a restart: Bitcoin Core's verification
 * progress shrinks a little while no block arrives, as it is an estimate against the clock. */
const JITTER = 0.005;
/** After a stall with no measured rate, a step this large (relative) is a jump (e.g. a
 * snapshot replacing the height the node was paused at). */
const BIG_STEP = 0.01;
/** Saved samples older than this are not restored. */
const RESTORE_MAX_AGE_MS = 30 * 60_000;
const STORAGE_KEY = "allod.progress";

function restore() {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return;
    const now = Date.now();
    for (const [key, s] of Object.entries(JSON.parse(raw) as Record<string, Series>)) {
      const last = s.samples?.[s.samples.length - 1];
      if (last && now - last.t < RESTORE_MAX_AGE_MS) series.set(key, s);
    }
  } catch {
    // Storage unavailable or unreadable: start without history.
  }
}

function save() {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(Object.fromEntries(series)));
  } catch {
    // Storage unavailable or full: the estimate still works for this page.
  }
}

restore();

/** Rate (per ms) over the samples in the window; 0 when flat or too short. */
function windowRate(samples: Sample[]): number {
  if (samples.length < 2) return 0;
  const first = samples[0]!;
  const last = samples[samples.length - 1]!;
  const span = last.t - first.t;
  return span > 0 ? Math.max(0, (last.v - first.v) / span) : 0;
}

/**
 * Records the value of a progress metric at `now` (pass the time the value was measured
 * when it is known, e.g. when bitcoind answered). A reading that is not newer than the
 * last one is ignored, so a repeated cached answer does not look like a stall. A value
 * that goes clearly backwards starts over.
 */
export function recordProgress(key: string, value: number, windowMs = 120_000, now = Date.now()) {
  const s = series.get(key) ?? { samples: [], windowMs };
  s.windowMs = windowMs;
  const last = s.samples[s.samples.length - 1];
  if (last && now <= last.t) return;
  if (last && value < last.v) {
    if (last.v - value <= Math.abs(last.v) * JITTER) return;
    s.samples = [];
    s.rate = undefined;
  } else if (last && isJump(s, value, now)) {
    s.samples = [];
    s.rate = undefined;
  }
  const moved = !last || value > last.v;
  s.samples.push({ t: now, v: value });
  while (s.samples.length > 2 && now - s.samples[0]!.t > windowMs) s.samples.shift();
  // Remember the rate while progress is being made; a stall must not wear it down.
  const rate = windowRate(s.samples);
  if (moved && rate > 0 && now - s.samples[0]!.t >= MIN_SPAN_MS) s.rate = rate;
  series.set(key, s);
  save();
  version++;
  listeners.forEach((l) => l());
}

/**
 * A step that does not continue the measured trend: far faster than the rate so far
 * (e.g. a snapshot moving the block height from 210,000 to 935,000 at once). Progress
 * resuming after a stall (a cache flush, a busy node) is not a jump: it is compared with
 * the rate from before the stall.
 */
function isJump(s: Series, value: number, now: number): boolean {
  const last = s.samples[s.samples.length - 1]!;
  const dv = value - last.v;
  const dt = now - last.t;
  if (dv <= 0 || dt <= 0) return false;
  const rate = windowRate(s.samples) || s.rate || 0;
  if (rate > 0) return dv / dt > rate * 20;
  // No rate yet: only a large step out of a stall counts (a single sample is no trend).
  return s.samples.length >= 2 && dv > Math.abs(last.v) * BIG_STEP;
}

/**
 * Seconds until `target`, or null while there is not enough data. During a stall the
 * rate from before it is used, so the estimate stays on screen.
 */
export function etaSeconds(key: string, target: number): number | null {
  const s = series.get(key);
  if (!s || s.samples.length === 0) return null;
  const last = s.samples[s.samples.length - 1]!;
  const span = last.t - s.samples[0]!.t;
  let rate = span >= MIN_SPAN_MS ? windowRate(s.samples) : 0;
  if (rate <= 0) rate = s.rate ?? 0;
  if (rate <= 0) return null;
  return Math.max(0, (target - last.v) / rate / 1000);
}

/** Re-renders the caller when new samples arrive. */
export function useProgressSamples(): number {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    () => version,
  );
}

export function formatEta(seconds: number | null): string {
  if (seconds == null) return "estimating…";
  if (seconds < 60) return "less than a minute left";
  const m = Math.round(seconds / 60);
  if (m < 60) return `about ${m} min left`;
  const h = Math.floor(m / 60);
  if (h < 48) return `about ${h} h${m % 60 >= 5 && h < 10 ? ` ${m % 60} min` : ""} left`;
  return `about ${Math.round(h / 24)} days left`;
}

/** "40 s" / "3 min" / "2 h": how long ago a reading was taken. */
export function formatAgo(seconds: number): string {
  if (seconds < 60) return `${Math.max(0, Math.round(seconds))} s`;
  if (seconds < 3600) return `${Math.round(seconds / 60)} min`;
  return `${Math.round(seconds / 3600)} h`;
}

/** Test helper. */
export function _resetProgress() {
  series.clear();
  try {
    localStorage.removeItem(STORAGE_KEY);
  } catch {
    // ignore
  }
}

/** Test helper: reload the saved samples as a new page would. */
export function _reloadProgress() {
  series.clear();
  restore();
}
