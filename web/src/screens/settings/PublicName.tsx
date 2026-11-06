import { useState } from "react";
import useSWR from "swr";
import { ArrowsClockwise, EnvelopeSimple, Globe, LockSimple } from "@phosphor-icons/react";
import { SignerConnect } from "../../components/SignerConnect";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { CopyButton, Field, Modal, Tabs } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  fetcher,
  publicName,
  PUBLIC_NAME_KEY,
  type Bip353Record,
  type Exposure,
  type PublicName,
  type TwelveCashEntry,
} from "../../lib/api";
import { useAccounts } from "../../lib/account";
import { restoreSigner, type Signer } from "../../lib/signer";
import { SettingsGroup } from "./SettingsLayout";

function message(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

/** Public name for Lightning addresses (SPEC §12.1). */
export function PublicNameSettings() {
  const { data, error, mutate } = useSWR<PublicName>(PUBLIC_NAME_KEY, fetcher, {
    // While deSEC's email link is pending, each load also checks whether it was clicked.
    refreshInterval: (d) => (d?.desec.pending ? 15_000 : 60_000),
  });
  if (error) return <Alert tone="danger" title="Could not load the public name">{message(error)}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  const update = (v: PublicName) => void mutate(v, { revalidate: false });
  return (
    <SettingsGroup
      title="Public name"
      description="A domain for your accounts' Lightning addresses, like alice@yourname.dedyn.io."
    >
      {data.env_public_url ? (
        <Card>
          <CardContent className="space-y-1 pt-6 text-sm">
            <p>
              Set by <span className="font-mono">ALLOD_PUBLIC_URL</span> on this installation:{" "}
              <span className="font-mono">{data.name}</span>.
            </p>
            <p className="text-muted">Add the BIP-353 records below at your DNS provider.</p>
          </CardContent>
        </Card>
      ) : data.mode === "none" ? (
        <Choose data={data} onChange={update} />
      ) : data.mode === "desec" && data.desec.pending ? (
        <Pending data={data} onChange={update} />
      ) : (
        <Current data={data} onChange={update} />
      )}
      {data.mode !== "none" && !data.desec.pending && <Records data={data} />}
      {data.twelve_cash_available && <TwelveCash data={data} onChange={update} />}
    </SettingsGroup>
  );
}

function Choose({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const [tab, setTab] = useState<"desec" | "own">("desec");
  return (
    <Card>
      <CardHeader>
        <CardTitle>Choose a name</CardTitle>
        <CardDescription>
          Wallets find a Lightning address through DNS. Allod can get you a free name, or use a domain you own.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        <Tabs
          tabs={[
            { id: "desec", label: "Free name" },
            { id: "own", label: "My own domain" },
          ]}
          value={tab}
          onChange={setTab}
        />
        {tab === "desec" ? <DesecSignup data={data} onChange={onChange} /> : <OwnDomain data={data} onChange={onChange} />}
      </CardContent>
    </Card>
  );
}

function DesecSignup({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const [label, setLabel] = useState("");
  const [email, setEmail] = useState("");
  const [answer, setAnswer] = useState("");
  const {
    data: captcha,
    error: captchaError,
    mutate: newCaptcha,
  } = useSWR("desec-captcha", () => publicName.captcha(), { revalidateOnFocus: false, revalidateIfStale: false });
  const [available, setAvailable] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [useToken, setUseToken] = useState(false);

  const loadCaptcha = () => {
    setAnswer("");
    void newCaptcha();
  };

  if (useToken) return <TokenForm onChange={onChange} onBack={() => setUseToken(false)} />;

  const previous = data.desec.account_domain;
  const checkLabel = async () => {
    setAvailable(null);
    if (!label) return;
    try {
      const r = await publicName.available(label);
      setAvailable(r.valid ? r.available : false);
    } catch {
      setAvailable(null);
    }
  };

  return (
    <form
      className="space-y-4"
      onSubmit={async (e) => {
        e.preventDefault();
        if (!captcha) return;
        setBusy(true);
        setError(null);
        try {
          onChange(await publicName.signup({ label, email, captcha_id: captcha.id, captcha_solution: answer }));
        } catch (err) {
          setError(message(err));
          loadCaptcha();
        } finally {
          setBusy(false);
        }
      }}
    >
      <p className="text-sm text-muted">
        Names come from <a className="underline" href="https://desec.io" target="_blank" rel="noreferrer">deSEC</a>, a
        free DNS service run by a non-profit. Allod creates the deSEC account for you; you only confirm your email.
      </p>
      {previous && (
        <Alert tone="info" title={`You already have ${previous}`}>
          <Button
            size="sm"
            variant="secondary"
            className="mt-2"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              try {
                onChange(await publicName.set({ mode: "desec" }));
              } catch (err) {
                setError(message(err));
              } finally {
                setBusy(false);
              }
            }}
          >
            Use {previous} again
          </Button>
        </Alert>
      )}
      <Field
        label="Name"
        hint={
          available === false
            ? "This name is taken or not valid. Lowercase letters, digits and hyphens."
            : available
              ? "Available."
              : "Lowercase letters, digits and hyphens."
        }
      >
        <div className="flex items-center gap-2">
          <Input
            value={label}
            onChange={(e) => {
              setLabel(e.target.value.toLowerCase().replace(/[^a-z0-9-]/g, ""));
              setAvailable(null);
            }}
            onBlur={() => void checkLabel()}
            placeholder="mynode"
            maxLength={63}
            required
            autoComplete="off"
            aria-label="Name"
          />
          <span className="shrink-0 font-mono text-sm text-muted">.dedyn.io</span>
        </div>
      </Field>
      <Field label="Email" hint="deSEC sends one link to confirm it, and uses it only for account recovery.">
        <Input type="email" value={email} onChange={(e) => setEmail(e.target.value)} required autoComplete="email" />
      </Field>
      <Field label="Type the characters in the picture">
        <div className="space-y-2">
          <div className="flex items-center gap-2">
            {captcha ? (
              <img
                src={`data:image/png;base64,${captcha.challenge}`}
                alt="Captcha from deSEC"
                className="h-16 rounded-lg border border-line bg-white"
              />
            ) : (
              <span className="text-sm text-muted">{captchaError ? message(captchaError) : "Loading…"}</span>
            )}
            <Button variant="ghost" size="sm" onClick={loadCaptcha} aria-label="New picture">
              <ArrowsClockwise size={16} /> New picture
            </Button>
          </div>
          <Input value={answer} onChange={(e) => setAnswer(e.target.value)} required autoComplete="off" aria-label="Captcha answer" />
        </div>
      </Field>
      {error && <Alert tone="danger" title="Signup failed">{error}</Alert>}
      <div className="flex flex-wrap items-center gap-3">
        <Button type="submit" disabled={busy || !captcha || available === false}>
          {busy ? "Signing up…" : "Get the name"}
        </Button>
        <Button variant="ghost" size="sm" onClick={() => setUseToken(true)}>
          I already have a deSEC account
        </Button>
      </div>
    </form>
  );
}

function TokenForm({ onChange, onBack }: { onChange: (v: PublicName) => void; onBack: () => void }) {
  const [name, setName] = useState("");
  const [token, setToken] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <form
      className="space-y-4"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          onChange(await publicName.token(name, token));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <p className="text-sm text-muted">
        Create a token at{" "}
        <a className="underline" href="https://desec.io/tokens" target="_blank" rel="noreferrer">
          desec.io/tokens
        </a>
        , ideally limited to this domain.
      </p>
      <Field label="Domain">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="mynode.dedyn.io" required />
      </Field>
      <Field label="Token">
        <Input value={token} onChange={(e) => setToken(e.target.value)} required autoComplete="off" />
      </Field>
      {error && <Alert tone="danger" title="Could not use the token">{error}</Alert>}
      <div className="flex gap-3">
        <Button type="submit" disabled={busy}>
          Use this domain
        </Button>
        <Button variant="ghost" onClick={onBack}>
          Back
        </Button>
      </div>
    </form>
  );
}

