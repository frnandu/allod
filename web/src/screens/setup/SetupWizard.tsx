import {
  AddressBook,
  ArrowLeft,
  ArrowRight,
  CheckCircle,
  Eye,
  EyeSlash,
  Key,
  Lightning,
  LockKey,
  ShieldCheck,
  Spinner,
  Sparkle,
} from "@phosphor-icons/react";
import { nip19 } from "nostr-tools";
import { useEffect, useMemo, useState, type ReactNode } from "react";
import useSWR from "swr";
import { Brand } from "../../components/Brand";
import { feeEstimateSummary, useFeeEstimates } from "../../components/FeeEstimates";
import { SignerConnect } from "../../components/SignerConnect";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { Input, Label, Textarea } from "../../components/ui/input";
import {
  fetcher,
  runSetup,
  type InfoResponse,
  type SetupBody,
  type FastSyncInfo,
  type FastSyncStatus,
  type SetupProgress,
} from "../../lib/api";
import { api } from "../../lib/api";
import { etaSeconds, formatEta, recordProgress, useProgressSamples } from "../../lib/eta";
import type { Signer } from "../../lib/signer";
import { cn, shorten } from "../../lib/utils";

type Step = "welcome" | "signer" | "phrase" | "backup" | "confirm" | "sync" | "done";
const ORDER: Step[] = ["welcome", "signer", "phrase", "backup", "confirm", "sync", "done"];

function pickPositions(count: number, picks: number): number[] {
  const out = new Set<number>();
  const rand = new Uint32Array(16);
  crypto.getRandomValues(rand);
  let i = 0;
  while (out.size < Math.min(picks, count)) out.add(rand[i++ % rand.length]! % count);
  return [...out].sort((a, b) => a - b);
}

function Shell({ step, children }: { step: Step; children: ReactNode }) {
  const index = ORDER.indexOf(step);
  return (
    <div className="v-grain flex min-h-screen flex-col items-center px-4 py-10">
      <Brand className="mb-8" />
      <div className="mb-6 flex gap-1.5" aria-label={`Step ${index + 1} of ${ORDER.length}`}>
        {ORDER.map((s, i) => (
          <span key={s} className={cn("h-1.5 w-8 rounded-full", i <= index ? "bg-accent" : "bg-line")} />
        ))}
      </div>
      <Card className="w-full max-w-xl">
        <CardContent className="space-y-6 p-6 sm:p-8">{children}</CardContent>
      </Card>
    </div>
  );
}

function Heading({ icon, title, children }: { icon: ReactNode; title: string; children?: ReactNode }) {
  return (
    <div className="space-y-2">
      <div className="flex size-11 items-center justify-center rounded-full bg-accent-soft text-accent">{icon}</div>
      <h1 className="text-2xl font-semibold">{title}</h1>
      {children && <p className="text-muted">{children}</p>}
    </div>
  );
}

