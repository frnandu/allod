// Time-left estimates for progress rows: values are recorded as the status is polled
// (outside render), and the estimate is the remaining amount over the recent rate.
import { useSyncExternalStore } from "react";

type Sample = { t: number; v: number };
const series = new Map<string, { samples: Sample[]; windowMs: number }>();
const listeners = new Set<() => void>();
let version = 0;

const MIN_SPAN_MS = 20_000;

/** Records the current value of a progress metric. A value that goes backwards starts over. */
export function recordProgress(key: string, value: number, windowMs = 120_000, now = Date.now()) {
  const s = series.get(key) ?? { samples: [], windowMs };
  s.windowMs = windowMs;
  const last = s.samples[s.samples.length - 1];
  if (last && value < last.v) s.samples = [];
  else if (last && s.samples.length >= 2 && isJump(s.samples, value, now)) s.samples = [];
  s.samples.push({ t: now, v: value });
  while (s.samples.length > 2 && now - s.samples[0]!.t > windowMs) s.samples.shift();
  series.set(key, s);
  version++;
  listeners.forEach((l) => l());
}

/**
 * A step that does not continue the measured trend: far faster than the rate so far
 * (e.g. a snapshot moving the block height from 210,000 to 935,000 at once), or any
 * movement after a stall. The estimate then starts over from the new value.
 */
function isJump(samples: Sample[], value: number, now: number): boolean {
  const first = samples[0]!;
  const last = samples[samples.length - 1]!;
  const dv = value - last.v;
  if (dv <= 0 || now <= last.t) return false;
  const span = last.t - first.t;
  const rate = span > 0 ? (last.v - first.v) / span : 0;
  if (rate <= 0) return true;
  return dv / (now - last.t) > rate * 20;
}

/** Seconds until `target`, or null while there is not enough data (or no progress). */
export function etaSeconds(key: string, target: number): number | null {
  const s = series.get(key)?.samples;
  if (!s || s.length < 2) return null;
  const first = s[0]!;
  const last = s[s.length - 1]!;
  const span = last.t - first.t;
  if (span < MIN_SPAN_MS) return null;
  const rate = (last.v - first.v) / (span / 1000);
  if (rate <= 0) return null;
  return Math.max(0, (target - last.v) / rate);
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

/** Test helper. */
export function _resetProgress() {
  series.clear();
}
