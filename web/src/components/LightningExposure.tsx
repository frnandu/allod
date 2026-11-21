import { ArrowRight, ShieldCheck, ShieldWarning } from "@phosphor-icons/react";
import { Link } from "react-router";
import useSWR from "swr";
import { fetcher, LIGHTNING_SETTINGS, type LightningSettings } from "../lib/api";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";

type Tone = "ok" | "warn" | "danger" | "neutral";

/** How peers reach the node: through a gateway (home IP hidden) or not. */
export function exposure(s: LightningSettings): { tone: Tone; label: string; detail: string; on: boolean } {
  const gw = s.gateways.find((g) => g.id === s.gateway_id);
  if (!s.gateway_id || !gw) {
    const own = s.node?.announcement_addresses ?? [];
    return {
      tone: "neutral",
      on: false,
      label: "Not through a gateway",
      detail: own.length
        ? `Peers reach the node at the address it announces itself (${own.join(", ")}), which can be your home IP.`
        : "Other nodes cannot open connections to yours; your node can still connect out and open channels.",
    };
  }
  const where = s.address ? `${s.address} (${gw.label})` : gw.label;
  const r = s.reach;
  if (!r) return { tone: "warn", on: true, label: "Checking…", detail: `Through ${where}.` };
  if (r.state === "waiting") return { tone: "warn", on: true, label: "Waiting for the gateway", detail: r.detail };
  if (r.state !== "reachable") return { tone: "danger", on: true, label: "Not reachable", detail: r.detail };
  if (s.managed && s.announced === false)
    return {
      tone: "warn",
      on: true,
      label: "Reachable, not announced yet",
      detail: `Peers can connect at ${where}; the Lightning node restarts to announce it.`,
    };
  return { tone: "ok", on: true, label: "Reachable through the gateway", detail: `Peers connect at ${where}; your home IP stays hidden.` };
}

/** Node overview: whether inbound Lightning connections go through a gateway. */
export function LightningExposureCard() {
  const { data } = useSWR<LightningSettings>(LIGHTNING_SETTINGS, fetcher, { refreshInterval: 30000 });
  if (!data?.available) return null;
  const e = exposure(data);
  const Icon = e.tone === "ok" ? ShieldCheck : ShieldWarning;
  return (
    <Card>
      <CardHeader className="flex-row items-center justify-between gap-3">
        <CardTitle className="flex items-center gap-2">
          <Icon size={20} weight="duotone" className={e.tone === "ok" ? "text-ok" : "text-muted"} />
          Inbound connections
        </CardTitle>
        <Badge tone={e.tone}>{e.label}</Badge>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <p className="text-muted">{e.detail}</p>
        <Link
          to="/settings/lightning"
          className="inline-flex items-center gap-1 text-sm font-medium text-accent underline-offset-2 hover:underline"
        >
          {e.on ? "Lightning settings" : "Expose the node through a gateway in Settings → Lightning"}
          <ArrowRight size={14} />
        </Link>
      </CardContent>
    </Card>
  );
}