function OwnDomain({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const [name, setName] = useState("");
  const [exposure, setExposure] = useState<Exposure>("private");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <form
      className="space-y-4"
      onSubmit={async (e) => {
        e.preventDefault();
        setBusy(true);
        setError(null);
        try {
          onChange(await publicName.set({ mode: "own", name, exposure }));
        } catch (err) {
          setError(message(err));
        } finally {
          setBusy(false);
        }
      }}
    >
      <Field label="Domain" hint="You add the DNS records at your provider; Allod lists them for you.">
        <Input value={name} onChange={(e) => setName(e.target.value)} placeholder="pay.example.com" required />
      </Field>
      <ExposureChoice value={exposure} publicAllowed={data.public_allowed.own} onChange={setExposure} />
      {error && <Alert tone="danger" title="Could not use the domain">{error}</Alert>}
      <Button type="submit" disabled={busy}>
        Use this domain
      </Button>
    </form>
  );
}

function ExposureChoice({
  value,
  publicAllowed,
  onChange,
}: {
  value: Exposure;
  publicAllowed: boolean;
  onChange: (e: Exposure) => void;
}) {
  const options: { id: Exposure; title: string; body: string; disabled?: boolean }[] = [
    {
      id: "private",
      title: "Private (recommended)",
      body: "Only BIP-353 records are published. Your IP address stays private and no port has to be open. Payers need a wallet with BIP-353 and BOLT12 (Phoenix, Zeus and others).",
    },
    {
      id: "public",
      title: "Public",
      body: publicAllowed
        ? "The name points at this machine and serves LNURL, so every Lightning wallet can pay. Port 443 must be reachable, and anyone who sees an address can learn this machine's IP address."
        : "Not available on this platform: it publishes Allod through its own proxy. Use your own domain instead.",
      disabled: !publicAllowed,
    },
  ];
  return (
    <fieldset className="space-y-2">
      <legend className="mb-1 text-sm font-medium">Exposure</legend>
      {options.map((o) => (
        <label
          key={o.id}
          className={`flex gap-3 rounded-lg border p-3 text-sm ${value === o.id ? "border-accent bg-accent-soft/40" : "border-line"} ${o.disabled ? "opacity-60" : "cursor-pointer"}`}
        >
          <input
            type="radio"
            name="exposure"
            className="mt-1"
            checked={value === o.id}
            disabled={o.disabled}
            onChange={() => onChange(o.id)}
          />
          <span>
            <span className="font-medium">{o.title}</span>
            <span className="block text-muted">{o.body}</span>
          </span>
        </label>
      ))}
    </fieldset>
  );
}

