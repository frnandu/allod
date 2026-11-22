import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { NetworkMap } from "./NetworkMap";
import type { GossipStatus } from "../lib/api";

const sidecarStatus: GossipStatus = {
  source: "sidecar",
  state: "active",
  channels: 27950,
  nodes: 9700,
  sidecar: {
    state: "ready",
    ready: true,
    started_at: 1,
    initial_sync_secs: 21,
    peers_configured: 4,
    peers_connected: 4,
    graph_channels: 28906,
    graph_nodes: 9774,
    verify: {
      enabled: true,
      scans_completed: 1,
      scanning: false,
      last_scan_finished_at: Math.floor(Date.now() / 1000) - 30,
      last_scan_height: 920000,
      last_scan_secs: 95,
      unspent_channels: 27950,
      missing_channels: 956,
      pending_channels: 0,
      unspent_capacity_sats: 450000000000,
    },
    snapshot: { served: 1, last_served_at: Math.floor(Date.now() / 1000) - 60, channels: 27950, channels_with_capacity: 27950, last_bytes: 2_200_000 },
  },
};

afterEach(cleanup);

describe("NetworkMap", () => {
  it("leads with what the node uses, and keeps the sidecar's numbers in the details", () => {
    render(<NetworkMap status={sidecarStatus} />);
    expect(screen.getByText("27,950")).toBeTruthy();
    expect(screen.getByText("9,700")).toBeTruthy();
    expect(screen.getByText("Up to date")).toBeTruthy();
    const details = screen.getByText("Details").closest("details")!;
    expect(details.open).toBe(false);
    expect(details.textContent).toContain("28,906");
    expect(details.textContent).toContain("4 of 4");
    expect(details.textContent).toContain("21 s");
    expect(details.textContent).toContain("956 closed or never");
    expect(details.textContent).toContain("Last copy handed to the node");
    expect(screen.queryByText(/in node/)).toBeNull();
  });

  it("explains the node's own gossip when no sidecar is configured", () => {
    render(<NetworkMap status={{ source: "p2p", state: "learning", channels: 5, nodes: 3 }} />);
    expect(screen.getByText("Learning")).toBeTruthy();
    expect(screen.getByLabelText("Learning the network map")).toBeTruthy();
    expect(screen.getByText(/collects the map itself/)).toBeTruthy();
  });
});
