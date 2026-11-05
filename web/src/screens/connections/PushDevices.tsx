import { useState } from "react";
import useSWR from "swr";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardHeader, CardTitle } from "../../components/ui/card";
import { Empty } from "../../components/ui/extras";
import { api, fetcher } from "../../lib/api";
import { ago } from "../../lib/format";
import type { PushInfo, PushSubscriptionView } from "../../lib/push";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/**
 * Devices a wallet app registered through this connection (NWC `register_push`,
 * SPEC §8.9). They receive the connection's notifications as push messages.
 */
export function PushDevices({ appId }: { appId: number }) {
  const { data, mutate, error } = useSWR<PushInfo>("/api/push", fetcher);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ tone: "info" | "danger"; text: string } | null>(null);

  if (error) return null; // notifications not available yet (node starting)
  const devices = (data?.subscriptions ?? []).filter((s) => s.app_id === appId);

  const act = async (f: () => Promise<string | void>) => {
    setBusy(true);
    setNote(null);
    try {
      const text = await f();
      if (text) setNote({ tone: "info", text });
    } catch (e) {
      setNote({ tone: "danger", text: message(e) });
    } finally {
      await mutate();
      setBusy(false);
    }
  };

  const remove = (d: PushSubscriptionView) => act(() => api.del(`/api/push/subscriptions/${d.id}`));
  const removeAll = () =>
    act(async () => {
      for (const d of devices) await api.del(`/api/push/subscriptions/${d.id}`);
      return `Removed ${devices.length} devices.`;
    });
  const test = (d: PushSubscriptionView) =>
    act(async () => {
      const r = await api.post<{ delivered: boolean; error?: string }>(`/api/push/subscriptions/${d.id}/test`);
      if (!r.delivered) throw new Error(r.error ?? "Not delivered.");
      return "Test sent. The app shows it if it handles test pushes.";
    });

  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between">
        <CardTitle>Push devices</CardTitle>
        {devices.length > 1 && (
          <Button size="sm" variant="ghost" disabled={busy} onClick={removeAll}>
            Remove all
          </Button>
        )}
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <p className="text-muted">
          Devices the app registered to get this connection&apos;s notifications as push messages. Removing one stops
          its notifications; the app can register it again.
        </p>
        {!data && <p className="text-muted">Loading…</p>}
        {data && devices.length === 0 && <Empty>No devices registered.</Empty>}
        {devices.map((d) => (
          <div key={d.id} className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-line p-3" aria-label={d.label || d.endpoint_host}>
            <div className="min-w-0 space-y-1">
              <div className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{d.label || "Unnamed device"}</span>
                {d.failures > 0 && <Badge tone="warn">Not delivering</Badge>}
              </div>
              <p className="text-xs text-muted">
                Via {d.endpoint_host} · registered {ago(d.created_at)} ·{" "}
                {d.last_success_at ? `last delivered ${ago(d.last_success_at)}` : "nothing delivered yet"}
                {d.last_error && <> · {d.last_error}</>}
              </p>
            </div>
            <div className="flex gap-2">
              <Button size="sm" variant="secondary" disabled={busy} onClick={() => test(d)}>
                Send a test
              </Button>
              <Button size="sm" variant="ghost" disabled={busy} onClick={() => remove(d)}>
                Remove
              </Button>
            </div>
          </div>
        ))}
        {note && <Alert tone={note.tone}>{note.text}</Alert>}
      </CardContent>
    </Card>
  );
}
