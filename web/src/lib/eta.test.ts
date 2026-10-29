import { describe, expect, it } from "vitest";
import { _resetProgress, etaSeconds, formatEta, recordProgress } from "./eta";

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

  it("formats", () => {
    expect(formatEta(null)).toBe("estimating…");
    expect(formatEta(30)).toBe("less than a minute left");
    expect(formatEta(12 * 60)).toBe("about 12 min left");
    expect(formatEta(2 * 3600 + 20 * 60)).toBe("about 2 h 20 min left");
    expect(formatEta(30 * 3600)).toBe("about 30 h left");
    expect(formatEta(5 * 86400)).toBe("about 5 days left");
  });
});