function Pending({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const pending = data.desec.pending!;
  const [busy, setBusy] = useState(false);
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex items-center gap-2">
          <EnvelopeSimple size={20} weight="duotone" /> Confirm your email
        </CardTitle>
        <CardDescription>
          deSEC sent a link to <span className="font-medium text-fg">{pending.email}</span>. Click it (it is valid for 12
          hours). This page continues by itself.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3 text-sm">
        <p>
          Your name: <span className="font-mono">{data.name}</span>
        </p>
        <p className="text-muted">No email? Check the spam folder, or start again with a new picture.</p>
        <Button
          variant="secondary"
          size="sm"
          disabled={busy}
          onClick={async () => {
            setBusy(true);
            try {
              onChange(await publicName.set({ mode: "none" }));
            } finally {
              setBusy(false);
            }
          }}
        >
          Start again
        </Button>
      </CardContent>
    </Card>
  );
}

type ReachState = NonNullable<PublicName["status"]["reach"]>["state"];

const REACH: Record<ReachState, { tone: "ok" | "warn" | "danger"; label: string }> = {
  reachable: { tone: "ok", label: "Reachable" },
  waiting_for_certificate: { tone: "warn", label: "Waiting for the certificate" },
  not_reachable: { tone: "danger", label: "Not reachable from the internet" },
};

