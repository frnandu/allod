import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { AnchorReserveWarning } from "./AnchorReserveWarning";
import type { Channel } from "../lib/api";

function channel(id: number): Channel {
  return {
    channel_id: `channel-${id}`,
    user_channel_id: String(id),
    counterparty_node_id: "02ab".repeat(16),
    capacity_sat: 1_000_000,
    outbound_capacity_msat: 500_000_000,
    inbound_capacity_msat: 500_000_000,
    is_outbound: true,
    is_channel_ready: true,
    is_usable: true,
    is_announced: false,
  };
}

afterEach(cleanup);

describe("AnchorReserveWarning", () => {
  it("warns when the on-chain balance does not cover 25k per channel", () => {
    render(<AnchorReserveWarning channels={[channel(1), channel(2)]} spendableSat={49_999} />);
    expect(screen.getByText("Low on-chain reserve for anchor channels")).toBeTruthy();
    expect(screen.getByText(/only 49,999 sats are spendable/)).toBeTruthy();
  });

  it("stays quiet when every channel has its reserve covered", () => {
    const { container } = render(
      <AnchorReserveWarning channels={[channel(1), channel(2)]} spendableSat={50_000} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it("stays quiet without channels", () => {
    const { container } = render(<AnchorReserveWarning channels={[]} spendableSat={0} />);
    expect(container.firstChild).toBeNull();
  });
});
