import { useEffect, useState } from "react";
import useSWR, { type KeyedMutator } from "swr";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { api, fetcher } from "../../lib/api";
import {
  disablePush,
  enablePush,
  PUSH_EVENTS,
  syncPush,
  unsupported,
  type PushEvent,
  type PushInfo,
  type PushSubscriptionView,
} from "../../lib/push";
import { SettingsGroup } from "./SettingsLayout";

const KEY = "/api/push";

function when(t: number | null): string {
  return t ? new Date(t * 1000).toLocaleString() : "never";
}

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

export function NotificationSettings() {
  const { data, error, mutate } = useSWR<PushInfo>(KEY, fetcher);
  // Which subscription is this browser's: learned by re-registering it.
  const [thisDevice, setThisDevice] = useState<number | null>(null);
  useEffect(() => {
    syncPush()
      .then((id) => {
        setThisDevice(id);
        if (id !== null) void mutate();
      })
      .catch(() => setThisDevice(null));
  }, [mutate]);

  return (
    <SettingsGroup
      title="Notifications"
      description="Get a notification on this device when a payment arrives or leaves. Notifications are encrypted end to end: the browser's push service only sees that something was sent."
    >
      {error && <Alert tone="danger" title="Could not load notification settings">{message(error)}</Alert>}
      {!data && !error && <p className="text-sm text-muted">Loading…</p>}
      {data && (
        <>
          <ThisDeviceCard info={data} id={thisDevice} onChange={(id) => { setThisDevice(id); void mutate(); }} />
          <DevicesCard subs={data.subscriptions} thisDevice={thisDevice} mutate={mutate} />
        </>
      )}
    </SettingsGroup>
  );
}

function ThisDeviceCard({ info, id, onChange }: { info: PushInfo; id: number | null; onChange: (id: number | null) => void }) {
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const why = unsupported();
  const enabled = id !== null && info.subscriptions.some((s) => s.id === id);

  const run = async (f: () => Promise<number | null>) => {
    setBusy(true);
    setErr(null);
    try {
      onChange(await f());
    } catch (e) {
      setErr(message(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHeader>
        <CardTitle>This device</CardTitle>
        <CardDescription>
          {enabled ? "This browser receives notifications." : "Turn on notifications for this browser."}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {why === "ios-not-installed" && (
          <Alert title="Add Allod to your Home Screen first">
            On iPhone and iPad, notifications only work in the installed web app (iOS 16.4 or later): tap Share, then
            “Add to Home Screen”, and open Allod from there.
          </Alert>
        )}
        {why === "insecure" && (
          <Alert tone="warn" title="Needs a secure connection">
            Browsers allow notifications only over HTTPS (or on localhost). Open Allod through its https address.
          </Alert>
        )}
        {why === "browser" && <Alert tone="warn" title="This browser cannot receive push notifications" />}
        {!why && typeof Notification !== "undefined" && Notification.permission === "denied" && (
          <Alert tone="warn" title="Notifications are blocked">
            Allow notifications for this site in the browser&apos;s site settings, then turn them on here.
          </Alert>
        )}
        {err && <Alert tone="danger" title="Could not change notifications">{err}</Alert>}
        {!why &&
          (enabled ? (
            <Button variant="secondary" disabled={busy} onClick={() => run(async () => (await disablePush(id), null))}>
              Turn off on this device
            </Button>
          ) : (
            <Button disabled={busy} onClick={() => run(async () => (await enablePush(info.vapid_public_key)).id)}>
              Turn on notifications
            </Button>
          ))}
      </CardContent>
    </Card>
  );
}

function DevicesCard({
  subs,
  thisDevice,
  mutate,
}: {
  subs: PushSubscriptionView[];
  thisDevice: number | null;
  mutate: KeyedMutator<PushInfo>;
}) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>Devices</CardTitle>
        <CardDescription>
          Every browser or installed app that receives notifications from this node. Wallet apps can also register their
          own device through their connection; they only get that connection&apos;s notifications.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {subs.length === 0 && <p className="text-sm text-muted">No devices yet.</p>}
        {subs.map((s) => (
          <DeviceRow key={s.id} sub={s} current={s.id === thisDevice} mutate={mutate} />
        ))}
      </CardContent>
    </Card>
  );
}

function DeviceRow({ sub, current, mutate }: { sub: PushSubscriptionView; current: boolean; mutate: KeyedMutator<PushInfo> }) {
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<{ tone: "info" | "danger"; text: string } | null>(null);

  const patch = async (body: { events?: PushEvent[]; hide_amounts?: boolean }) => {
    setBusy(true);
    setNote(null);
    try {
      await api.patch(`/api/push/subscriptions/${sub.id}`, body);
      await mutate();
    } catch (e) {
      setNote({ tone: "danger", text: message(e) });
    } finally {
      setBusy(false);
    }
  };

  const toggle = (e: PushEvent, on: boolean) =>
    patch({ events: on ? [...sub.events, e] : sub.events.filter((x) => x !== e) });

  const test = async () => {
    setBusy(true);
    setNote(null);
    try {
      const r = await api.post<{ delivered: boolean; error?: string }>(`/api/push/subscriptions/${sub.id}/test`);
      setNote(r.delivered ? { tone: "info", text: "Test sent. It should appear on the device in a few seconds." } : { tone: "danger", text: r.error ?? "Not delivered." });
      await mutate();
    } catch (e) {
      setNote({ tone: "danger", text: message(e) });
    } finally {
      setBusy(false);
    }
  };

  const remove = async () => {
    setBusy(true);
    try {
      if (current) await disablePush(sub.id);
      else await api.del(`/api/push/subscriptions/${sub.id}`);
      await mutate();
    } catch (e) {
      setNote({ tone: "danger", text: message(e) });
      setBusy(false);
    }
  };

  return (
    <div className="space-y-3 rounded-lg border border-line p-4" aria-label={sub.label || "Device"}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{sub.label || sub.app_name || "Unnamed device"}</span>
        {current && <Badge tone="accent">This device</Badge>}
        {sub.app_id !== null && <Badge>Via connection{sub.app_name ? `: ${sub.app_name}` : ""}</Badge>}
        {sub.failures > 0 && <Badge tone="warn">Not delivering</Badge>}
      </div>
      <p className="text-xs text-muted">
        Via {sub.endpoint_host} · last delivered {when(sub.last_success_at)}
        {sub.last_error && <> · {sub.last_error}</>}
      </p>
      <fieldset className="flex flex-wrap gap-x-5 gap-y-2 text-sm" disabled={busy}>
        <legend className="sr-only">Notify about</legend>
        {PUSH_EVENTS.map((e) => (
          <label key={e.id} className="flex items-center gap-2">
            <input type="checkbox" checked={sub.events.includes(e.id)} onChange={(ev) => toggle(e.id, ev.target.checked)} />
            {e.label}
          </label>
        ))}
        <label className="flex items-center gap-2">
          <input type="checkbox" checked={sub.hide_amounts} onChange={(ev) => patch({ hide_amounts: ev.target.checked })} />
          Hide amounts and descriptions
        </label>
      </fieldset>
      {note && <Alert tone={note.tone}>{note.text}</Alert>}
      <div className="flex flex-wrap gap-2">
        <Button size="sm" variant="secondary" disabled={busy} onClick={test}>
          Send a test
        </Button>
        <Button size="sm" variant="ghost" disabled={busy} onClick={remove}>
          Remove
        </Button>
      </div>
    </div>
  );
}