export function SetupWizard({ info }: { info: InfoResponse }) {
  const [step, setStep] = useState<Step>("welcome");
  const [signer, setSigner] = useState<Signer | null>(null);
  const [pubkey, setPubkey] = useState("");
  const [phrase, setPhrase] = useState<string | null>(null);
  const managed = info.deployment.mode === "managed";

  switch (step) {
    case "welcome":
      return (
        <Shell step={step}>
          <Heading icon={<Sparkle size={22} weight="duotone" />} title="Set up your node">
            Allod runs your own Lightning node on {info.network ?? "Bitcoin"}. This takes a few minutes.
          </Heading>
          <Alert tone="info" title="Allod trusts the machine it runs on">
            Your keys are stored on this machine without an extra unlock step, so anyone with access to its disk
            can reach your funds. Use full-disk encryption on hardware you own, or a VPS provider you trust with
            your keys.
          </Alert>
          <Button size="lg" className="w-full" onClick={() => setStep("signer")}>
            Get started <ArrowRight size={18} />
          </Button>
        </Shell>
      );

    case "signer":
      return (
        <Shell step={step}>
          <Heading icon={<Key size={22} weight="duotone" />} title="Connect your Nostr signer">
            Your Nostr key becomes the owner of this node.
          </Heading>
          <ul className="space-y-2 rounded-lg border border-line bg-surface-2/40 p-4 text-sm">
            <li className="flex gap-2">
              <LockKey size={18} weight="duotone" className="mt-0.5 shrink-0 text-accent" />
              <span>You sign in with it instead of a password.</span>
            </li>
            <li className="flex gap-2">
              <AddressBook size={18} weight="duotone" className="mt-0.5 shrink-0 text-accent" />
              <span>
                The people you follow on Nostr become your contacts: in Send, type a name and pay them through the BOLT12 offer or
                Lightning address in their profile. Your node reads your public follow list and their public profiles from Nostr
                relays; your private key never leaves your signer.
              </span>
            </li>
          </ul>
          {signer ? (
            <div className="space-y-4">
              <Alert tone="info" title="Owner key connected">
                <span className="font-mono text-xs">{shorten(nip19.npubEncode(pubkey), 14, 8)}</span>
              </Alert>
              <Button size="lg" className="w-full" onClick={() => setStep("phrase")}>
                Continue <ArrowRight size={18} />
              </Button>
            </div>
          ) : (
            <SignerConnect
              onConnected={(s, pk) => {
                setSigner(s);
                setPubkey(pk);
              }}
            />
          )}
        </Shell>
      );

    case "phrase":
      return (
        <Shell step={step}>
          <PhraseStep
            managed={managed}
            onBack={() => setStep("signer")}
            onSubmit={async (body) => {
              const res = await runSetup(signer!, body);
              if (res.recovery_phrase) {
                setPhrase(res.recovery_phrase);
                setStep("backup");
              } else {
                setStep("sync");
              }
            }}
          />
        </Shell>
      );

    case "backup":
      return (
        <Shell step={step}>
          <BackupStep phrase={phrase!} onNext={() => setStep("confirm")} />
        </Shell>
      );

    case "confirm":
      return (
        <Shell step={step}>
          <ConfirmStep phrase={phrase!} onBack={() => setStep("backup")} onNext={() => {
            setPhrase(null);
            setStep("sync");
          }} />
        </Shell>
      );

    case "sync":
      return (
        <Shell step={step}>
          <SyncChoiceStep onNext={() => setStep("done")} />
        </Shell>
      );

    case "done":
      return (
        <Shell step={step}>
          <DoneStep onFinish={() => window.location.assign("/")} />
        </Shell>
      );
  }
}

