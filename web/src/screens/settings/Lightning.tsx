import { useState, type ReactNode } from "react";
import { Link } from "react-router";
import { ArrowsClockwise, CloudArrowUp, Detective, Globe, HardDrives, HouseLine, Prohibit } from "@phosphor-icons/react";
import useSWR, { type KeyedMutator } from "swr";
import { AddressLine, exposure, ExposurePaths, type Tone } from "../../components/LightningExposure";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { Field, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  fetcher,
  lightning,
  LIGHTNING_SETTINGS,
  type LightningClearnet,
  type LightningConnectivityRequest,
  type LightningSettings,
} from "../../lib/api";
import { cn } from "../../lib/utils";
import { Liquidity } from "./Liquidity";
import { SettingsGroup } from "./SettingsLayout";

type Tab = "connectivity" | "liquidity";

/** Settings → Lightning (SPEC §12.7.2). */
export function LightningSettings() {
  const [tab, setTab] = useState<Tab>("connectivity");
  return (
    <SettingsGroup title="Lightning" description="How your Lightning node meets the rest of the network.">
      <Tabs
        tabs={[
          { id: "connectivity", label: "Connectivity" },
          { id: "liquidity", label: "Liquidity" },
        ]}
        value={tab}
        onChange={setTab}
      />
      {tab === "connectivity" && <Connectivity />}
      {tab === "liquidity" && <Liquidity />}
    </SettingsGroup>
  );
}

function Connectivity() {
  const { data, error, mutate } = useSWR<LightningSettings>(LIGHTNING_SETTINGS, fetcher, {
    // Follow the checks and the node's restart for a while after a change.
    refreshInterval: (d) => {
      const rs = [d?.reach?.clearnet, d?.reach?.tor];
      const settling =
        d?.announced === false ||
        (d?.clearnet && d.clearnet !== "none" && !rs[0]) ||
        (d?.tor && !rs[1]) ||
        rs.some((r) => r && r.state !== "reachable" && r.state !== "unchecked");
      return settling ? 5000 : 30000;
    },
  });
  if (error) return <Alert tone="danger" title="Could not load the Lightning settings">{error.message}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  if (!data.available)
    return (
      <Alert tone="info" title="Not available here">
        This installation does not let Allod change how the node is reached.
      </Alert>
    );
  return <Body data={data} mutate={mutate} />;
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

/** What the form holds; saved as one request. */
interface Draft {
  tor: boolean;
  clearnet: LightningClearnet;
  gatewayId: number | null;
  port: string;
  host: string;
  hostPort: string;
}

function draftOf(d: LightningSettings): Draft {
  const usable = d.gateways.filter((g) => g.state !== "expired");
  return {
    tor: !!d.tor,
    clearnet: d.clearnet ?? "none",
    gatewayId: (d.gateway_id && usable.some((g) => g.id === d.gateway_id) ? d.gateway_id : null) ?? usable[0]?.id ?? null,
    port: String(d.port ?? d.node_port ?? 9735),
    host: d.host ?? "",
    hostPort: String(d.host_port ?? d.default_host_port ?? 9735),
  };
}

const portOk = (p: string) => {
  const n = Number(p);
  return Number.isInteger(n) && n > 0 && n < 65536;
};

/** In a sentence: who can connect with these settings. */
function outcome(d: Draft): { tone: Tone; label: string; text: string } {
  if (!d.tor && d.clearnet === "none")
    return { tone: "neutral", label: "Private", text: "Nobody can connect to your node. It still pays, receives and opens channels itself." };
  if (d.clearnet === "none") return { tone: "ok", label: "Tor only", text: "Only peers that use Tor can connect. Your IP stays hidden." };
  const gateway = d.clearnet === "gateway";
  return {
    tone: gateway ? "ok" : "warn",
    label: d.tor ? (gateway ? "Tor and a gateway" : "Tor and home IP") : gateway ? "Through a gateway" : "Home IP",
    text:
      `Every node can connect ${gateway ? "through the gateway" : "at your home IP"}` +
      `${d.tor ? ", and Tor peers at an onion address" : ""}.` +
      (gateway ? " Your home IP stays hidden." : " Your home IP is public."),
  };
}

/** An on/off switch (WAI-ARIA `switch`). */
function Switch({ checked, onChange, disabled, label }: { checked: boolean; onChange: (v: boolean) => void; disabled?: boolean; label: string }) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={() => onChange(!checked)}
      className={cn(
        "relative inline-flex h-6 w-11 shrink-0 items-center rounded-full transition focus-visible:outline-2 focus-visible:outline-offset-2 focus-visible:outline-accent",
        checked ? "bg-accent" : "bg-line",
        disabled && "cursor-not-allowed opacity-50",
      )}
    >
      <span className={cn("inline-block h-5 w-5 rounded-full bg-white shadow transition", checked ? "translate-x-5" : "translate-x-0.5")} />
    </button>
  );
}

