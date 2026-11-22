import type { ReactNode } from "react";
import { Link } from "react-router";
import useSWR from "swr";
import {
  DOMAINS_KEY,
  PRISM_FAILURES_KEY,
  PUBLIC_NAME_KEY,
  fetcher,
  type AllodStatus,
  type AppConn,
  type Domains,
  type HealthResponse,
  type InfoResponse,
  type Nip05Name,
  type PrismFailures,
  type PublicName,
} from "../lib/api";
import { items as lightningAddresses } from "../screens/settings/LightningAddresses";
import { useBackupVerified } from "../lib/backup";
import { formatAgo } from "../lib/eta";
import { useNodeReady } from "../lib/health";
import { useNow } from "../lib/useNow";
import { cn } from "../lib/utils";
import { Amount } from "./Amount";
import { bytes } from "./BitcoinCoreCard";
import { Alert } from "./ui/alert";
import { Badge } from "./ui/badge";
import { Card, CardContent, CardHeader, CardTitle } from "./ui/card";
import { Disclosure } from "./ui/extras";

const DAY_SECS = 24 * 3600;

const PLATFORM: Record<string, string> = { docker: "Docker", umbrel: "umbrelOS", startos: "StartOS" };

const TONE = { ok: "text-ok", warn: "text-warn", danger: "text-danger" } as const;

/** One figure; linked to where it is managed when `to` is given. */
function Stat({ label, to, value, sub, tone }: { label: string; to?: string; value: ReactNode; sub?: ReactNode; tone?: keyof typeof TONE }) {
  const body = (
    <>
      <p className="text-muted group-hover:text-fg">{label}</p>
      <p className={cn("font-mono", tone && TONE[tone])}>{value}</p>
      {sub != null && <div className="text-xs text-muted">{sub}</div>}
    </>
  );
  return to ? (
    <Link to={to} className="group block">
      {body}
    </Link>
  ) : (
    <div>{body}</div>
  );
}

const plural = (n: number, one: string, many = `${one}s`) => `${n.toLocaleString()} ${n === 1 ? one : many}`;

const DB_NAME: Record<string, string> = { sqlite: "SQLite", postgres: "PostgreSQL" };

/** "wss://relay.example.com/" → "relay.example.com". */
function host(url: string) {
  try {
    return new URL(url).host || url;
  } catch {
    return url;
  }
}

type Relays = NonNullable<AllodStatus["relays"]>;

/** NWC relays: connected out of all, the rest named. Apps reach the wallet only through these. */
function relaySummary(relays: Relays | null | undefined) {
  const list = relays?.states?.relays;
  if (!list) return null;
  const up = list.filter((r) => r.connected).length;
  const down = list.filter((r) => !r.connected).map((r) => host(r.url));
  return {
    value: `${up}/${list.length} connected`,
    tone: list.length === 0 || up === 0 ? ("danger" as const) : down.length ? ("warn" as const) : ("ok" as const),
    sub: down.length ? `down: ${down.join(", ")}` : "apps reach the wallet",
  };
}

/**
 * Allod's own layer, above the Lightning node: the apps and accounts it serves, the names it
 * answers for, and whether its ledger and backup are in order.
 */
