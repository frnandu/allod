import { describe, expect, it } from "vitest";
import { _reloadProgress, _resetProgress, etaSeconds, formatAgo, formatEta, recordProgress } from "./eta";

describe("eta", () => {
  it("estimates from the recent rate and resets when progress goes backwards", () => {
    _resetProgress();
    recordProgress("dl", 0, 120_000, 0);
    expect(etaSeconds("dl", 100)).toBeNull();
    recordProgress("dl", 10, 120_000, 10_000);
    expect(etaSeconds("dl", 100)).toBeNull(); // under 20 s of data
    recordProgress("dl", 30, 120_000, 30_000); // 1 per second
    expect(etaSeconds("dl", 100)).toBeCloseTo(70);
    recordProgress("dl", 5, 120_000, 40_000); // restarted
    expect(etaSeconds("dl", 100)).toBeNull();
    // Old samples leave the window: the estimate follows the current speed.
    recordProgress("x", 0, 60_000, 0);
    recordProgress("x", 100, 60_000, 100_000); // slow start
    recordProgress("x", 1100, 60_000, 200_000);
    recordProgress("x", 2100, 60_000, 300_000); // 10 per second now
    expect(etaSeconds("x", 3100)).toBeCloseTo(100);
  });

  it("starts over when a snapshot moves the height at once", () => {
    _resetProgress();
    // Full sync paused during the snapshot download: the height stands still…
    recordProgress("btc", 210_000, 600_000, 0);
    recordProgress("btc", 210_000, 600_000, 30_000);
    // …then the snapshot is loaded and the node syncs from 935,000 at ~1 block/s.
    recordProgress("btc", 935_131, 600_000, 35_000);
    expect(etaSeconds("btc", 973_583)).toBeNull(); // estimating, not "less than a minute"
    recordProgress("btc", 935_141, 600_000, 45_000);
    recordProgress("btc", 935_161, 600_000, 65_000);
    expect(etaSeconds("btc", 973_583)).toBeCloseTo(38_422, -1); // about 10.7 h at 1 block/s
    // A burst far above the trend also starts over.
    recordProgress("btc", 945_000, 600_000, 70_000);
    expect(etaSeconds("btc", 973_583)).toBeNull();
  });

  it("keeps the estimate through a stall and resumes without starting over", () => {
    _resetProgress();
    for (let i = 0; i <= 12; i++) recordProgress("b", 1000 + i * 5, 600_000, i * 5_000); // 1 block/s
    expect(etaSeconds("b", 2060)).toBeCloseTo(1000);
    // A cache flush: no block for 10 minutes, the window goes flat.
    for (let t = 65_000; t <= 665_000; t += 5_000) recordProgress("b", 1060, 600_000, t);
    expect(etaSeconds("b", 2060)).toBeCloseTo(1000); // the rate from before the stall
    // Blocks again at the old pace: no reset, the estimate stays on screen.
    recordProgress("b", 1065, 600_000, 670_000);
    expect(etaSeconds("b", 2060)).not.toBeNull();
  });

  it("ignores repeated readings and small dips of Bitcoin Core's estimate", () => {
    _resetProgress();
    recordProgress("vp", 0.9155, 600_000, 0);
    recordProgress("vp", 0.9156, 600_000, 30_000);
    const eta = etaSeconds("vp", 1);
    // The same cached answer again: not a new sample.
    recordProgress("vp", 0.9156, 600_000, 30_000);
    // The estimate shrinks a little while no block arrives: jitter, not a restart.
    recordProgress("vp", 0.91559, 600_000, 35_000);
    expect(etaSeconds("vp", 1)).toBeCloseTo(eta!);
    // A real restart still starts over.
    recordProgress("vp", 0.2, 600_000, 40_000);
    expect(etaSeconds("vp", 1)).toBeNull();
  });

  it("survives a page reload", () => {
    _resetProgress();
    const now = Date.now();
    recordProgress("r", 0, 120_000, now - 30_000);
    recordProgress("r", 30, 120_000, now);
    _reloadProgress();
    expect(etaSeconds("r", 100)).toBeCloseTo(70);
  });

  it("formats", () => {
    expect(formatAgo(42)).toBe("42 s");
    expect(formatAgo(180)).toBe("3 min");
    expect(formatEta(null)).toBe("estimating…");
    expect(formatEta(30)).toBe("less than a minute left");
    expect(formatEta(12 * 60)).toBe("about 12 min left");
    expect(formatEta(2 * 3600 + 20 * 60)).toBe("about 2 h 20 min left");
    expect(formatEta(30 * 3600)).toBe("about 30 h left");
    expect(formatEta(5 * 86400)).toBe("about 5 days left");
  });
});