function Section({
  icon,
  title,
  description,
  action,
  children,
}: {
  icon: ReactNode;
  title: string;
  description: string;
  action?: ReactNode;
  children?: ReactNode;
}) {
  return (
    <section className="space-y-3 rounded-lg border border-line p-4" aria-label={title}>
      <div className="flex items-start gap-3">
        <span className="mt-0.5 text-muted">{icon}</span>
        <div className="min-w-0 flex-1 space-y-0.5">
          <h3 className="font-medium">{title}</h3>
          <p className="text-xs text-muted">{description}</p>
        </div>
        {action}
      </div>
      {children}
    </section>
  );
}

/** One internet path; its settings open inside it while it is selected. */
function Path({
  value,
  draft,
  onPick,
  icon,
  title,
  badge,
  current,
  unavailable,
  description,
  children,
}: {
  value: LightningClearnet;
  draft: Draft;
  onPick: (v: LightningClearnet) => void;
  icon: ReactNode;
  title: string;
  badge?: { tone: Tone; label: string };
  /** The path the node uses now. */
  current: boolean;
  unavailable?: string;
  description: string;
  children?: ReactNode;
}) {
  const selected = draft.clearnet === value;
  const id = `clearnet-${value}`;
  return (
    <div className={cn("rounded-lg border transition", selected ? "border-accent bg-accent-soft/30" : "border-line", unavailable && "opacity-60")}>
      <label htmlFor={id} className={cn("flex gap-3 p-3", unavailable ? "cursor-not-allowed" : "cursor-pointer")}>
        <input
          id={id}
          type="radio"
          name="lightning-clearnet"
          className="mt-1"
          checked={selected}
          disabled={!!unavailable}
          onChange={() => onPick(value)}
          aria-label={title}
        />
        <span className="min-w-0 flex-1 space-y-0.5">
          <span className="flex flex-wrap items-center gap-2">
            {icon}
            <span className="font-medium">{title}</span>
            {badge && <Badge tone={badge.tone}>{badge.label}</Badge>}
            {current && <Badge tone="accent">Current</Badge>}
          </span>
          <span className="block text-xs text-muted">{unavailable ?? description}</span>
        </span>
      </label>
      {selected && children && <div className="space-y-3 border-t border-line p-3 sm:pl-10">{children}</div>}
    </div>
  );
}

