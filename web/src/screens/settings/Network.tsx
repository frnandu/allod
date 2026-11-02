import { useState } from "react";
import useSWR from "swr";
import { LockSimple, WifiHigh, WifiLow, WifiMedium } from "@phosphor-icons/react";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { Empty, Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { api, fetcher, type NetworkStatus, type WifiNetwork } from "../../lib/api";
import { SettingsGroup } from "./SettingsLayout";

export const NETWORK_KEY = "/api/system/network";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Network settings of the Allod appliance (SPEC §2.3.10): connections and Wi-Fi. */
export function NetworkSettings() {
  const { data, error, mutate } = useSWR<NetworkStatus>(NETWORK_KEY, fetcher, { refreshInterval: 10_000 });
  if (error) return <Alert tone="danger" title="Could not load the network status">{message(error)}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  if (!data.available) return <Empty>Network settings are only available on the Allod appliance images.</Empty>;
  return (
    <SettingsGroup title="Network" description="How this device connects to your network and the internet.">
      <StatusCard status={data} />
      {data.wifi_device ? (
        <WifiCard status={data} onChange={() => void mutate()} />
      ) : (
        <Card>
          <CardContent className="pt-6 text-sm text-muted">This device has no Wi-Fi adapter.</CardContent>
        </Card>
      )}
    </SettingsGroup>
  );
}

function StatusCard({ status }: { status: NetworkStatus }) {
  const devices = (status.devices ?? []).filter((d) => d.type === "ethernet" || d.type === "wifi");
  return (
    <Card>
      <CardHeader>
        <CardTitle>Connections</CardTitle>
        <CardDescription>
          Reach this device at <span className="font-mono">http://{status.hostname ?? "allod"}.local:8029</span> on
          the same network.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {status.hotspot && (
          <Alert tone="warn" title="Setup hotspot is on">
            The device found no network and opened its own Wi-Fi for setup. Choose a network below.
          </Alert>
        )}
        {devices.length === 0 && <p className="text-sm text-muted">No network adapters found.</p>}
        {devices.map((d) => (
          <div key={d.device} className="flex flex-wrap items-center justify-between gap-2 text-sm">
            <span>
              {d.type === "wifi" ? "Wi-Fi" : "Ethernet"} <span className="text-muted">({d.device})</span>
            </span>
            <span className="flex items-center gap-2">
              {d.ipv4.length > 0 && <span className="font-mono text-xs">{d.ipv4.join(", ")}</span>}
              <Badge tone={d.state === "connected" ? "ok" : "neutral"}>{d.state}</Badge>
            </span>
          </div>
        ))}
      </CardContent>
    </Card>
  );
}

function SignalIcon({ signal }: { signal: number }) {
  const Icon = signal >= 67 ? WifiHigh : signal >= 34 ? WifiMedium : WifiLow;
  return <Icon size={18} weight="duotone" aria-label={`signal ${signal}%`} />;
}

function WifiCard({ status, onChange }: { status: NetworkStatus; onChange: () => void }) {
  const [networks, setNetworks] = useState<WifiNetwork[] | null>(null);
  const [scanning, setScanning] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [joining, setJoining] = useState<{ ssid: string; open: boolean; hidden: boolean } | null>(null);
  const saved = new Set(status.saved ?? []);

  async function scan() {
    setScanning(true);
    setError(null);
    try {
      const r = await api.post<{ networks: WifiNetwork[] }>("/api/system/network/scan");
      setNetworks(r.networks);
    } catch (e) {
      setError(message(e));
    } finally {
      setScanning(false);
    }
  }

  async function forget(ssid: string) {
    setError(null);
    try {
      await api.post("/api/system/network/wifi/forget", { ssid });
      onChange();
    } catch (e) {
      setError(message(e));
    }
  }

  return (
    <Card>
      <CardHeader>
        <CardTitle>Wi-Fi</CardTitle>
        <CardDescription>
          {status.wifi ? (
            <>
              Connected to <strong>{status.wifi.ssid}</strong>.
            </>
          ) : (
            "Not connected to a Wi-Fi network."
          )}{" "}
          Ethernet is used first when both are connected.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <CountryRow country={status.country ?? null} onChange={onChange} />
        {error && <Alert tone="danger" title="Wi-Fi">{error}</Alert>}
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" size="sm" onClick={() => void scan()} disabled={scanning}>
            {scanning ? "Scanning…" : networks ? "Scan again" : "Find networks"}
          </Button>
          <Button variant="ghost" size="sm" onClick={() => setJoining({ ssid: "", open: false, hidden: true })}>
            Hidden network…
          </Button>
        </div>
        {networks && networks.length === 0 && <p className="text-sm text-muted">No networks in range.</p>}
        {networks && networks.length > 0 && (
          <ul className="divide-y divide-line rounded-lg border border-line">
            {networks.map((n) => (
              <li key={n.ssid} className="flex items-center justify-between gap-3 px-3 py-2 text-sm">
                <span className="flex min-w-0 items-center gap-2">
                  <SignalIcon signal={n.signal} />
                  <span className="truncate">{n.ssid}</span>
                  {n.security && <LockSimple size={14} className="shrink-0 text-muted" aria-label="secured" />}
                  {n.in_use && <Badge tone="ok">connected</Badge>}
                </span>
                {!n.in_use && (
                  <Button size="sm" variant="secondary" onClick={() => setJoining({ ssid: n.ssid, open: !n.security, hidden: false })}>
                    Join
                  </Button>
                )}
              </li>
            ))}
          </ul>
        )}
        {saved.size > 0 && (
          <div className="space-y-1">
            <p className="text-sm font-medium">Saved networks</p>
            {[...saved].map((ssid) => (
              <div key={ssid} className="flex items-center justify-between text-sm">
                <span className="truncate">{ssid}</span>
                <Button size="sm" variant="ghost" onClick={() => void forget(ssid)}>
                  Forget
                </Button>
              </div>
            ))}
          </div>
        )}
      </CardContent>
      {joining && (
        <JoinDialog
          {...joining}
          onClose={() => setJoining(null)}
          onJoined={() => {
            setJoining(null);
            onChange();
          }}
        />
      )}
    </Card>
  );
}

function CountryRow({ country, onChange }: { country: string | null; onChange: () => void }) {
  const [value, setValue] = useState(country ?? "");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  async function save() {
    setBusy(true);
    setError(null);
    try {
      await api.put("/api/system/network/country", { country: value });
      onChange();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="space-y-1">
      <div className="flex items-end gap-2">
        <Field label="Country" hint="Two-letter code (e.g. DE, US). Decides which Wi-Fi channels are allowed.">
          <Input value={value} maxLength={2} className="w-20 uppercase" onChange={(e) => setValue(e.target.value.toUpperCase())} />
        </Field>
        {value !== (country ?? "") && (
          <Button size="sm" onClick={() => void save()} disabled={busy || value.length !== 2} className="mb-6">
            Save
          </Button>
        )}
      </div>
      {error && <p className="text-sm text-danger">{error}</p>}
    </div>
  );
}

function JoinDialog({
  ssid: initialSsid,
  open,
  hidden,
  onClose,
  onJoined,
}: {
  ssid: string;
  open: boolean;
  hidden: boolean;
  onClose: () => void;
  onJoined: () => void;
}) {
  const [ssid, setSsid] = useState(initialSsid);
  const [password, setPassword] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function join() {
    setBusy(true);
    setError(null);
    try {
      await api.post("/api/system/network/wifi", { ssid, password, hidden });
      onJoined();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <Modal open onClose={onClose} title={hidden ? "Join a hidden network" : `Join ${initialSsid}`}>
      <form
        className="space-y-4"
        onSubmit={(e) => {
          e.preventDefault();
          void join();
        }}
      >
        {hidden && (
          <Field label="Network name">
            <Input value={ssid} onChange={(e) => setSsid(e.target.value)} autoFocus />
          </Field>
        )}
        {!open && (
          <Field label="Password" hint="Leave empty for an open network.">
            <Input type="password" value={password} onChange={(e) => setPassword(e.target.value)} autoFocus={!hidden} />
          </Field>
        )}
        <p className="text-xs text-muted">
          If you reach this page over the network you are switching away from, it may stop responding for a moment.
          The device keeps the network for later boots.
        </p>
        {error && <Alert tone="danger" title="Could not join">{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy || !ssid}>
            <WifiHigh size={16} /> {busy ? "Joining…" : "Join"}
          </Button>
        </div>
      </form>
    </Modal>
  );
}
