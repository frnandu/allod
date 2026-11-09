import { Eye, HouseLine, ShieldCheck, Wallet, Warning, type Icon } from "@phosphor-icons/react";
import { type PublicName } from "../../lib/api";
import { cn } from "../../lib/utils";

/**
 * The privacy ladder of Lightning address options (best first). Every step is described
 * with the same four signals, so options compare at a glance; steps not built yet are
 * shown as "coming soon".
 */

type Level = "ok" | "warn" | "danger";

interface Signal {
  level: Level;
  text: string;
}

/** Where the add dialog opens for a step. */
export type LadderAction = "own" | "own-lnurl" | "twelve" | "twelve-custom" | "clank" | "picked" | "hosted" | "zeus" | "alby";

export interface LadderStep {
  id: string;
  /** 0 (least private) … 5 (fully yours); one step each. */
  rank: number;
  title: string;
  promise: string;
  cost: string;
  sees: Signal;
  redirect: Signal;
  ip: Signal;
  wallets: Signal;
  /** The practical cost: setup, upkeep or fragility, in one line. */
  catch?: string;
  /** Choices that open the add dialog; empty: coming soon. */
  actions: { label: string; to: LadderAction }[];
  soon?: string;
  /** Not built yet ("Coming soon"). */
  upcoming?: boolean;
}

const EVERY: Signal = { level: "ok", text: "Every Lightning wallet" };
const MODERN: Signal = { level: "warn", text: "Only wallets supporting BOLT12/BIP-353 can pay" };
const NOBODY_SEES: Signal = { level: "ok", text: "Nobody but you" };
/** The limit of every BIP-353-only option, first in its warning box. */
const BIP353_ONLY = "Only wallets supporting BOLT12/BIP-353 can pay it.";
const IP_HIDDEN: Signal = { level: "ok", text: "Your node's IP stays hidden" };

export function ladder(d: PublicName): LadderStep[] {
  const steps: LadderStep[] = [
    {
      id: "relay",
      rank: 5,
      title: "Fully yours",
      promise: "Your domain and a small rented server that you control. No third party anywhere.",
      cost: "Your domain + ~5k sats a month",
      sees: NOBODY_SEES,
      redirect: { level: "ok", text: "No third party: your own domain and node answer payers" },
      ip: { level: "ok", text: "Hidden: only the gateway's IP is public" },
      wallets: EVERY,
      catch:
        "A monthly bill. Allod will rent the server (paid in sats) and set it up for you. It only relays requests: it holds no keys or funds, which stay on your node.",
      actions: [],
      upcoming: true,
      soon: "Have a server already? Run deploy/relay/allod-relay on it, then set its IP as the A record under Your domain → DNS provider.",
    },
    {
      id: "domain",
      rank: 4,
      title: "Your own domain",
      promise: "name@your-domain, as many names as you want. No registry in between.",
      cost: "Your domain",
      sees: NOBODY_SEES,
      redirect: { level: "ok", text: "No third party: your own domain and node answer payers" },
      ip: IP_HIDDEN,
      wallets: MODERN,
      catch: `The domain's DNS must support DNSSEC. Your IP never matters here.`,
      actions: [{ label: "Use my domain", to: "own" }],
      soon: "With Njalla, Allod writes the DNS records for you; elsewhere you add them by hand.",
    },
    d.prefs?.site === "server"
      ? {
          id: "domain-lnurl",
          rank: 3,
          title: "Your own domain, served by this server",
          promise: "The same domain also answers every Lightning wallet, straight from this server.",
          cost: "Your domain",
          sees: NOBODY_SEES,
          redirect: { level: "ok", text: "No third party: your own domain and node answer payers" },
          ip: { level: "warn", text: "The server's IP is public (not your home)" },
          wallets: EVERY,
          catch:
            "The hosting provider can read this server's memory and disk, including the node's keys: you trust it with your funds. Port 443 must be reachable, and the domain's A record must point at the server.",
          actions: [{ label: "Use my domain", to: "own-lnurl" }],
        }
      : {
          id: "domain-lnurl",
          rank: 3,
          title: "Your own domain, served from home",
          promise: "The same domain also answers every Lightning wallet, straight from this machine.",
          cost: "Your domain",
          sees: NOBODY_SEES,
          redirect: { level: "ok", text: "No third party: your own domain and node answer payers" },
          ip: { level: "danger", text: "Your home IP becomes public" },
          wallets: EVERY,
          catch:
            "Home IPs change: when the router restarts, the domain must be updated to the new IP, which needs a DNS provider with an API (many registrars have none). Also needs port 443 forwarded, and does not work behind CGNAT.",
          actions: [{ label: "Use my domain", to: "own-lnurl" }],
        },
    {
      id: "picked",
      rank: 2,
      title: "A name you pick",
      promise: "Your own name on a registry, no domain needed.",
      cost: "1k–5k sats once",
      sees: NOBODY_SEES,
      redirect: { level: "warn", text: "The registry could swap your offer, but only publicly: the record is signed for everyone to see, and Allod checks it" },
      ip: IP_HIDDEN,
      wallets: MODERN,
      catch: `A third-party registry keeps your record online.`,
      actions: d.clank_available || d.twelve_cash_available ? [{ label: "Choose", to: "picked" }] : [],
      soon: d.clank_available || d.twelve_cash_available ? undefined : "Mainnet only.",
    },
    {
      id: "free",
      rank: 1,
      title: "Free and private",
      promise: "A random name like brave.penguin@twelve.cash. Free, and nobody sees your payments.",
      cost: "Free",
      sees: NOBODY_SEES,
      redirect: { level: "warn", text: "twelve.cash could swap your offer, but only publicly: the record is signed for everyone to see, and Allod checks it" },
      ip: IP_HIDDEN,
      wallets: MODERN,
      catch: `You cannot pick, change or delete the name.`,
      actions: d.twelve_cash_available ? [{ label: "Get one", to: "twelve" }] : [],
      soon: d.twelve_cash_available ? undefined : "Mainnet only.",
    },
    {
      id: "hosted",
      rank: 0,
      title: "A name on a Lightning service",
      promise: "A name@zeuspay.com (automatic) or name@getalby.com (set up on their site) that every wallet can pay.",
      cost: "Free",
      sees: { level: "danger", text: "The service sees every payment (amount and comment)" },
      redirect: { level: "danger", text: "The service answers every payer and could quietly hand them its own invoice" },
      ip: IP_HIDDEN,
      wallets: EVERY,
      catch:
        "The service sees every payment (amount and comment) and could hand payers its own invoice instead of yours. One address per node; your node must be online to receive.",
      actions: (d.zeus_available && !d.zeus) || d.alby_available ? [{ label: "Choose", to: "hosted" }] : [],
      soon: d.zeus_available || d.alby_available ? undefined : "Mainnet only.",
    },
  ];
  if (d.prefs?.site !== "server") return steps;
  // On a rented server the hosting provider controls the machine: it can read every
  // payment and the node's keys, and change what the node answers, whatever the option.
  const provider = (sig: Signal, text: string): Signal => (sig.level === "danger" ? sig : { level: "danger", text });
  return steps.map((st) => ({
    ...st,
    sees: provider(st.sees, "The hosting provider can see every payment (it controls the server)"),
    redirect: provider(st.redirect, "The hosting provider controls the server and could swap your invoices or take the funds"),
  }));
}