function Current({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmPublic, setConfirmPublic] = useState(false);
  const [confirmOff, setConfirmOff] = useState(false);
  const allowed = data.mode === "own" ? data.public_allowed.own : data.public_allowed.desec;
  const reach = data.status.reach;
  const run = async (f: () => Promise<PublicName>) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await f());
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  const setExposure = (exposure: Exposure) => {
    if (exposure === data.exposure) return;
    if (exposure === "public") setConfirmPublic(true);
    else void run(() => publicName.set({ mode: data.mode, name: data.name ?? undefined, exposure }));
  };
  return (
    <Card>
      <CardHeader>
        <CardTitle className="flex flex-wrap items-center gap-2">
          {data.exposure === "public" ? <Globe size={20} weight="duotone" /> : <LockSimple size={20} weight="duotone" />}
          <span className="font-mono">{data.name}</span>
          {data.mode === "desec" && <Badge tone="accent">deSEC</Badge>}
          {reach && <Badge tone={REACH[reach.state].tone}>{REACH[reach.state].label}</Badge>}
        </CardTitle>
        <CardDescription>
          Accounts with a username receive at <span className="font-mono">username@{data.name}</span>.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-4">
        {data.mode === "desec" && !data.desec.has_token && (
          <Alert tone="warn" title="No deSEC token">
            Allod cannot update this name. Use a token from desec.io/tokens, or turn the name off and sign up again.
          </Alert>
        )}
        {data.status.errors.map((e) => (
          <Alert key={e} tone="warn" title="Problem">
            {e}
          </Alert>
        ))}
        <ExposureChoice value={data.exposure} publicAllowed={allowed} onChange={setExposure} />
        {data.exposure === "public" && <PublicHints data={data} />}
        {error && <Alert tone="danger" title="Could not save">{error}</Alert>}
        <div className="flex flex-wrap gap-2">
          <Button variant="secondary" size="sm" disabled={busy} onClick={() => void run(publicName.refresh)}>
            <ArrowsClockwise size={16} /> Check now
          </Button>
          {data.desec.account_email && <DesecLogin />}
          <Button variant="ghost" size="sm" disabled={busy} onClick={() => setConfirmOff(true)}>
            Turn off
          </Button>
        </div>
        {data.mode === "desec" && (
          <p className="text-xs text-muted">
            deSEC is a donation-funded non-profit.{" "}
            <a className="underline" href="https://desec.io/donate" target="_blank" rel="noreferrer">
              Support them
            </a>
            .
          </p>
        )}
      </CardContent>
      <Modal open={confirmPublic} onClose={() => setConfirmPublic(false)} title="Make the name public?">
        <div className="space-y-4 text-sm">
          <p>
            <span className="font-mono">{data.name}</span> will point at this machine's public IP address. Anyone who sees
            one of your Lightning addresses can look it up and learn where this node runs.
          </p>
          <p className="text-muted">
            Port 443 must reach this machine (port forward on a home router) and the stack's https profile must be on.
            Connections behind CGNAT cannot receive incoming connections.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirmPublic(false)}>
              Cancel
            </Button>
            <Button
              onClick={() => {
                setConfirmPublic(false);
                void run(() => publicName.set({ mode: data.mode, name: data.name ?? undefined, exposure: "public" }));
              }}
            >
              Make public
            </Button>
          </div>
        </div>
      </Modal>
      <Modal open={confirmOff} onClose={() => setConfirmOff(false)} title="Turn the public name off?">
        <div className="space-y-4 text-sm">
          <p>Lightning addresses on {data.name} stop working.</p>
          {data.mode === "desec" && (
            <p className="text-muted">
              Allod deletes the BIP-353 records it wrote. The deSEC account stays, so you can use the name again later.
            </p>
          )}
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirmOff(false)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              onClick={() => {
                setConfirmOff(false);
                void run(() => publicName.set({ mode: "none" }));
              }}
            >
              Turn off
            </Button>
          </div>
        </div>
      </Modal>
    </Card>
  );
}

function PublicHints({ data }: { data: PublicName }) {
  const reach = data.status.reach;
  return (
    <div className="space-y-2 text-sm">
      {data.mode === "desec" ? (
        data.status.ipv4 && (
          <p className="text-muted">
            Points at <span className="font-mono">{data.status.ipv4}</span>
            {data.status.ipv6 && (
              <>
                {" "}
                and <span className="font-mono">{data.status.ipv6}</span>
              </>
            )}
            ; Allod updates it when the address changes.
          </p>
        )
      ) : (
        <p className="text-muted">
          At your DNS provider, point an A record for <span className="font-mono">{data.name}</span> at this machine's
          public IP address.
        </p>
      )}
      {reach?.state === "not_reachable" && (
        <Alert tone="warn" title="Let's Encrypt could not reach this machine">
          Forward port 443 to this machine and turn on the stack's https profile. Connections behind CGNAT cannot be
          reached; use the private exposure there.
          {reach.detail && <span className="mt-1 block font-mono text-xs text-muted">{reach.detail}</span>}
        </Alert>
      )}
    </div>
  );
}