export function AllodCard() {
  const now = useNow();
  // Wallet routes (apps, accounts, ledger) answer 503 until the Lightning node runs.
  const ready = useNodeReady();
  const { data: info } = useSWR<InfoResponse>("/api/info", fetcher);
  const { data: health } = useSWR<HealthResponse>("/api/health", fetcher, { refreshInterval: 15000 });
  const { data: apps } = useSWR<AppConn[]>(ready ? "/api/apps" : null, fetcher, { refreshInterval: 30000 });
  const { data: accounts } = useSWR<{ count: number; total_balance_msat: number }>(ready ? "/api/accounts/summary" : null, fetcher, {
    refreshInterval: 30000,
  });
  // Reads every posting: checked once per visit, not polled.
  const { data: ledger } = useSWR<{ ok: boolean; problems: unknown[] }>(ready ? "/api/ledger/check" : null, fetcher, {
    revalidateOnFocus: false,
  });
  const { data: domains } = useSWR<Domains>(DOMAINS_KEY, fetcher);
  const { data: publicName } = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher, { refreshInterval: 60000 });
  const { data: nip05 } = useSWR<{ names: Nip05Name[] }>("/api/nip05", fetcher);
  const { data: prism } = useSWR<PrismFailures>(PRISM_FAILURES_KEY, fetcher, { refreshInterval: 60000 });
  const backedUp = useBackupVerified();
  const { data: status } = useSWR<AllodStatus>("/api/allod/status", fetcher, { refreshInterval: 15000 });
  const relays = relaySummary(status?.relays);
  const activity = status?.activity;
  const held = status?.held_invoices;
  const push = status?.push;
  const db = status?.database;

  const usedToday = apps?.filter((a) => a.last_used_at != null && now - a.last_used_at < DAY_SECS).length ?? 0;
  const expired = apps?.filter((a) => a.expires_at != null && a.expires_at <= now).length ?? 0;
  // Own domains and third-party names (twelve.cash, clank.money, ZEUS Pay, Alby) alike.
  const addresses = publicName ? lightningAddresses(publicName, domains) : undefined;
  const liveAddresses = addresses?.filter((a) => a.status.tone === "ok").length ?? 0;

  const badge: { tone: "ok" | "warn" | "danger" | "accent"; label: string } = !health
    ? { tone: "accent", label: "Starting" }
    : health.status === "down"
      ? { tone: "danger", label: "Database down" }
      : health.status === "degraded"
        ? { tone: "warn", label: "Waiting for node" }
        : ledger && !ledger.ok
          ? { tone: "danger", label: "Ledger problem" }
          : { tone: "ok", label: "Running" };

  const subtitle = [
    info?.version ? `Allod ${info.version}` : "Allod",
    info ? (PLATFORM[info.deployment.platform] ?? null) : null,
  ]
    .filter(Boolean)
    .join(" · ");

  return (
    <Card className="card-wash-allod min-w-0 lg:col-span-2">
      <CardHeader className="flex-row items-center justify-between gap-3">
        <div className="min-w-0">
          <CardTitle>Allod</CardTitle>
          <p className="pt-1 text-xs text-muted">{subtitle}</p>
        </div>
        <Badge tone={badge.tone}>{badge.label}</Badge>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="grid grid-cols-2 gap-x-4 gap-y-4 text-sm sm:grid-cols-4">
          <Stat
            label="Connections"
            to="/connections"
            value={apps ? apps.length.toLocaleString() : "—"}
            sub={apps ? (expired ? `${usedToday} used today · ${expired} expired` : `${usedToday} used today`) : undefined}
          />
          <Stat
            label="Accounts"
            to="/accounts"
            value={accounts ? accounts.count.toLocaleString() : "—"}
            sub={accounts && accounts.count > 0 ? <Amount msat={accounts.total_balance_msat} /> : accounts ? "none yet" : undefined}
          />
          <Stat
            label="Lightning addresses"
            to="/settings/lightning-addresses"
            value={addresses ? addresses.length.toLocaleString() : "—"}
            tone={addresses && liveAddresses < addresses.length ? "warn" : undefined}
            sub={
              [
                addresses?.length ? `${liveAddresses} live` : null,
                nip05 ? plural(nip05.names.length, "Nostr name") : null,
              ]
                .filter(Boolean)
                .join(" · ") || undefined
            }
          />
          <Stat
            label="Ledger"
            value={ledger ? (ledger.ok ? "Balanced" : plural(ledger.problems.length, "problem")) : "—"}
            tone={ledger ? (ledger.ok ? "ok" : "danger") : undefined}
            sub="double-entry check"
          />
          <Stat
            label="NWC relays"
            to="/connections"
            value={relays?.value ?? "—"}
            tone={relays?.tone}
            sub={relays?.sub}
          />
          <Stat
            label="Last 24 hours"
            to="/activity"
            value={activity ? plural(activity.received.count + activity.sent.count, "payment") : "—"}
            sub={
              activity && (
                <>
                  <p>
                    <Amount msat={activity.received.msat} /> in
                  </p>
                  <p>
                    <Amount msat={activity.sent.msat} /> out
                    {activity.failed_sends > 0 && <span className="text-warn"> · {activity.failed_sends} failed</span>}
                  </p>
                </>
              )
            }
          />
          <Stat
            label="Notifications"
            to="/settings/notifications"
            value={push ? plural(push.devices, "device") : "—"}
            tone={push && push.failing > 0 ? "warn" : undefined}
            sub={
              push &&
              ([
                push.failing > 0 ? `${push.failing} failing` : push.last_success_at ? `last sent ${formatAgo(now - push.last_success_at)} ago` : push.devices ? "nothing sent yet" : "none subscribed",
                push.via_connections ? `${push.via_connections} via a connection` : null,
              ]
                .filter(Boolean)
                .join(" · "))
            }
          />
          <Stat
            label="Database"
            value={db?.size_bytes != null ? bytes(db.size_bytes) : "—"}
            sub={db && [DB_NAME[db.backend] ?? db.backend, db.litestream ? "replicated by Litestream" : null].filter(Boolean).join(" · ")}
          />
        </div>
        {held && held.count > 0 && (
          <Alert tone="info" title={`${plural(held.count, "hold invoice")} waiting to be settled`}>
            <Amount msat={held.msat} /> paid and held for the receiving app
            {held.next_deadline != null && <> · settle or cancel before block {held.next_deadline.toLocaleString()}, or the node cancels</>}.
          </Alert>
        )}
        {prism && prism.failed_count > 0 && (
          <Alert tone="warn" title={`${plural(prism.failed_count, "prism payout")} failed`}>
            <Amount msat={prism.failed_msat} /> did not reach its split destinations.{" "}
            <Link to="/settings/lightning-addresses" className="font-medium underline underline-offset-2">
              Review the splits
            </Link>
          </Alert>
        )}
        {backedUp === false && (
          <Alert tone="warn" title="Recovery phrase not checked">
            Without a checked backup, a lost disk loses the funds.{" "}
            <Link to="/settings/backup" className="font-medium underline underline-offset-2">
              Check it now
            </Link>
          </Alert>
        )}
        {status?.relays?.states && (
          <Disclosure label="Relays">
            <ul className="space-y-1 text-xs">
              {status.relays.states.relays.map((r) => (
                <li key={r.url} className="flex items-center gap-2">
                  <span aria-hidden className={cn("size-1.5 shrink-0 rounded-full", r.connected ? "bg-ok" : "bg-danger")} />
                  <span className="truncate font-mono">{r.url}</span>
                  <span className="shrink-0 text-muted">
                    {r.connected && r.connected_at ? `connected ${formatAgo(now - r.connected_at)}` : r.status}
                  </span>
                </li>
              ))}
            </ul>
          </Disclosure>
        )}
      </CardContent>
    </Card>
  );
}
