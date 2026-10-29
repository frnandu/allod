import { sats } from "../lib/format";
import type { Channel } from "../lib/api";
import { Alert } from "./ui/alert";

/** LDK's default on-chain reserve for each anchor channel. */
export const ANCHOR_RESERVE_PER_CHANNEL_SAT = 25_000;

/**
 * Warns when the on-chain balance does not cover the recommended anchor reserve.
 * Allod configures ldk-server with a zero reserve so inbound channels work without an
 * on-chain deposit, but an urgent close may then be stuck until the wallet is funded.
 */
export function AnchorReserveWarning({
  channels,
  spendableSat,
}: {
  channels: Channel[];
  spendableSat: number;
}) {
  const required = channels.length * ANCHOR_RESERVE_PER_CHANNEL_SAT;
  if (channels.length === 0 || required <= spendableSat) return null;
  return (
    <Alert tone="warn" title="Low on-chain reserve for anchor channels">
      Your {channels.length} channel{channels.length === 1 ? "" : "s"} would need{" "}
      {sats(required * 1000)} sats on-chain to keep the recommended{" "}
      {sats(ANCHOR_RESERVE_PER_CHANNEL_SAT * 1000)} sats per channel, but only{" "}
      {sats(spendableSat * 1000)} sats are spendable. If a channel has to be force-closed,
      the node may not be able to raise the closing fee, and your funds stay locked until
      you add on-chain sats. This does not stop channels from opening or being used.
    </Alert>
  );
}
