import { useState } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import { fetcher, startStandbyBitcoind, type ChainSourceSettings } from "../lib/api";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";

const KEY = "/api/settings/chain-source";

/**
 * Shown on every page when the external Bitcoin node the Lightning node reads from has
 * stopped answering and the stack ships a built-in node to replace it (SPEC §2.3.2).
 */
export function StandbyBitcoindBanner() {
  const { data, mutate } = useSWR<ChainSourceSettings>(KEY, fetcher, { refreshInterval: 30_000 });
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const standby = data?.standby;
  if (!standby?.suggested) return null;

  const since = standby.outage_since
    ? new Date(standby.outage_since * 1000).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })
    : null;

  async function start() {
    setErr(null);
    setBusy(true);
    try {
      await mutate(await startStandbyBitcoind(), { revalidate: false });
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Alert tone="danger" title="Your Bitcoin node is not answering" className="mb-6">
      <div className="space-y-3">
        <p>
          Your Lightning node reads the blockchain from an external Bitcoin node that has not answered
          {since ? ` since ${since}` : ""}. If you removed it (for example Umbrel&apos;s Bitcoin Node app),
          start Allod&apos;s built-in node. Until the Lightning node can read the chain again, it cannot pay, receive
          or watch your channels.
        </p>
        <p className="text-xs">
          The built-in node is pruned and has to download the blockchain first: hours to a few days. The Lightning
          node stays offline until it finishes. To get going right away, pick a public server in Settings instead.
          {standby.outage_error ? ` Last error: ${standby.outage_error}` : ""}
        </p>
        {err && <p className="font-medium">Could not start it: {err}</p>}
        <div className="flex flex-wrap items-center gap-3">
          <Button size="sm" disabled={busy} onClick={() => void start()}>
            Start built-in node
          </Button>
          <Link to="/settings/bitcoin" className="text-sm font-medium text-accent underline">
            Other chain sources
          </Link>
        </div>
      </div>
    </Alert>
  );
}
