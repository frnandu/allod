import { ArrowRight, ShieldCheck, ShieldWarning } from "@phosphor-icons/react";
import { Link } from "react-router";
import useSWR from "swr";
import { fetcher, LIGHTNING_SETTINGS, type LightningReach, type LightningSettings } from "../lib/api";
import { Badge } from "./ui/badge";
import { CopyButton, Disclosure } from "./ui/extras";

export type Tone = "ok" | "warn" | "danger" | "neutral";

/** One way in (Tor, a gateway, the home IP) and what its last check found. */
export interface ExposurePath {
  name: string;
  address: string | null;
  tone: Tone;
  label: string;
  detail: string;
}

export interface Exposure {
  tone: Tone;
  label: string;
  detail: string;
  /** Other nodes can open connections to this one. */
  exposed: boolean;
  paths: ExposurePath[];
  /** The node has not announced the chosen addresses yet (it restarts to). */
  pending: boolean;
}

function pathStatus(name: string, address: string | null, r: LightningReach | null | undefined): ExposurePath {
  const at = (tone: Tone, label: string, detail: string) => ({ name, address, tone, label, detail });
  if (!r) return at("warn", "Checking…", "");
  switch (r.state) {
    case "reachable":
      // The connection string already says where; "peers can connect at …" adds nothing.
      return at("ok", "Reachable", "");
    case "waiting":
      return at("warn", "Waiting", r.detail);
    case "unchecked":
      return at("neutral", "Published", r.detail);
    default:
      return at("danger", "Not reachable", r.detail);
  }
}

const RANK: Record<Tone, number> = { ok: 0, neutral: 1, warn: 2, danger: 3 };

/** How other nodes reach this one: privately not at all, over Tor, through a gateway, at the home IP. */
export function exposure(s: LightningSettings): Exposure {
  const base = { paths: [] as ExposurePath[], pending: false };
  const paths: ExposurePath[] = [];
  if (s.clearnet === "gateway") {
    const gw = s.gateways.find((g) => g.id === s.gateway_id);
    paths.push(pathStatus(`Gateway${gw ? ` ${gw.label}` : ""}`, s.address ?? null, s.reach?.clearnet));
  } else if (s.clearnet === "host") {
    paths.push(pathStatus("Home IP", s.address ?? null, s.reach?.clearnet));
  }
  if (s.tor) paths.push(pathStatus("Tor", s.onion_address ?? null, s.reach?.tor));
  const pending = !!s.managed && s.announced === false;
  if (!paths.length)
    return {
      ...base,
      tone: "neutral",
      exposed: false,
      pending,
      label: "Private",
      detail: "Nobody can open a connection to your node. It still connects out, opens channels and pays.",
    };
  const label =
    s.clearnet === "gateway"
      ? s.tor
        ? "Tor and a gateway"
        : "Through a gateway"
      : s.clearnet === "host"
        ? s.tor
          ? "Tor and home IP"
          : "Home IP"
        : "Tor only";
  let tone = paths.map((p) => p.tone).reduce((a, b) => (RANK[b] > RANK[a] ? b : a), "ok" as Tone);
  if (s.clearnet === "host" && tone === "ok") tone = "warn";
  if (pending && tone === "ok") tone = "warn";
  const detail =
    s.clearnet === "host"
      ? "Peers connect straight to this machine, so your home IP is public."
      : s.clearnet === "gateway"
        ? s.tor
          ? "Peers connect through the gateway's IP or over Tor; your home IP stays hidden."
          : "Peers connect through the gateway's IP; your home IP stays hidden."
        : "Only Tor peers can connect, at an onion address; your IP stays hidden.";
  return { tone, label, detail, exposed: true, paths, pending };
}

/** The paths with their check results, one row each; with the node's pubkey, each address
 * comes as the `pubkey@address` string peers connect with, ready to copy. */
export function ExposurePaths({ paths, pubkey }: { paths: ExposurePath[]; pubkey?: string | null }) {
  if (!paths.length) return null;
  return (
    <ul className="space-y-2">
      {paths.map((p) => (
        <li key={p.name} className="space-y-0.5">
          <div className="flex flex-wrap items-center gap-2">
            <span className="font-medium">{p.name}</span>
            <Badge tone={p.tone}>{p.label}</Badge>
          </div>
          {p.address && <AddressLine value={pubkey ? `${pubkey}@${p.address}` : p.address} />}
          {p.detail && <p className="text-xs text-muted">{p.detail}</p>}
        </li>
      ))}
    </ul>
  );
}

/** A node address (or `pubkey@address`) with a copy button. */
export function AddressLine({ value }: { value: string }) {
  return (
    <div className="flex items-center gap-2">
      <span className="min-w-0 flex-1 break-all font-mono text-xs text-muted">{value}</span>
      <CopyButton value={value} />
    </div>
  );
}

/** Node overview: whether, and how, other nodes can open connections to this one. */
export function InboundConnections({ nodeUris = [] }: { nodeUris?: string[] }) {
  const { data } = useSWR<LightningSettings>(LIGHTNING_SETTINGS, fetcher, { refreshInterval: 30000 });
  if (!data?.available)
    // No connectivity setting here: what the node announces by itself.
    return (
      <div className="space-y-3">
        <h4 className="font-medium">Inbound connections</h4>
        {nodeUris.length ? (
          nodeUris.map((u) => <AddressLine key={u} value={u} />)
        ) : (
          <p className="text-sm text-muted">Your node announces no address, so other nodes cannot connect to it.</p>
        )}
      </div>
    );
  const e = exposure(data);
  const Icon = e.tone === "ok" ? ShieldCheck : ShieldWarning;
  return (
    <div className="space-y-3">
      <div className="flex items-center justify-between gap-2">
        <h4 className="flex items-center gap-2 font-medium">
          <Icon size={18} weight="duotone" className={e.tone === "ok" ? "text-ok" : "text-muted"} />
          Inbound connections
        </h4>
        <Badge tone={e.tone}>{e.label}</Badge>
      </div>
      <p className="text-sm text-muted">{e.detail}</p>
      {e.paths.length > 0 && (
        <>
          {/* At a glance: each way in and its state; addresses and check results fold away. */}
          <ul className="flex flex-wrap gap-x-4 gap-y-1 text-sm">
            {e.paths.map((p) => (
              <li key={p.name} className="flex items-center gap-2">
                <span>{p.name}</span>
                <Badge tone={p.tone}>{p.label}</Badge>
              </li>
            ))}
          </ul>
          <Disclosure label="Addresses and checks">
            <ExposurePaths paths={e.paths} pubkey={data.node?.pubkey} />
          </Disclosure>
        </>
      )}
      {e.pending && <p className="text-xs text-muted">Not announced yet: the Lightning node restarts to announce it.</p>}
      <Link
        to="/settings/lightning"
        className="inline-flex items-center gap-1 text-sm font-medium text-accent underline-offset-2 hover:underline"
      >
        {e.exposed ? "Connectivity settings" : "Make your node reachable"}
        <ArrowRight size={14} />
      </Link>
    </div>
  );
}
