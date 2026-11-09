import { useState } from "react";
import { ArrowLeft, ArrowRight, PencilSimple } from "@phosphor-icons/react";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { publicName, type AddressPrefs, type PublicName } from "../../lib/api";
import { CatchNote, ladder, SignalLegend, Signals, type LadderAction, type LadderStep } from "./AddressLadder";

/**
 * "Add an address" as a few questions (SPEC §13): who must be able to pay, whether the
 * owner has (or will buy) a domain, where the node runs, and whether to choose the name.
 * The answers narrow the options to one recommendation (plus an alternative or a "for
 * now" option). Answers are remembered, so the next addition skips them.
 */

type Wallets = NonNullable<AddressPrefs["wallets"]>;
type Domain = NonNullable<AddressPrefs["domain"]>;
type Site = NonNullable<AddressPrefs["site"]>;

/** One recommendation: a ladder step and what to open. */
interface Pick {
  step: string;
  to: LadderAction | null;
  label?: string;
}
interface Outcome {
  heading: string;
  main: Pick;
  alt?: Pick;
  domainGuide?: boolean;
}

function outcomes(w: Wallets, domain: Domain, site: Site | null, choose: boolean | null): Outcome[] {
  const hasDomain = domain !== "no";
  const modern = (): Outcome =>
    hasDomain
      ? {
          heading: "Private, for wallets with BIP-353",
          main: { step: "domain", to: "own", label: "Use my domain" },
          domainGuide: domain === "buy",
        }
      : choose
        ? {
            heading: "Private, with a name you choose",
            main: { step: "picked", to: "clank", label: "clank.money (~1k sats, can be changed)" },
            alt: { step: "picked", to: "twelve-custom", label: "twelve.cash (5,000 sats, permanent)" },
          }
        : { heading: "Private and free", main: { step: "free", to: "twelve" } };
  const every = (): Outcome =>
    hasDomain
      ? site === "server"
        ? {
            heading: "For every wallet",
            main: { step: "domain-lnurl", to: "own-lnurl", label: "Use my domain" },
            domainGuide: domain === "buy",
          }
        : {
            heading: "For every wallet",
            main: { step: "relay", to: null },
            alt: { step: "domain-lnurl", to: "own-lnurl", label: "Served from home" },
            domainGuide: domain === "buy",
          }
      : { heading: "For every wallet", main: { step: "hosted", to: "hosted" } };
  if (w === "modern") return [modern()];
  if (w === "any") return [every()];
  return [modern(), every()];
}

function Question<T extends string>({
  title,
  hint,
  options,
  onAnswer,
}: {
  title: string;
  hint?: string;
  options: { id: T; label: string; body: string }[];
  onAnswer: (v: T) => void;
}) {
  return (
    <div className="space-y-3">
      <div>
        <p className="text-base font-semibold">{title}</p>
        {hint && <p className="text-muted">{hint}</p>}
      </div>
      <div className="space-y-2">
        {options.map((o) => (
          <button
            key={o.id}
            type="button"
            onClick={() => onAnswer(o.id)}
            className="flex w-full items-center justify-between gap-3 rounded-lg border border-line p-4 text-left transition hover:border-accent"
          >
            <span>
              <span className="block font-medium">{o.label}</span>
              <span className="block text-muted">{o.body}</span>
            </span>
            <ArrowRight size={18} className="shrink-0 text-muted" />
          </button>
        ))}
      </div>
    </div>
  );
}

const WALLETS_LABEL: Record<Wallets, string> = {
  any: "Every wallet",
  modern: "Modern wallets",
  both: "Not sure: both",
};
const DOMAIN_LABEL: Record<Domain, string> = { yes: "Has a domain", buy: "Will buy a domain", no: "No domain" };
const SITE_LABEL: Record<Site, string> = { home: "Runs at home", server: "Runs on a rented server" };