function Body({ data, mutate }: { data: LightningSettings; mutate: KeyedMutator<LightningSettings> }) {
  const [draft, setDraft] = useState<Draft>(() => draftOf(data));
  const [busy, setBusy] = useState(false);
  const [checking, setChecking] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const set = (p: Partial<Draft>) => setDraft((d) => ({ ...d, ...p }));
  const [detect, setDetect] = useState<{ state: "idle" | "busy" | "done" | "failed"; text?: string }>({ state: "idle" });

  /** Asks an echo service (from this machine, not the browser) for the public IP. */
  async function detectIp() {
    setDetect({ state: "busy" });
    try {
      const r = await lightning.detectIp();
      set({ host: r.ip });
      setDetect({ state: "done", text: r.source });
    } catch (x) {
      setDetect({ state: "failed", text: x instanceof Error ? x.message : String(x) });
    }
  }

  function pick(v: LightningClearnet) {
    set({ clearnet: v });
    // Prefilled the first time the home IP is opened empty.
    if (v === "host" && !draft.host.trim() && detect.state === "idle") void detectIp();
  }

  const saved = draftOf(data);
  const usable = data.gateways.filter((g) => g.state !== "expired");
  const torAvailable = !!data.tor_available;
  const defaultHostPort = data.default_host_port ?? 9735;
  const dirty =
    draft.tor !== saved.tor ||
    draft.clearnet !== saved.clearnet ||
    (draft.clearnet === "gateway" && (draft.gatewayId !== data.gateway_id || Number(draft.port) !== data.port)) ||
    (draft.clearnet === "host" && (draft.host.trim() !== (data.host ?? "") || Number(draft.hostPort) !== data.host_port));
  const valid =
    (draft.clearnet !== "gateway" || (!!draft.gatewayId && portOk(draft.port))) &&
    (draft.clearnet !== "host" || (draft.host.trim() !== "" && portOk(draft.hostPort)));

  const e = exposure(data);
  const next = outcome(draft);
  const chosenGateway = data.gateways.find((g) => g.id === data.gateway_id);

  function request(): LightningConnectivityRequest {
    const tor = torAvailable && draft.tor;
    if (draft.clearnet === "gateway") return { clearnet: "gateway", tor, gateway_id: draft.gatewayId, port: Number(draft.port) };
    if (draft.clearnet === "host") return { clearnet: "host", tor, host: draft.host.trim(), host_port: Number(draft.hostPort) };
    return { clearnet: "none", tor };
  }

  async function save() {
    setErr(null);
    setBusy(true);
    try {
      const v = await lightning.set(request());
      await mutate(v, { revalidate: false });
      setDraft(draftOf(v));
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
      <Card>
        <CardHeader className="flex-row flex-wrap items-start justify-between gap-3">
          <div className="space-y-1">
            <CardTitle className="flex flex-wrap items-center gap-2">
              Inbound connections <Badge tone={e.tone}>{e.label}</Badge>
            </CardTitle>
            <CardDescription>{e.detail}</CardDescription>
          </div>
          {e.paths.length > 0 && (
            <Button size="sm" variant="ghost" onClick={() => void check()} disabled={checking}>
              <ArrowsClockwise size={14} /> {checking ? "Checking…" : "Check now"}
            </Button>
          )}
        </CardHeader>
        {(e.paths.length > 0 || e.pending) && (
          <CardContent className="space-y-3 text-sm">
            <ExposurePaths paths={e.paths} pubkey={data.node?.pubkey} />
            {e.pending && <p className="text-xs text-muted">Not announced yet: the Lightning node restarts to announce it.</p>}
          </CardContent>
        )}
      </Card>

      <Card>
        <CardHeader>
          <CardTitle>Who can connect</CardTitle>
          <CardDescription>
            Peers that can reach your node can open channels to it and route payments through it. Tor and an internet
            address work on their own or together.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4 text-sm">
          <Section
            icon={<Detective size={20} weight="duotone" />}
            title="Over Tor"
            description={
              torAvailable
                ? "Tor peers reach your node at an onion address, which hides your IP. Fewer nodes use Tor, and connections are slower."
                : "Not available on this device yet."
            }
            action={<Switch checked={torAvailable && draft.tor} disabled={!torAvailable} onChange={(v) => set({ tor: v })} label="Reachable over Tor" />}
          >
            {torAvailable && (draft.tor || data.tor) && (
              <div className="space-y-1 pl-8">
                {data.tor && data.onion_address ? (
                  <>
                    <p className="text-xs font-medium">Onion address{draft.tor ? "" : " (turned off when you save)"}</p>
                    <AddressLine value={data.onion_address} />
                  </>
                ) : data.tor ? (
                  <p className="text-xs text-muted">Tor is creating the onion address; it shows here in a few seconds.</p>
                ) : (
                  <p className="text-xs text-muted">The onion address is created when you save, and stays the same from then on.</p>
                )}
              </div>
            )}
          </Section>

          <Section
            icon={<Globe size={20} weight="duotone" />}
            title="Over the internet"
            description="Most Lightning nodes connect over plain internet addresses. Pick how they reach yours."
          >
            <div className="space-y-2" role="radiogroup" aria-label="Over the internet">
              <Path
                value="none"
                draft={draft}
                onPick={pick}
                current={saved.clearnet === "none"}
                icon={<Prohibit size={18} weight="duotone" />}
                title="Not over the internet"
                description="No internet address is announced, and the node's port stays closed."
              />
              <Path
                value="gateway"
                draft={draft}
                onPick={pick}
                current={saved.clearnet === "gateway"}
                icon={<CloudArrowUp size={18} weight="duotone" />}
                title="Through a gateway"
                badge={{ tone: "ok", label: "Home IP hidden" }}
                unavailable={data.tunnel ? undefined : "Not available on this device yet."}
                description="Peers connect to a small public server, which passes the connection on to your node. No port forward needed."
              >
                {usable.length === 0 ? (
                  <NoGateway />
                ) : (
                  <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
                    <Field label="Gateway">
                      <select
                        className="h-10 w-full rounded-lg border border-line bg-surface px-3 text-sm"
                        value={draft.gatewayId ?? ""}
                        onChange={(ev) => set({ gatewayId: Number(ev.target.value) || null })}
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
                    <Field label="Port" hint="9735 is the usual one.">
                      <Input
                        value={draft.port}
                        onChange={(ev) => set({ port: ev.target.value.replace(/\D/g, "") })}
                        inputMode="numeric"
                        aria-label="Port"
                      />
                    </Field>
                  </div>
                )}
              </Path>
              <Path
                value="host"
                draft={draft}
                onPick={pick}
                current={saved.clearnet === "host"}
                icon={<HouseLine size={18} weight="duotone" />}
                title="At my home IP"
                badge={{ tone: "danger", label: "Publishes your IP" }}
                description="Peers connect straight to this machine. Needs a public IP and a port forward on your router."
              >
                <Alert tone="warn">
                  Your IP — and roughly where you are — is announced to the whole network and stays on record. Prefer a
                  gateway if you can.
                </Alert>
                <div className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_9rem]">
                  <Field label="Public address" hint="Your router's public IP, or a DNS name that points to it.">
                    <Input
                      value={draft.host}
                      onChange={(ev) => set({ host: ev.target.value })}
                      placeholder={detect.state === "busy" ? "Detecting…" : "203.0.113.7"}
                      aria-label="Public address"
                    />
                  </Field>
                  <Field label="Port" hint={`Forward it to port ${defaultHostPort} of this machine.`}>
                    <Input
                      value={draft.hostPort}
                      onChange={(ev) => set({ hostPort: ev.target.value.replace(/\D/g, "") })}
                      inputMode="numeric"
                      aria-label="Port"
                    />
                  </Field>
                </div>
                <p className="text-xs text-muted" aria-live="polite">
                  {detect.state === "busy" && "Detecting the public IP of this machine…"}
                  {detect.state === "done" &&
                    `Filled in by asking ${detect.text} which address this machine's connections come from. A VPN or a second router can make it differ from the one to forward. `}
                  {detect.state === "failed" && <span className="text-warn">{detect.text}. </span>}
                  {detect.state !== "busy" && (
                    <button type="button" className="font-medium text-accent underline-offset-2 hover:underline" onClick={() => void detectIp()}>
                      {detect.state === "idle" ? "Detect my public IP" : "Detect again"}
                    </button>
                  )}
                </p>
              </Path>
            </div>
          </Section>

          {err && <Alert tone="danger" title="Could not save">{err}</Alert>}

          <div className="flex flex-col gap-3 rounded-lg bg-surface-2 p-3 sm:flex-row sm:items-center sm:justify-between" role="status" aria-label="Result">
            <div className="min-w-0 space-y-0.5">
              <p className="flex flex-wrap items-center gap-2">
                <span className="font-medium">{dirty ? "After saving" : "Saved"}</span>
                <Badge tone={next.tone}>{next.label}</Badge>
              </p>
              <p className="text-xs text-muted">
                {next.text}
                {dirty && data.managed ? " Saving restarts the Lightning node." : ""}
              </p>
            </div>
            <div className="flex shrink-0 gap-2">
              {dirty && (
                <Button variant="ghost" onClick={() => setDraft(saved)} disabled={busy}>
                  Discard
                </Button>
              )}
              <Button onClick={() => void save()} disabled={busy || !dirty || !valid}>
                {busy ? "Saving…" : "Save"}
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      {!data.managed && (data.addresses?.length ?? 0) > 0 && (
        <Alert tone="warn" title="Announce the address yourself">
          Allod does not manage this Lightning node&apos;s configuration. Set LDK Server&apos;s{" "}
          <code className="font-mono">announcement_addresses</code> to <code className="font-mono">{data.addresses!.join(", ")}</code>.
        </Alert>
      )}
      {e.exposed && data.node && !data.node.alias && (
        <Alert tone="info" title="Your node has no public name">
          Without a name (alias) your node is not listed in the network&apos;s node directory, so peers find it only
          through the connection string above. The name is set when installing (<code className="font-mono">ALLOD_ALIAS</code>).
        </Alert>
      )}
      {data.clearnet === "gateway" && chosenGateway?.provider === "manual" && data.reach?.clearnet?.state === "unreachable" && (
        <Alert tone="info">
          Set up {chosenGateway.label} with an older Allod? Run its setup script once more (
          <Link className="underline" to="/settings/gateways">
            Gateways
          </Link>
          ); after that the server follows this setting by itself.
        </Alert>
      )}
      <ul className="list-disc space-y-1 pl-5 text-xs text-muted">
        <li>
          This decides who can connect to your node. Connections your node opens itself still leave from your home IP,
          except those to onion addresses.
        </li>
        <li>Unless you choose your home IP, the node&apos;s port stays closed: you need no port forward, and can remove one you had.</li>
        <li>Lightning traffic is encrypted end to end: a gateway or Tor relay sees that a connection happens, not what is sent.</li>
      </ul>
    </div>
  );
}
