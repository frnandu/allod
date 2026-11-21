import { useState } from "react";
import { Link } from "react-router";
import { ArrowsClockwise, CloudArrowUp, HardDrives, ShieldCheck } from "@phosphor-icons/react";
import useSWR, { type KeyedMutator } from "swr";
import { exposure } from "../../components/LightningExposure";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import { fetcher, lightning, LIGHTNING_SETTINGS, type LightningSettings } from "../../lib/api";
import { SettingsGroup } from "./SettingsLayout";

/**
 * Settings → Lightning (SPEC §12.7.2): other nodes connect to this one at a gateway's
 * public IP, which passes the connections over WireGuard to the node's port 9735, so the
 * home IP is never published in gossip.
 */
export function LightningSettings() {
  const { data, error, mutate } = useSWR<LightningSettings>(LIGHTNING_SETTINGS, fetcher, {
    // Follow the check and the node's restart for a while after a change.
    refreshInterval: (d) => (d?.gateway_id && (!d.reach || d.reach.state !== "reachable" || d.announced === false) ? 5000 : 30000),
  });
  return (
    <SettingsGroup title="Lightning" description="How other Lightning nodes reach yours.">
      <Card>
        <CardHeader>
          <CardTitle className="flex items-center gap-2">
            <ShieldCheck size={20} weight="duotone" /> Accept connections through a gateway
          </CardTitle>
          <CardDescription>
            Peers connect to the gateway&apos;s public IP, and the gateway passes the connection over its WireGuard tunnel to
            your node. Your node announces the gateway&apos;s address instead of your home IP, and you need no port forward on
            your router.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          {error && <Alert tone="danger" title="Could not load the Lightning settings">{error.message}</Alert>}
          {!data && !error && <p className="text-muted">Loading…</p>}
          {data && <Body data={data} mutate={mutate} />}
        </CardContent>
      </Card>
    </SettingsGroup>
  );
}

function NoGateway() {
  return (
    <Alert tone="info" title="No gateway yet">
      <div className="space-y-2">
        <p>A gateway is a small public server that carries connections to your node. You can:</p>
        <ul className="space-y-1">
          <li className="flex gap-2">
            <HardDrives size={16} weight="duotone" className="mt-0.5 shrink-0" />
            <span>use a Debian or Ubuntu server you already have with a public IPv4, and run Allod&apos;s setup script on it once;</span>
          </li>
          <li className="flex gap-2">
            <CloudArrowUp size={16} weight="duotone" className="mt-0.5 shrink-0" />
            <span>or let Allod rent one at LNVPS, paid in sats from Main, set up and renewed by itself.</span>
          </li>
        </ul>
        <p>The same gateway can also serve your domains over HTTPS.</p>
        <Link to="/settings/gateways" className="inline-block font-medium text-accent underline">
          Add a gateway in Settings → Gateways
        </Link>
      </div>
    </Alert>
  );
}

