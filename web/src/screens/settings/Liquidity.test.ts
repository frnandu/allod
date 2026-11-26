import { describe, expect, it } from "vitest";
import type { LiquiditySettings } from "../../lib/api";
import { restartPhase } from "./Liquidity";

const LSP = `02${"ab".repeat(32)}@203.0.113.9:9735`;
const base = (over: Partial<LiquiditySettings>): LiquiditySettings => ({
  available: true,
  enabled: true,
  lsps: [LSP],
  trust_0conf: false,
  sources: [],
  saved_at: 1000,
  active: { lsps: [LSP], trust_0conf: false, started_at: 900, request: "applied" },
  applied: false,
  ...over,
});

describe("restartPhase", () => {
  it("follows a change: handed over, restarted, up, running", () => {
    // Started before the change: still to restart, then stalled after 3 minutes.
    expect(restartPhase(base({}), true, 1010).kind).toBe("waiting");
    expect(restartPhase(base({}), true, 1200).kind).toBe("stalled");
    // Restarted with it, not answering yet; then up.
    const restarted = base({ applied: true, active: { lsps: [LSP], trust_0conf: false, started_at: 1005, request: "applied" } });
    expect(restartPhase(restarted, false, 1010).kind).toBe("starting");
    expect(restartPhase(restarted, true, 1010)).toEqual({ kind: "done", at: 1005 });
    // Long ago: nothing to show.
    expect(restartPhase(restarted, true, 1000 + 3600).kind).toBe("idle");
  });

  it("says why the node did not take the change", () => {
    const unreadable = base({ active: { lsps: [], trust_0conf: false, started_at: 1005, request: "unreadable", request_error: "ldk-server cannot read /handoff/lsps2.json (owner or mode)" } });
    expect(restartPhase(unreadable, true, 1010)).toEqual({ kind: "failed", reason: "ldk-server cannot read /handoff/lsps2.json (owner or mode)" });
    const other = base({ active: { lsps: [], trust_0conf: false, started_at: 1005, request: "applied" }, applied: false });
    expect(restartPhase(other, true, 1010).kind).toBe("failed");
  });

  it("follows an older Lightning node that does not date its start", () => {
    const old = (applied: boolean) => base({ applied, active: { lsps: [LSP], trust_0conf: false } });
    expect(restartPhase(old(false), true, 1010).kind).toBe("waiting");
    expect(restartPhase(old(false), true, 1200).kind).toBe("stalled");
    expect(restartPhase(old(true), false, 1010).kind).toBe("starting");
    expect(restartPhase(old(true), true, 1010)).toEqual({ kind: "done", at: 1000 });
  });

  it("shows nothing without a change or a report", () => {
    expect(restartPhase(base({ saved_at: null }), true, 1010).kind).toBe("idle");
    expect(restartPhase(base({ active: null }), true, 1010).kind).toBe("idle");
  });
});