/** The practical cost of an option: a soft amber box, readable but clearly a warning. */
export function CatchNote({ s }: { s: LadderStep }) {
  const bip353Only = s.wallets === MODERN;
  return (
    <span className="mt-1 flex items-start gap-1.5 rounded-md border border-warn/40 bg-warn/10 px-2 py-1.5 text-xs text-fg">
      <Warning size={14} weight="fill" className="mt-px shrink-0 text-warn" aria-label="Warning" />
      <span>
        {s.catch}
        {bip353Only && <span className="block">{BIP353_ONLY}</span>}
      </span>
    </span>
  );
}

const TONE: Record<Level, string> = { ok: "text-ok", warn: "text-warn", danger: "text-danger" };

function SignalIcon({ icon: I, signal, label }: { icon: Icon; signal: Signal; label: string }) {
  return (
    <span className={cn("inline-flex", TONE[signal.level])} title={`${label}: ${signal.text}`} aria-label={`${label}: ${signal.text}`}>
      <I size={18} weight="duotone" />
    </span>
  );
}

/** The four signals, in the order the icons show them, with the question each answers. */
const SIGNALS: { key: "sees" | "redirect" | "ip" | "wallets"; icon: Icon; label: string }[] = [
  { key: "sees", icon: Eye, label: "Who sees your payments" },
  { key: "redirect", icon: ShieldCheck, label: "Who could swap your invoices" },
  { key: "ip", icon: HouseLine, label: "Your IP address" },
  { key: "wallets", icon: Wallet, label: "Who can pay" },
];

export function Signals({ s }: { s: LadderStep }) {
  return (
    <span className="flex items-center gap-1.5">
      {SIGNALS.map((x) => (
        <SignalIcon key={x.key} icon={x.icon} signal={s[x.key]} label={x.label} />
      ))}
    </span>
  );
}

/** The four signals spelled out, one per line: the icon, the question, the answer. */
export function SignalList({ s }: { s: LadderStep }) {
  return (
    <ul className="space-y-1.5 text-sm">
      {SIGNALS.map((x) => {
        const sig = s[x.key];
        return (
          <li key={x.key} className="flex items-start gap-2">
            <span className={cn("mt-0.5 inline-flex shrink-0", TONE[sig.level])} aria-hidden>
              <x.icon size={18} weight="duotone" />
            </span>
            <span>
              <span className="text-muted">{x.label}: </span>
              <span className={TONE[sig.level]}>{sig.text}</span>
            </span>
          </li>
        );
      })}
    </ul>
  );
}


/** Explains the four icons, for the bottom of a list that shows them. */
export function SignalLegend() {
  const item = (I: Icon, text: string) => (
    <span className="inline-flex items-center gap-1">
      <I size={14} weight="duotone" /> {text}
    </span>
  );
  return (
    <div className="space-y-1 text-xs text-muted">
      <p className="flex flex-wrap gap-x-3 gap-y-1">
        {item(Eye, "who sees your payments")}
        {item(ShieldCheck, "whether a third party could swap your invoices")}
        {item(HouseLine, "whether your IP stays hidden")}
        {item(Wallet, "which wallets can pay")}
      </p>
      <p className="flex flex-wrap gap-x-3">
        <span className="text-ok">green: good</span>
        <span className="text-warn">amber: a limit</span>
        <span className="text-danger">red: a real cost</span>
        <span>· hover an icon for details</span>
      </p>
    </div>
  );
}