function DesecLogin() {
  const [open, setOpen] = useState(false);
  const [signer, setSigner] = useState<Signer | null>(() => restoreSigner());
  const [login, setLogin] = useState<{ email: string; password: string } | null>(null);
  const [error, setError] = useState<string | null>(null);
  const close = () => {
    setOpen(false);
    setLogin(null);
    setError(null);
  };
  return (
    <>
      <Button variant="ghost" size="sm" onClick={() => setOpen(true)}>
        deSEC login
      </Button>
      <Modal open={open} onClose={close} title="deSEC login">
        <div className="space-y-4 text-sm">
          {login ? (
            <>
              <p>Sign in at desec.io with these, for example to delete the account.</p>
              <Field label="Email">
                <div className="flex gap-2">
                  <Input readOnly value={login.email} />
                  <CopyButton value={login.email} />
                </div>
              </Field>
              <Field label="Password">
                <div className="flex gap-2">
                  <Input readOnly value={login.password} className="font-mono" />
                  <CopyButton value={login.password} />
                </div>
              </Field>
            </>
          ) : signer ? (
            <>
              <p>Sign with your Nostr key to show the password Allod created.</p>
              {error && <Alert tone="danger" title="Could not show it">{error}</Alert>}
              <Button
                onClick={async () => {
                  try {
                    setLogin(await publicName.credentials(signer));
                  } catch (e) {
                    setError(message(e));
                  }
                }}
              >
                Sign and show
              </Button>
            </>
          ) : (
            <SignerConnect onConnected={(s) => setSigner(s)} />
          )}
        </div>
      </Modal>
    </>
  );
}

function Records({ data }: { data: PublicName }) {
  const own = data.mode === "own" || !!data.env_public_url;
  return (
    <Card>
      <CardHeader>
        <CardTitle>Lightning addresses</CardTitle>
        <CardDescription>
          {own
            ? "Add one TXT record per address at your DNS provider. The domain must be signed with DNSSEC."
            : "Allod publishes these BIP-353 records for every account with a username."}
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {data.records.length === 0 && (
          <p className="text-sm text-muted">No account has a username yet. Give one to an account when you create it.</p>
        )}
        {data.records.map((r) => (
          <RecordRow key={r.account_id} record={r} own={own} />
        ))}
      </CardContent>
    </Card>
  );
}

function RecordRow({ record, own }: { record: Bip353Record; own: boolean }) {
  return (
    <div className="space-y-1 rounded-lg border border-line p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="font-medium">{record.address}</span>
        <span className="flex items-center gap-2">
          <span className="text-xs text-muted">{record.account_name}</span>
          {!own && <Badge tone={record.published ? "ok" : "warn"}>{record.published ? "Published" : "Pending"}</Badge>}
          <CopyButton value={record.address} label="Copy address" />
        </span>
      </div>
      {own && (
        <div className="space-y-1">
          <div className="flex items-center gap-2">
            <span className="w-10 shrink-0 text-xs text-muted">Name</span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs">{record.name}</span>
            <CopyButton value={record.name} />
          </div>
          <div className="flex items-center gap-2">
            <span className="w-10 shrink-0 text-xs text-muted">TXT</span>
            <span className="min-w-0 flex-1 truncate font-mono text-xs">{record.txt}</span>
            <CopyButton value={record.txt} />
          </div>
        </div>
      )}
    </div>
  );
}

/** Free BIP-353 names on twelve.cash for Main and each account (SPEC §12.3). */
function TwelveCash({ data, onChange }: { data: PublicName; onChange: (v: PublicName) => void }) {
  const { open } = useAccounts();
  const targets = [{ id: "main", name: "Main" }, ...open.map((a) => ({ id: String(a.id), name: a.name }))];
  const byTarget = new Map(data.twelve_cash.map((e) => [e.target, e]));
  return (
    <Card>
      <CardHeader>
        <CardTitle>twelve.cash addresses</CardTitle>
        <CardDescription>
          A free address like <span className="font-mono">brave.penguin@twelve.cash</span> that points at a wallet's
          BOLT12 offer. No domain, DNS or open port needed. Run by a third party:{" "}
          <a className="underline" href="https://twelve.cash" target="_blank" rel="noreferrer">
            twelve.cash
          </a>
          . Payers need a wallet with BIP-353 and BOLT12.
        </CardDescription>
      </CardHeader>
      <CardContent className="space-y-3">
        {targets.map((t) => (
          <TwelveRow key={t.id} target={t.id} name={t.name} entry={byTarget.get(t.id)} onChange={onChange} />
        ))}
      </CardContent>
    </Card>
  );
}