export function AddressQuestions({
  data,
  domainNames,
  onChange,
  onPick,
  onAllOptions,
}: {
  data: PublicName;
  /** The owner's domains (Settings → Domains). */
  domainNames: string[];
  onChange: (v: PublicName) => void;
  onPick: (to: LadderAction) => void;
  onAllOptions: () => void;
}) {
  const knownDomain = domainNames.length > 0;
  const [wallets, setWallets] = useState<Wallets | null>(data.prefs?.wallets ?? null);
  // A saved domain answers "yes" unless the owner chooses to answer again.
  const [domain, setDomain] = useState<Domain | null>(knownDomain ? "yes" : (data.prefs?.domain ?? null));
  const [askDomain, setAskDomain] = useState(false);
  const [site, setSite] = useState<Site | null>(data.prefs?.site ?? null);
  // Not remembered: it depends on the name wanted this time.
  const [choose, setChoose] = useState<boolean | null>(null);
  const steps = ladder(data);
  const save = (p: Partial<AddressPrefs>) => {
    void publicName.setPrefs(p).then(onChange).catch(() => undefined);
  };
  const restart = () => {
    setWallets(null);
    setDomain(null);
    setAskDomain(true);
    setSite(null);
    setChoose(null);
    void publicName.resetPrefs().then(onChange).catch(() => undefined);
  };

  const answered = [
    wallets && WALLETS_LABEL[wallets],
    domain && (knownDomain && domain === "yes" ? `Domain ${domainNames.join(", ")}` : DOMAIN_LABEL[domain]),
    site && SITE_LABEL[site],
  ].filter(Boolean);
  const header = answered.length > 0 && (
    <div className="flex flex-wrap items-center gap-2 text-xs">
      {answered.map((a) => (
        <Badge key={a}>{a}</Badge>
      ))}
      <Button variant="ghost" size="sm" onClick={restart}>
        <PencilSimple size={14} /> Change answers
      </Button>
    </div>
  );
  // Back: undo the most recent answer that led here (choose → site → domain → wallets).
  const siteAsked = wallets !== "modern" && domain !== null && domain !== "no";
  const back = (() => {
    if (choose !== null) return () => setChoose(null);
    if (site && siteAsked) return () => setSite(null);
    if (domain) {
      return () => {
        setDomain(null);
        setAskDomain(true);
      };
    }
    if (wallets) return () => setWallets(null);
    return null;
  })();
  const footer = (
    <div className="flex flex-wrap items-center gap-2">
      {back && (
        <Button variant="ghost" size="sm" onClick={back}>
          <ArrowLeft size={14} /> Back
        </Button>
      )}
      <Button variant="ghost" size="sm" onClick={onAllOptions}>
        See all options instead
      </Button>
    </div>
  );

  let body;
  if (!wallets) {
    body = (
      <Question<Wallets>
        title="Who should be able to pay this address?"
        options={[
          { id: "any", label: "Anyone, with any Lightning wallet", body: "Works with every wallet. Some options let a service see your payments." },
          {
            id: "modern",
            label: "People with modern wallets is enough",
            body: "Wallets with BIP-353 (Phoenix, Zeus and others). The most private kind: nobody sees your payments.",
          },
          { id: "both", label: "Not sure", body: "Get one of each: a private one, and one every wallet can pay." },
        ]}
        onAnswer={(v) => {
          setWallets(v);
          save({ wallets: v });
        }}
      />
    );
  } else if (!domain) {
    body = (
      <Question<Domain>
        title="Do you have a domain name?"
        hint="Like example.com. Your own domain gives the most private addresses, with any name you like."
        options={[
          knownDomain && askDomain
            ? {
                id: "yes" as const,
                label: `Yes, ${domainNames.join(", ")}`,
                body: "Add more domains in Settings → Domains.",
              }
            : { id: "yes" as const, label: "Yes", body: "Its DNS must support DNSSEC." },
          { id: "buy", label: "No, but I'm willing to buy one", body: "Allod explains what to look for." },
          { id: "no", label: "No, and I don't want one", body: "Use a name on a registry or a Lightning service instead." },
        ]}
        onAnswer={(v) => {
          setDomain(v);
          save({ domain: v });
        }}
      />
    );
  } else if (wallets !== "modern" && domain !== "no" && !site) {
    body = (
      <Question<Site>
        title="Where does this node run?"
        hint="For every wallet to pay through your domain, something must answer them over HTTPS."
        options={[
          { id: "home", label: "At home", body: "A home connection: its IP changes and should stay private." },
          {
            id: "server",
            label: "On a rented server (VPS)",
            body: "A fixed public IP in a data centre. The hosting provider can read the server's memory and disk, including the node's keys: you trust it with your funds.",
          },
        ]}
        onAnswer={(v) => {
          setSite(v);
          save({ site: v });
        }}
      />
    );
  } else if (wallets !== "any" && domain === "no" && choose === null) {
    body = (
      <Question<"random" | "choose">
        title="Do you want to choose the name?"
        options={[
          { id: "random", label: "No, a random free name is fine", body: "Like brave.penguin@twelve.cash." },
          { id: "choose", label: "Yes, I'll pay for a name I choose", body: "From about 1,000 sats, once." },
        ]}
        onAnswer={(v) => setChoose(v === "choose")}
      />
    );
  } else {
    const list = outcomes(wallets, domain, site, choose);
    body = (
      <div className="space-y-4">
        {list.some((o) => o.domainGuide) && <DomainGuide />}
        {list.map((o) => (
          <OutcomeCard key={o.heading} o={o} steps={steps} onPick={onPick} />
        ))}
        <SignalLegend />
      </div>
    );
  }
  return (
    <div className="space-y-4 text-sm">
      {header}
      {body}
      <div className="flex justify-start">{footer}</div>
    </div>
  );
}

