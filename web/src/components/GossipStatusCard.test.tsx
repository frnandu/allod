import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { GossipStatusCard } from "./GossipStatusCard";
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

describe("GossipStatusCard", () => {
  it("shows sidecar sync, funding check and snapshot numbers", () => {
    render(<GossipStatusCard status={sidecarStatus} />);
    expect(screen.getByText(/Local gossip sidecar/)).toBeTruthy();
    expect(screen.getByText("4/4")).toBeTruthy();
    expect(screen.getByText("28,906")).toBeTruthy();
    expect(screen.getByText(/synced in 21 s/)).toBeTruthy();
    expect(screen.getByText(/956 closed or unfunded/)).toBeTruthy();
    expect(screen.getByText(/The node fetches one per hour/)).toBeTruthy();
  });

  it("keeps the P2P view when no sidecar is configured", () => {
    render(<GossipStatusCard status={{ source: "p2p", state: "learning", channels: 5, nodes: 3 }} />);
    expect(screen.getByText(/P2P gossip · no third-party snapshot/)).toBeTruthy();
    expect(screen.getByText("Known channels")).toBeTruthy();
  });
});