function PhraseStep({
  managed,
  onBack,
  onSubmit,
}: {
  managed: boolean;
  onBack: () => void;
  onSubmit: (body: SetupBody) => Promise<void>;
}) {
  const [choice, setChoice] = useState<"create" | "restore">("create");
  const [words, setWords] = useState("");
  const [birthday, setBirthday] = useState("");
  const [ldk, setLdk] = useState({ address: "", cert: "", macaroon: "" });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const wordCount = words.trim() ? words.trim().split(/\s+/).length : 0;

  async function submit() {
    setError(null);
    setBusy(true);
    try {
      let body: SetupBody;
      if (managed) {
        body =
          choice === "create"
            ? { mode: "create" }
            : {
                mode: "restore",
                recovery_phrase: words.trim(),
                ...(birthday ? { birthday_height: Number(birthday) } : {}),
              };
      } else {
        body = {
          mode: "standalone",
          ldk_server_address: ldk.address.trim(),
          ldk_server_tls_cert_pem: ldk.cert,
          ldk_server_macaroon_hex: ldk.macaroon.trim(),
          ...(choice === "restore" ? { recovery_phrase: words.trim() } : {}),
        };
      }
      await onSubmit(body);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const restoreValid = choice === "create" || wordCount === 12 || wordCount === 24;
  const ldkValid = managed || (ldk.address && ldk.cert && ldk.macaroon);

  return (
    <div className="space-y-6">
      <Heading icon={<LockKey size={22} weight="duotone" />} title="Recovery phrase">
        One phrase restores your node{managed ? " and its Lightning keys" : ""}. Keep it offline.
      </Heading>

      {!managed && (
        <div className="space-y-3">
          <p className="text-sm font-medium">Your ldk-server</p>
          <Input placeholder="host:port" value={ldk.address} onChange={(e) => setLdk({ ...ldk, address: e.target.value })} />
          <Textarea placeholder="TLS certificate (PEM)" value={ldk.cert} onChange={(e) => setLdk({ ...ldk, cert: e.target.value })} />
          <Input placeholder="Macaroon (hex)" value={ldk.macaroon} onChange={(e) => setLdk({ ...ldk, macaroon: e.target.value })} />
        </div>
      )}

      <div className="grid gap-3 sm:grid-cols-2" role="radiogroup">
        {(["create", "restore"] as const).map((c) => (
          <button
            key={c}
            type="button"
            role="radio"
            aria-checked={choice === c}
            onClick={() => setChoice(c)}
            className={cn(
              "rounded-[var(--radius-card)] border p-4 text-left transition",
              choice === c ? "border-accent bg-accent-soft/50" : "border-line hover:border-accent/50",
            )}
          >
            <span className="block font-medium">{c === "create" ? "Create a new phrase" : "Restore a phrase"}</span>
            <span className="block text-sm text-muted">
              {c === "create" ? "For a brand-new node" : "You already have 12 or 24 words"}
            </span>
          </button>
        ))}
      </div>

      {choice === "restore" && (
        <div className="space-y-3">
          <div className="space-y-1">
            <Label htmlFor="words">Recovery phrase</Label>
            <Textarea
              id="words"
              value={words}
              onChange={(e) => setWords(e.target.value)}
              placeholder="word1 word2 …"
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
            />
            <p className="text-xs text-muted">{wordCount} words</p>
          </div>
          {managed && (
            <div className="space-y-1">
              <Label htmlFor="birthday">Block height of your first transaction (optional)</Label>
              <Input
                id="birthday"
                inputMode="numeric"
                value={birthday}
                onChange={(e) => setBirthday(e.target.value.replace(/\D/g, ""))}
                placeholder="e.g. 850000"
              />
              <p className="text-xs text-muted">
                Lets the node find older on-chain funds. A pruned node can only rescan blocks it still has.
              </p>
            </div>
          )}
          <Alert tone="warn" title="Channel funds need a channel-state backup">
            The phrase restores on-chain funds. Lightning channel funds also need the latest channel-state backup.
          </Alert>
        </div>
      )}

      {error && <Alert tone="danger" title="Setup failed">{error}</Alert>}

      <div className="flex gap-3">
        <Button variant="secondary" onClick={onBack}>
          <ArrowLeft size={16} /> Back
        </Button>
        <Button className="flex-1" size="lg" disabled={busy || !restoreValid || !ldkValid} onClick={() => void submit()}>
          {busy ? <Spinner size={18} className="animate-spin" /> : null}
          {choice === "create" ? "Create my node" : "Restore my node"}
        </Button>
      </div>
      <p className="text-center text-xs text-muted">Your signer will ask you to approve one signature.</p>
    </div>
  );
}

function BackupStep({ phrase, onNext }: { phrase: string; onNext: () => void }) {
  const [visible, setVisible] = useState(false);
  const [saved, setSaved] = useState(false);
  const words = phrase.split(/\s+/);
  return (
    <div className="space-y-6">
      <Heading icon={<ShieldCheck size={22} weight="duotone" />} title="Write down your recovery phrase">
        Write these {words.length} words on paper, in order. Anyone who has them can take your funds.
      </Heading>
      <div className="relative">
        <ol className={cn("grid grid-cols-2 gap-2 sm:grid-cols-3", !visible && "select-none blur-md")} aria-hidden={!visible}>
          {words.map((w, i) => (
            <li key={i} className="flex items-center gap-2 rounded-lg border border-line bg-surface-2 px-3 py-2 font-mono text-sm">
              <span className="w-5 text-right text-xs text-muted">{i + 1}</span>
              {w}
            </li>
          ))}
        </ol>
        {!visible && (
          <div className="absolute inset-0 flex items-center justify-center">
            <Button variant="secondary" onClick={() => setVisible(true)}>
              <Eye size={16} /> Show words
            </Button>
          </div>
        )}
      </div>
      {visible && (
        <Button variant="ghost" size="sm" onClick={() => setVisible(false)}>
          <EyeSlash size={16} /> Hide
        </Button>
      )}
      <label className="flex items-start gap-3 text-sm">
        <input type="checkbox" className="mt-1 accent-[var(--v-accent)]" checked={saved} onChange={(e) => setSaved(e.target.checked)} />
        I wrote the words down and stored them somewhere safe.
      </label>
      <Button size="lg" className="w-full" disabled={!saved} onClick={onNext}>
        Continue <ArrowRight size={18} />
      </Button>
    </div>
  );
}

function ConfirmStep({ phrase, onBack, onNext }: { phrase: string; onBack: () => void; onNext: () => void }) {
  const words = useMemo(() => phrase.split(/\s+/), [phrase]);
  const positions = useMemo(() => pickPositions(words.length, 3), [words.length]);
  const [answers, setAnswers] = useState<Record<number, string>>({});
  const correct = positions.every((p) => (answers[p] ?? "").trim().toLowerCase() === words[p]);
  return (
    <div className="space-y-6">
      <Heading icon={<CheckCircle size={22} weight="duotone" />} title="Check your backup">
        Type the requested words from your paper copy.
      </Heading>
      <div className="space-y-3">
        {positions.map((p) => (
          <div key={p} className="space-y-1">
            <Label htmlFor={`word-${p}`}>Word #{p + 1}</Label>
            <Input
              id={`word-${p}`}
              autoComplete="off"
              spellCheck={false}
              className="font-mono"
              value={answers[p] ?? ""}
              onChange={(e) => setAnswers({ ...answers, [p]: e.target.value })}
            />
          </div>
        ))}
      </div>
      <div className="flex gap-3">
        <Button variant="secondary" onClick={onBack}>
          <ArrowLeft size={16} /> Show words again
        </Button>
        <Button className="flex-1" size="lg" disabled={!correct} onClick={onNext}>
          Confirm backup
        </Button>
      </div>
    </div>
  );
}

/**
 * Optional fast sync (AssumeUTXO). Only offered for the bundled Bitcoin node while it is
 * still syncing; otherwise the step skips itself. Full validation is the default.
 */
function SyncChoiceStep({ onNext }: { onNext: () => void }) {
  const fs = useSWR<FastSyncInfo>("/api/sync/fast", fetcher);
  const progress = useSWR<SetupProgress>("/api/setup/progress", fetcher);
  const [mode, setMode] = useState<"full" | "fast">("full");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const synced = progress.data?.bitcoind && progress.data.bitcoind.initial_block_download === false;
  // Optional step: skip it when fast sync is not offered, Bitcoin is already synced, or
  // the check itself fails.
  const unavailable = !!fs.error || (fs.data && (!fs.data.available || synced));

  useEffect(() => {
    if (unavailable) onNext();
  }, [unavailable, onNext]);
  if (!fs.data || unavailable) return <p className="text-sm text-muted">Checking the Bitcoin node…</p>;
  const sug = fs.data.suggestion;

  async function save() {
    setError(null);
    if (mode === "full") return onNext();
    setBusy(true);
    try {
      await api.post("/api/sync/fast", { url: url.trim() });
      onNext();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const option = (id: "full" | "fast", title: string, body: string) => (
    <button
      type="button"
      onClick={() => {
        setMode(id);
        if (id === "fast" && !url && sug) setUrl(sug.url);
      }}
      className={cn("w-full rounded-lg border px-4 py-3 text-left", mode === id ? "border-accent bg-accent-soft/50" : "border-line")}
    >
      <span className="block text-sm font-medium">{title}</span>
      <span className="block text-xs text-muted">{body}</span>
    </button>
  );

  return (
    <div className="space-y-6">
      <Heading icon={<Lightning size={22} weight="duotone" />} title="Initial sync">
        Your Bitcoin node has to catch up with the network before Lightning can start.
      </Heading>
      <div className="space-y-2">
        {option("full", "Full validation (default)", "Checks every block since 2009. Takes one to several days, depending on the machine.")}
        {option(
          "fast",
          "Fast sync from a snapshot",
          "Starts from a UTXO snapshot (AssumeUTXO): usable in about an hour after the download. Old blocks are still checked in the background.",
        )}
      </div>
      <div className="space-y-2 rounded-lg border border-line bg-surface-2/40 p-4 text-sm">
        <p className="flex items-center gap-2 font-medium">
          <ShieldCheck size={18} weight="duotone" className="text-ok" /> Why fast sync is safe
        </p>
        <ul className="list-disc space-y-1.5 pl-5 text-xs text-muted">
          <li>
            The snapshot&apos;s hash is written into Bitcoin Core itself, reviewed like the rest of its code. Your node rejects any file
            that does not match it exactly, so it does not matter who serves the download: a wrong or tampered file cannot be loaded.
          </li>
          <li>
            You already trust Bitcoin Core to check your bitcoin; the snapshot asks for no other trust. It is the same kind of shortcut
            as Bitcoin Core&apos;s default <span className="font-mono">assumevalid</span> setting.
          </li>
          <li>
            Every block after the snapshot is fully checked as usual. In the background your node also checks every block from 2009 up to
            the snapshot and confirms that it arrives at the same result. If it ever did not, it would stop and tell you.
          </li>
          <li>Your keys and your recovery phrase are not involved: the snapshot only lists which coins exist on the blockchain.</li>
        </ul>
        <p className="text-xs text-muted">
          The only thing the download reveals is your IP address to the server you fetch it from.
        </p>
      </div>
      {mode === "fast" && (
        <div className="space-y-3">
          <div className="space-y-1">
            <Label htmlFor="snap-url">Snapshot URL</Label>
            <Input id="snap-url" value={url} onChange={(e) => setUrl(e.target.value)} placeholder="https://…/utxo-935000.dat" spellCheck={false} />
          </div>
          <p className="text-xs text-muted">
            {sug && `The suggested file (height ${sug.height.toLocaleString()}) is from ${sug.source}; any other source works the same. `}
            The download is about 9–10 GB and is deleted once loaded.
          </p>
        </div>
      )}
      {error && <Alert tone="danger" title="Could not start fast sync">{error}</Alert>}
      <Button size="lg" className="w-full" disabled={busy || (mode === "fast" && !url.trim())} onClick={() => void save()}>
        {busy && <Spinner size={18} className="animate-spin" />} Continue <ArrowRight size={18} />
      </Button>
    </div>
  );
}

const LOAD_PHASE: Record<NonNullable<FastSyncStatus["phase"]>, string> = {
  starting: "starting",
  coins: "loading coins",
  writing: "writing to disk",
  checking: "checking the snapshot against Bitcoin Core (several minutes)",
  finishing: "finishing",
};

function fastSyncText(s: FastSyncStatus): { value: string; done: boolean; pct?: number | null; warn?: boolean } {
  const dl = s.total ? Math.min(100, ((s.bytes ?? 0) / s.total) * 100) : null;
  const gb = (n?: number) => ((n ?? 0) / 1e9).toFixed(1);
  switch (s.state) {
    case "downloading":
      return { value: `downloading ${dl != null ? `${Math.floor(dl)}% · ` : ""}${gb(s.bytes)}${s.total ? ` of ${gb(s.total)}` : ""} GB`, done: false, pct: dl };
    case "download_error":
      return { value: s.detail ?? "download interrupted, retrying", done: false, pct: dl, warn: true };
    case "waiting_for_headers":
      return { value: `waiting for block headers (${(s.headers ?? 0).toLocaleString()})`, done: false, pct: null };
    case "loading": {
      const at = s.base_height ? ` at ${s.base_height.toLocaleString()}` : "";
      if (s.phase === "coins" && s.pct != null) {
        const millions = s.coins ? ` · ${(s.coins / 1e6).toFixed(0)}M coins` : "";
        return { value: `loading snapshot${at}: ${Math.floor(s.pct)}%${millions}`, done: false, pct: s.pct };
      }
      return { value: `loading snapshot${at}${s.phase ? `: ${LOAD_PHASE[s.phase]}` : ""}`, done: false, pct: null };
    }
    case "load_interrupted":
      return { value: "loading was interrupted, retrying", done: false, pct: null, warn: true };
    case "loaded":
      return { value: `loaded${s.base_height ? ` at ${s.base_height.toLocaleString()}` : ""}`, done: true };
    case "failed":
      return { value: `failed: ${s.error ?? "unknown error"}`, done: false, warn: true };
    case "not_needed":
      return { value: "not needed", done: true };
    case "canceled":
      return { value: "canceled", done: true };
  }
}

function DoneStep({ onFinish }: { onFinish: () => void }) {
  const { data } = useSWR<SetupProgress>("/api/setup/progress", fetcher, { refreshInterval: 5000 });
  return (
    <div className="space-y-6">
      <Heading icon={<CheckCircle size={22} weight="duotone" />} title="Your node is set up">
        The Lightning node starts once Bitcoin has caught up. You can close this page; it keeps working.
      </Heading>
      <SyncStatus progress={data} />
      <Button size="lg" className="w-full" onClick={onFinish}>
        Go to Allod <ArrowRight size={18} />
      </Button>
    </div>
  );
}

type StatusRow = {
  label: string;
  value: string;
  done: boolean;
  /** 0–100 for a bar, null for a bar without a known percentage, undefined for none. */
  pct?: number | null;
  warn?: boolean;
  /** Time-left text under the bar. */
  eta?: string;
};

function ProgressBar({ pct, warn }: { pct: number | null; warn?: boolean }) {
  const color = warn ? "bg-warn" : "bg-accent";
  return (
    <div
      className="h-1.5 w-full overflow-hidden rounded-full bg-surface-2"
      role="progressbar"
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={pct == null ? undefined : Math.floor(pct)}
    >
      {pct == null ? (
        <div className={cn("h-full w-1/3 rounded-full motion-safe:animate-[allod-indeterminate_1.6s_ease-in-out_infinite]", color)} />
      ) : (
        <div className={cn("h-full rounded-full transition-[width] duration-700", color)} style={{ width: `${Math.max(1, pct)}%` }} />
      )}
    </div>
  );
}

export function SyncStatus({ progress }: { progress?: SetupProgress }) {
  // Samples for the time-left estimates are taken as the status arrives (every 5 s).
  useSWR<SetupProgress>("/api/setup/progress", fetcher, {
    refreshInterval: 5000,
    onSuccess: (p) => {
      const b = p.bitcoind;
      if (b?.blocks != null) recordProgress("btc-blocks", b.blocks, 600_000);
      if (b?.verification_progress != null) recordProgress("btc-vp", b.verification_progress, 600_000);
    },
  });
  const fast = useSWR<FastSyncInfo>(progress ? "/api/sync/fast" : null, fetcher, {
    refreshInterval: 5000,
    onSuccess: (f) => {
      const st = f.status;
      if (st?.state === "downloading" && st.bytes != null) recordProgress("fs-dl", st.bytes, 120_000);
      if (st?.state === "loading" && st.phase === "coins" && st.pct != null) recordProgress("fs-load", st.pct, 120_000);
    },
  });
  useProgressSamples();
  const fees = useFeeEstimates();
  if (!progress) return <p className="text-sm text-muted">Checking status…</p>;
  const btc = progress.bitcoind;
  const rows: StatusRow[] = [];
  // While a fast sync is under way, bitcoind's own numbers (blocks from genesis, or
  // nothing while paused) say nothing useful: only the snapshot's progress is shown.
  // Once loaded, the Bitcoin row is back, counting from the snapshot's block.
  const fs = fast.data?.status;
  const fsState = fs?.state;
  const fastPending =
    !!fast.data?.requested_url &&
    (!fsState ||
      fsState === "downloading" ||
      fsState === "download_error" ||
      fsState === "waiting_for_headers" ||
      fsState === "loading" ||
      fsState === "load_interrupted");
  const fromSnapshot = fsState === "loaded" ? fs?.base_height : undefined;
  if (fs && fast.data?.requested_url !== undefined && (fast.data.requested_url || fs.state !== "canceled")) {
    const eta =
      fs.state === "downloading" && fs.total
        ? formatEta(etaSeconds("fs-dl", fs.total))
        : fs.state === "loading" && fs.phase === "coins"
          ? formatEta(etaSeconds("fs-load", 100))
          : undefined;
    rows.push({ label: "Fast sync", ...fastSyncText(fs), eta });
  } else if (fast.data?.requested_url) {
    rows.push({ label: "Fast sync", value: "starting", done: false, pct: null });
  }
  if (btc && !btc.error && !fastPending) {
    const blocks = btc.blocks ?? 0;
    const headers = btc.headers ?? 0;
    // After a snapshot, count from its block; otherwise Bitcoin Core's own estimate.
    const pct =
      fromSnapshot && headers > fromSnapshot
        ? Math.min(100, (Math.max(0, blocks - fromSnapshot) / (headers - fromSnapshot)) * 100)
        : btc.verification_progress != null
          ? Math.min(100, btc.verification_progress * 100)
          : null;
    rows.push({
      label: "Bitcoin",
      value: btc.initial_block_download
        ? `syncing ${pct != null ? `${pct < 10 ? pct.toFixed(1) : Math.floor(pct)}% · ` : ""}${blocks.toLocaleString()} of ${headers.toLocaleString()}${fromSnapshot ? ` from snapshot at ${fromSnapshot.toLocaleString()}` : ""}`
        : `synced at ${blocks.toLocaleString()}`,
      done: !btc.initial_block_download,
      pct: btc.initial_block_download ? pct : undefined,
      // Catch-up after a snapshot: blocks per second. Full validation: Bitcoin Core's
      // transaction-weighted progress, which stays closer to linear than block counts.
      eta: btc.initial_block_download
        ? formatEta(fromSnapshot ? etaSeconds("btc-blocks", headers) : etaSeconds("btc-vp", 1))
        : undefined,
    });
  }
  // Once Bitcoin is synced on mainnet, ldk-server's last gate is the fee estimator
  // (see images/ldk-server/ldk-entrypoint.sh): say so instead of "starting".
  const fee = btc && !btc.error && btc.initial_block_download === false ? feeEstimateSummary(fees.data) : undefined;
  if (fee) {
    rows.push({ label: "Fee estimates", value: fee.value, done: fee.done, warn: fee.warn });
  }
  if (progress.mode === "managed") {
    rows.push({
      label: "Recovery phrase handed to the node",
      value: progress.seed_handoff === "waiting_for_ldk_server" ? "waiting for Bitcoin sync" : progress.seed_handoff === "done" ? "done" : "pending",
      done: progress.seed_handoff === "done",
    });
  }
  const lnHeight = progress.node?.block_height;
  const btcSynced = !!btc && !btc.error && btc.initial_block_download === false;
  const waitingForFees = btc?.chain === "main" && !!fee && !fee.done;
  rows.push({
    label: "Lightning node",
    value:
      lnHeight != null
        ? `running at ${lnHeight.toLocaleString()}`
        : waitingForFees
          ? "waiting for fee estimates"
          : progress.credentials.detail ?? (btcSynced ? "starting" : "waiting for Bitcoin"),
    done: lnHeight != null,
    pct: lnHeight == null && btcSynced ? null : undefined,
  });
  return (
    <ul className="divide-y divide-line rounded-lg border border-line">
      {rows.map((r) => (
        <li key={r.label} className="space-y-2 px-4 py-3 text-sm">
          <div className="flex items-center justify-between gap-4">
            <span className="flex items-center gap-2">
              {r.done ? (
                <CheckCircle size={18} weight="fill" className="text-ok" />
              ) : (
                <Spinner size={18} className={cn("motion-safe:animate-spin", r.warn ? "text-warn" : "text-muted")} />
              )}
              {r.label}
            </span>
            <span className={cn("text-right font-mono text-xs", r.warn ? "text-warn" : "text-muted")}>{r.value}</span>
          </div>
          {r.pct !== undefined && (
            <div className="space-y-1 pl-[26px]">
              <ProgressBar pct={r.pct} warn={r.warn} />
              {r.eta && <p className="text-right text-[11px] text-muted">{r.eta}</p>}
            </div>
          )}
        </li>
      ))}
    </ul>
  );
}