function stepOf(steps: LadderStep[], id: string): LadderStep | undefined {
  return steps.find((s) => s.id === id);
}

/** One option with its full picture: what it is, its four signals, cost and warning. */
function OptionBlock({
  p,
  steps,
  onPick,
  primary,
}: {
  p: Pick;
  steps: LadderStep[];
  onPick: (to: LadderAction) => void;
  primary?: boolean;
}) {
  const s = stepOf(steps, p.step);
  if (!s) return null;
  const ready = p.to && s.actions.length > 0;
  return (
    <div className="space-y-2">
      <div className="flex flex-wrap items-start justify-between gap-3">
        <div className="min-w-0 space-y-1">
          <p className={primary ? "text-base font-semibold" : "font-semibold"}>{p.label ?? s.title}</p>
          <p className="text-muted">{s.promise}</p>
          <div className="flex flex-wrap items-center gap-2">
            <Signals s={s} />
            <Badge tone={s.cost === "Free" ? "ok" : "neutral"}>{s.cost}</Badge>
          </div>
        </div>
        {ready && p.to ? (
          <Button variant={primary ? "primary" : "secondary"} onClick={() => onPick(p.to as LadderAction)}>
            Set it up
          </Button>
        ) : (
          <Badge>{s.upcoming ? "Coming soon" : "Not available here"}</Badge>
        )}
      </div>
      {s.catch && <CatchNote s={s} />}
    </div>
  );
}

function OutcomeCard({ o, steps, onPick }: { o: Outcome; steps: LadderStep[]; onPick: (to: LadderAction) => void }) {
  if (!stepOf(steps, o.main.step)) return null;
  return (
    <div className="space-y-3 rounded-lg border border-accent/50 p-4">
      <p className="text-xs uppercase tracking-wide text-muted">{o.heading}</p>
      <OptionBlock p={o.main} steps={steps} onPick={onPick} primary />
      {o.alt && (
        <div className="space-y-2 rounded-lg border border-line p-3">
          <p className="text-xs uppercase tracking-wide text-muted">Or</p>
          <OptionBlock p={o.alt} steps={steps} onPick={onPick} />
        </div>
      )}
    </div>
  );
}

function DomainGuide() {
  return (
    <div className="space-y-2 rounded-lg bg-surface-2 p-4">
      <p className="font-semibold">Getting a domain</p>
      <ul className="space-y-1 text-muted">
        <li>• Pick a registrar or DNS host that supports DNSSEC: BIP-353 addresses need it.</li>
        <li>
          • At Njalla (takes bitcoin), Allod can keep the DNS records for you with an API token: no manual record
          editing.
        </li>
        <li>• The price depends on the name and the registrar.</li>
      </ul>
      <p className="text-muted">
        Once you have it, come back to "Add": Allod remembers your answers and starts at your domain.
      </p>
    </div>
  );
}