function Body({ data, mutate }: { data: LightningSettings; mutate: KeyedMutator<LightningSettings> }) {
  const usable = data.gateways.filter((g) => g.state !== "expired");
  const [on, setOn] = useState(!!data.gateway_id);
  const [gatewayId, setGatewayId] = useState<number | null>(data.gateway_id ?? usable[0]?.id ?? null);
  const [port, setPort] = useState(String(data.port ?? data.node_port ?? 9735));
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState<string | null>(null);

  if (!data.available || data.gateways.length === 0) return <NoGateway />;

  const portNumber = Number(port);
  const portOk = Number.isInteger(portNumber) && portNumber > 0 && portNumber < 65536;
  const unchanged = on
    ? data.gateway_id === gatewayId && data.port === portNumber
    : !data.gateway_id;
  const e = exposure(data);
  const uri = data.node && data.address ? `${data.node.pubkey}@${data.address}` : null;
  const chosen = data.gateways.find((g) => g.id === data.gateway_id);

  async function save() {
    setErr(null);
    setBusy(true);
    try {
      const v = await lightning.set(on && gatewayId ? { gateway_id: gatewayId, port: portNumber } : { gateway_id: null });
      await mutate(v, { revalidate: false });
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    } finally {
      setBusy(false);
    }
  }

  async function check() {
    setChecking(true);
    try {
      await mutate(await lightning.check(), { revalidate: false });
    } catch (x) {
      setErr(x instanceof Error ? x.message : String(x));
    } finally {
      setChecking(false);
    }
  }

  return (
    <div className="space-y-4">
      <div className="space-y-2 rounded-lg bg-surface-2 p-3" aria-label="Status">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <Badge tone={e.tone}>{e.label}</Badge>
          {data.gateway_id && (
            <Button size="sm" variant="ghost" onClick={() => void check()} disabled={checking}>
              <ArrowsClockwise size={14} /> {checking ? "Checking…" : "Check now"}
            </Button>
          )}
        </div>
        <p className="text-muted">{e.detail}</p>
        {uri && (
          <div className="flex flex-wrap items-center gap-2 font-mono text-xs">
            <span className="min-w-0 flex-1 break-all">{uri}</span>
            <CopyButton value={uri} />
          </div>
        )}
      </div>

      {!data.tunnel && (
        <Alert tone="warn" title="No tunnel service here">
          This installation does not run the tunnel service (the Compose bundle&apos;s https profile), so connections cannot
          come through a gateway.
        </Alert>
      )}

      <label className="flex items-start gap-2">
        <input type="checkbox" className="mt-1" checked={on} onChange={(ev) => setOn(ev.target.checked)} aria-label="Through a gateway" />
        <span>
          Accept Lightning connections through a gateway
          <span className="block text-xs text-muted">Off: your node announces no gateway address.</span>
        </span>
      </label>

      {on && (
        <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
          <Field label="Gateway">
            <select
              className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
              value={gatewayId ?? ""}
              onChange={(ev) => setGatewayId(Number(ev.target.value) || null)}
              aria-label="Gateway"
            >
              {usable.map((g) => (
                <option key={g.id} value={g.id}>
                  {g.label} — {g.ipv4 ?? "no IP yet"}
                  {g.state !== "active" ? ` (${g.state})` : ""}
                </option>
              ))}
            </select>
          </Field>
          <Field label="Public port" hint="9735 is the usual one.">
            <Input
              value={port}
              onChange={(ev) => setPort(ev.target.value.replace(/\D/g, ""))}
              inputMode="numeric"
              aria-label="Public port"
            />
          </Field>
        </div>
      )}

      {err && <Alert tone="danger" title="Could not save">{err}</Alert>}
      <div className="flex flex-wrap items-center justify-end gap-2">
        {!unchanged && data.managed && <span className="text-xs text-muted">Saving restarts the Lightning node.</span>}
        <Button onClick={() => void save()} disabled={busy || unchanged || (on && (!gatewayId || !portOk))}>
          {busy ? "Saving…" : "Save"}
        </Button>
      </div>

      {data.gateway_id && !data.managed && data.address && (
        <Alert tone="warn" title="Announce the address yourself">
          Allod does not manage this Lightning node&apos;s configuration. Set ldk-server&apos;s{" "}
          <code className="font-mono">announcement_addresses</code> to <code className="font-mono">{data.address}</code>.
        </Alert>
      )}
      {data.gateway_id && data.node && !data.node.alias && (
        <Alert tone="info" title="Your node has no alias">
          Without one the node does not announce itself in the gossip network, so peers learn the address only from the
          connection string above. Set <code className="font-mono">ALLOD_ALIAS</code> in the stack settings to announce it.
        </Alert>
      )}
      {chosen?.provider === "manual" && data.reach?.state === "unreachable" && (
        <p className="text-xs text-muted">
          Set up {chosen.label} with an older Allod? Run its setup script once more (
          <Link className="underline" to="/settings/gateways">
            Gateways
          </Link>
          ); after that the server follows this setting by itself.
        </p>
      )}
      <ul className="list-disc space-y-1 pl-5 text-xs text-muted">
        <li>Only connections to your node come through the gateway: the connections your node opens to peers still leave from your home IP.</li>
        <li>Do not forward port 9735 on your home router as well, or your home IP stays reachable.</li>
        <li>The gateway sees who connects, but not what is sent: Lightning traffic is encrypted end to end.</li>
      </ul>
    </div>
  );
}