function TwelveRow({
  target,
  name,
  entry,
  onChange,
}: {
  target: string;
  name: string;
  entry: TwelveCashEntry | undefined;
  onChange: (v: PublicName) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [linking, setLinking] = useState(false);
  const run = async (f: () => Promise<PublicName>) => {
    setBusy(true);
    setError(null);
    try {
      onChange(await f());
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };
  return (
    <div className="space-y-2 rounded-lg border border-line p-3 text-sm">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <span className="font-medium">{name}</span>
        {entry ? (
          <span className="flex flex-wrap items-center gap-2">
            <span className="font-mono">{entry.address}</span>
            {!entry.active ? (
              <Badge tone="danger">Offer disabled</Badge>
            ) : entry.verified ? (
              <Badge tone="ok">Live</Badge>
            ) : (
              <Badge tone="warn">Waiting for DNS</Badge>
            )}
            <CopyButton value={entry.address} />
            <Button variant="ghost" size="sm" disabled={busy} onClick={() => void run(() => publicName.twelveForget(target))}>
              Forget
            </Button>
          </span>
        ) : (
          <span className="flex flex-wrap gap-2">
            <Button size="sm" disabled={busy} onClick={() => void run(() => publicName.twelveAdd(target))}>
              {busy ? "Registering…" : "Get a free address"}
            </Button>
            <Button variant="ghost" size="sm" onClick={() => setLinking(true)}>
              Use a name I bought
            </Button>
          </span>
        )}
      </div>
      {entry && !entry.active && (
        <p className="text-xs text-muted">
          This wallet's offer was disabled, so payments to the address fail. twelve.cash cannot delete it; stop sharing it.
        </p>
      )}
      {error && <Alert tone="danger" title="twelve.cash">{error}</Alert>}
      <LinkTwelve
        open={linking}
        target={target}
        name={name}
        onClose={() => setLinking(false)}
        onLinked={(v) => {
          setLinking(false);
          onChange(v);
        }}
      />
    </div>
  );
}

function LinkTwelve({
  open,
  target,
  name,
  onClose,
  onLinked,
}: {
  open: boolean;
  target: string;
  name: string;
  onClose: () => void;
  onLinked: (v: PublicName) => void;
}) {
  const { data: offer, error: offerError } = useSWR(open ? ["twelve-offer", target] : null, () =>
    publicName.twelveOffer(target),
  );
  const [address, setAddress] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  return (
    <Modal open={open} onClose={onClose} title={`Custom twelve.cash name for ${name}`}>
      <form
        className="space-y-4 text-sm"
        onSubmit={async (e) => {
          e.preventDefault();
          setBusy(true);
          setError(null);
          try {
            onLinked(await publicName.twelveAdd(target, address));
          } catch (err) {
            setError(message(err));
          } finally {
            setBusy(false);
          }
        }}
      >
        <p>
          1. Buy a name at{" "}
          <a className="underline" href="https://twelve.cash" target="_blank" rel="noreferrer">
            twelve.cash
          </a>{" "}
          and paste this BOLT12 offer as its payment option:
        </p>
        {offer ? (
          <div className="flex items-center gap-2">
            <span className="min-w-0 flex-1 truncate font-mono text-xs">{offer.offer}</span>
            <CopyButton value={offer.offer} />
          </div>
        ) : (
          <p className="text-muted">{offerError ? message(offerError) : "Loading…"}</p>
        )}
        <p>2. Once it is active, enter the address. Allod checks that it points at this offer.</p>
        <Input value={address} onChange={(e) => setAddress(e.target.value)} placeholder="satoshi@twelve.cash" required />
        {error && <Alert tone="danger" title="Could not link it">{error}</Alert>}
        <div className="flex justify-end gap-2">
          <Button variant="ghost" onClick={onClose}>
            Cancel
          </Button>
          <Button type="submit" disabled={busy}>
            Link
          </Button>
        </div>
      </form>
    </Modal>
  );
}
