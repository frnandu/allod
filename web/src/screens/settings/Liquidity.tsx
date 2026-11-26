import { CheckCircle, CircleNotch, WarningCircle } from "@phosphor-icons/react";
import { useState, type ReactNode } from "react";
import useSWR, { type KeyedMutator } from "swr";
import { Alert } from "../../components/ui/alert";
import { Badge } from "../../components/ui/badge";
import { Button } from "../../components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "../../components/ui/card";
import { Field, Modal } from "../../components/ui/extras";
import { Input } from "../../components/ui/input";
import {
  fetcher,
  type HealthResponse,
  liquidity,
  LIQUIDITY_SETTINGS,
  type LiquidityRequest,
  type LiquiditySettings,
  type LiquiditySource,
} from "../../lib/api";
import { formatAgo } from "../../lib/eta";
import { useNow } from "../../lib/useNow";
import { cn } from "../../lib/utils";

/** How long a restart may take to begin before the page says something is off. */
const RESTART_STALL_SECS = 180;
/** How long the outcome of a change stays on the page. */
const OUTCOME_SECS = 10 * 60;

type Phase =
  | { kind: "idle" }
  | { kind: "waiting" }
  | { kind: "stalled" }
  | { kind: "starting" }
  | { kind: "done"; at: number }
  | { kind: "failed"; reason: string };

/** Where the Lightning node is with the last change: handed over (`saved_at`), restarted
 * (its `started_at` after that), up again (health) and running with it (`applied`). */
export function restartPhase(d: LiquiditySettings, ldkUp: boolean | undefined, now: number): Phase {
  const savedAt = d.saved_at;
  const a = d.active;
  if (!savedAt || !a || now - savedAt > OUTCOME_SECS) return { kind: "idle" };
  const waiting = (): Phase => ({ kind: now - savedAt > RESTART_STALL_SECS ? "stalled" : "waiting" });
  // An ldk-server image that does not date its start: all it says is whether it runs with
  // the saved LSPs, which is the sign it restarted.
  if (a.started_at == null) {
    if (!d.applied) return waiting();
    return ldkUp ? { kind: "done", at: savedAt } : { kind: "starting" };
  }
  if (a.started_at < savedAt) return waiting();
  if (a.request === "unreadable" || a.request === "ignored")
    return { kind: "failed", reason: a.request_error ?? "the Lightning node could not use the saved LSPs" };
  if (!d.applied) return { kind: "failed", reason: "the Lightning node started with other LSPs than the saved ones" };
  if (!ldkUp) return { kind: "starting" };
  return { kind: "done", at: a.started_at ?? savedAt };
}

const inProgress = (d?: LiquiditySettings) =>
  !!d?.saved_at && !!d.active && (d.active.started_at == null ? d.applied === false : d.active.started_at < d.saved_at);

function Step({ state, children }: { state: "done" | "busy" | "todo" | "failed"; children: ReactNode }) {
  return (
    <li className="flex items-start gap-2">
      {state === "done" ? (
        <CheckCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-ok" aria-label="done" />
      ) : state === "busy" ? (
        <CircleNotch size={16} className="mt-0.5 shrink-0 animate-spin text-accent" aria-label="in progress" />
      ) : state === "failed" ? (
        <WarningCircle size={16} weight="fill" className="mt-0.5 shrink-0 text-danger" aria-label="failed" />
      ) : (
        <span aria-hidden className="mt-1 size-3.5 shrink-0 rounded-full border border-line" />
      )}
      <span className={cn(state === "todo" && "text-muted")}>{children}</span>
    </li>
  );
}

/** The restart a change causes, step by step. */
function RestartProgress({ phase, names }: { phase: Phase; names: string }) {
  const now = useNow();
  if (phase.kind === "idle") return null;
  const restarted = phase.kind === "starting" || phase.kind === "done" || phase.kind === "failed";
  return (
    <div className="space-y-2 rounded-lg border border-line p-3 text-sm" role="status" aria-label="Restart">
      <ol className="space-y-1.5">
        <Step state="done">Saved and handed to the Lightning node</Step>
        <Step state={restarted ? "done" : phase.kind === "stalled" ? "failed" : "busy"}>
          {restarted ? "The Lightning node restarted" : "Waiting for the Lightning node to restart…"}
        </Step>
        <Step state={phase.kind === "done" ? "done" : phase.kind === "failed" ? "failed" : phase.kind === "starting" ? "busy" : "todo"}>
          {phase.kind === "done"
            ? `Running with ${names}`
            : phase.kind === "failed"
              ? "It did not take the new LSPs"
              : phase.kind === "starting"
                ? "Starting up with the new LSPs…"
                : "Running with the new LSPs"}
        </Step>
      </ol>
      {phase.kind === "starting" && (
        <p className="text-xs text-muted">This can take a minute, longer while it waits for Bitcoin Core&apos;s fee estimates.</p>
      )}
      {phase.kind === "done" && (
        <p className="text-xs text-ok">Done: the Lightning node restarted {formatAgo(Math.max(0, now - phase.at))} ago and asks these LSPs for just-in-time channels.</p>
      )}
      {phase.kind === "failed" && (
        <Alert tone="danger" title="The Lightning node kept its earlier LSPs">
          It restarted, but {phase.reason}. Check the Lightning node&apos;s log; saving again retries.
        </Alert>
      )}
      {phase.kind === "stalled" && (
        <Alert tone="warn" title="The Lightning node has not restarted">
          A change is normally picked up within seconds. If it stays like this, the Lightning node may be stopped, or its
          image may be older than this setting: update the stack.
        </Alert>
      )}
    </div>
  );
}

/** `pubkey@host:port` with a compressed pubkey (the server checks it again). */
const LSP_FORM = /^(02|03)[0-9a-fA-F]{64}@(\[[0-9a-fA-F:]+\]|[A-Za-z0-9.-]+):\d{1,5}$/;

type Custom = { name: string; lsp: string; has_token: boolean; token?: string };
type Draft = { enabled: boolean; lsps: string[]; trust_0conf: boolean; custom: Custom[] };

// An older Allod answers with one `lsp` (or without these fields at all).
const fromSettings = (d: LiquiditySettings & { lsp?: string | null }): Draft => ({
  enabled: !!d.enabled,
  lsps: d.lsps ?? (d.lsp ? [d.lsp] : []),
  trust_0conf: !!d.trust_0conf,
  custom: (d.sources ?? []).filter((s) => s.custom).map((s) => ({ name: s.name, lsp: s.lsp, has_token: s.has_token })),
});

const same = (a: Draft, b: Draft) =>
  a.enabled === b.enabled &&
  (!a.enabled || (a.trust_0conf === b.trust_0conf && [...a.lsps].sort().join() === [...b.lsps].sort().join())) &&
  a.custom.length === b.custom.length &&
  a.custom.every((c, i) => c.name === b.custom[i]?.name && c.lsp === b.custom[i]?.lsp && c.token === undefined);

/** "034066…53b0@64.23.159.177:9735". */
function shortLsp(lsp: string) {
  const [pubkey = "", address = ""] = lsp.split("@");
  return `${pubkey.slice(0, 6)}…${pubkey.slice(-4)}@${address}`;
}

/** Settings → Lightning → Liquidity: just-in-time channels from an LSPS2 LSP (SPEC §2.3). */
export function Liquidity() {
  const { data, error, mutate } = useSWR<LiquiditySettings>(LIQUIDITY_SETTINGS, fetcher, {
    // Follow the node's restart after a change.
    refreshInterval: (d) => (inProgress(d) || d?.applied === false ? 3000 : 0),
  });
  if (error) return <Alert tone="danger" title="Could not load the liquidity settings">{error.message}</Alert>;
  if (!data) return <p className="text-sm text-muted">Loading…</p>;
  if (!data.available)
    return (
      <Alert tone="info" title="Not available here">
        Allod does not manage this Lightning node&apos;s configuration: set its LSPS2 LSP in LDK Server&apos;s own settings.
      </Alert>
    );
  return <Body key={JSON.stringify(fromSettings(data))} data={data} mutate={mutate} />;
}

function Body({ data, mutate }: { data: LiquiditySettings; mutate: KeyedMutator<LiquiditySettings> }) {
  const saved = fromSettings(data);
  const [draft, setDraft] = useState<Draft>(saved);
  const [adding, setAdding] = useState({ lsp: "", token: "" });
  const [confirming, setConfirming] = useState(false);
  const [busy, setBusy] = useState(false);
  const [err, setErr] = useState<string | null>(null);
  const now = useNow();
  // While the node restarts: whether it answers again.
  const watching = !!data.saved_at && now - data.saved_at < OUTCOME_SECS;
  const { data: health } = useSWR<HealthResponse>(watching ? "/api/health" : null, fetcher, { refreshInterval: watching ? 3000 : 0 });
  const phase = restartPhase(data, health?.ldk_server?.ok, now);
  // No report from the node: its ldk-server image predates this setting and ignores it.
  const outdated = data.available && data.active == null;

  const builtin = (data.sources ?? []).filter((s) => !s.custom);
  const sources: (LiquiditySource | (Custom & { id: string; custom: true; suggested: false }))[] = [
    ...builtin,
    ...draft.custom.map((c) => ({ ...c, id: `custom:${c.lsp}`, custom: true as const, suggested: false as const })),
  ];
  const dirty = !same(draft, saved);
  const valid = !draft.enabled || draft.lsps.some((l) => sources.some((s) => s.lsp === l));
  const addLsp = adding.lsp.trim();
  const addOk = LSP_FORM.test(addLsp) && !sources.some((s) => s.lsp.toLowerCase() === addLsp.toLowerCase());
  const activeLsps = data.active?.lsps ?? [];
  // What the node's graph says about each LSP (a custom one added just now: nothing yet).
  const known = new Map((data.sources ?? []).map((s) => [s.lsp, s]));
  /** The alias it announced in the node's graph; else a name saved earlier (or the default
   * LSP's), else its short pubkey. */
  const nameOf = (s: { name: string; lsp: string; custom: boolean }) =>
    known.get(s.lsp)?.alias ?? (s.name || shortLsp(s.lsp).split("@")[0]!);
  const toggle = (lsp: string, on: boolean) =>
    setDraft((d) => ({ ...d, lsps: on ? [...d.lsps.filter((l) => l !== lsp), lsp] : d.lsps.filter((l) => l !== lsp) }));

  const add = () => {
    const c: Custom = { name: "", lsp: addLsp, has_token: false, token: adding.token.trim() || undefined };
    setDraft((d) => ({ ...d, custom: [...d.custom, c], lsps: d.enabled ? [...d.lsps, c.lsp] : d.lsps }));
    setAdding({ lsp: "", token: "" });
  };
  const remove = (lsp: string) =>
    setDraft((d) => ({ ...d, custom: d.custom.filter((c) => c.lsp !== lsp), lsps: d.lsps.filter((l) => l !== lsp) }));

  const save = async () => {
    setBusy(true);
    setErr(null);
    const body: LiquidityRequest = {
      enabled: draft.enabled,
      lsps: draft.enabled ? draft.lsps : [],
      trust_0conf: draft.trust_0conf,
      custom: draft.custom.map((c) => ({ name: c.name, lsp: c.lsp, ...(c.token !== undefined ? { token: c.token } : {}) })),
    };
    try {
      await mutate(await liquidity.set(body), { revalidate: false });
      setConfirming(false);
    } catch (e) {
      setErr(e instanceof Error ? e.message : String(e));
      setConfirming(false);
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="space-y-4">
      <Card>
        <CardHeader>
          <CardTitle>Just-in-time channels</CardTitle>
          <CardDescription>
            When a payment comes in and your node has no channel with room for it, an LSP (Lightning service provider)
            opens one to you as the payment arrives. Its opening fee comes out of that payment; you need no channel or
            on-chain funds up front.
          </CardDescription>
        </CardHeader>
        <CardContent className="space-y-4">
          <label className="flex items-start gap-3 rounded-lg border border-line p-3">
            <input
              type="checkbox"
              className="mt-1"
              checked={draft.enabled}
              onChange={(e) =>
                setDraft((d) => ({
                  ...d,
                  enabled: e.target.checked,
                  lsps: e.target.checked && d.lsps.length === 0 && sources[0] ? [sources[0].lsp] : d.lsps,
                }))
              }
            />
            <span>
              <span className="block text-sm font-medium">Let Receive ask LSPs for a channel</span>
              <span className="block text-xs text-muted">
                Off: Receive only uses the channels you have; without inbound room it cannot take Lightning payments.
              </span>
            </span>
          </label>

          <fieldset className={cn("space-y-2", !draft.enabled && "opacity-60")} disabled={!draft.enabled}>
            <legend className="pb-1 text-sm font-medium">LSPs</legend>
            <p className="text-xs text-muted">
              Each payment asks every LSP ticked here for an offer, and takes the cheapest that fits it. LSPs that do not
              answer are skipped.
            </p>
            {sources.length === 0 && <p className="text-sm text-muted">No LSP suggested on this network: add one below.</p>}
            <div className="space-y-2" role="group" aria-label="LSPs">
              {sources.map((s) => (
                <label
                  key={s.id}
                  className={cn(
                    "flex cursor-pointer items-center gap-3 rounded-lg border p-3",
                    draft.lsps.includes(s.lsp) ? "border-accent bg-accent-soft/40" : "border-line",
                  )}
                >
                  <input
                    type="checkbox"
                    checked={draft.lsps.includes(s.lsp)}
                    onChange={(e) => toggle(s.lsp, e.target.checked)}
                    aria-label={nameOf(s)}
                  />
                  <span className="min-w-0 flex-1">
                    <span className="flex flex-wrap items-center gap-2 text-sm font-medium">
                      {nameOf(s)}
                      {s.custom && (s.has_token || ("token" in s && s.token)) && <Badge tone="neutral">Token</Badge>}
                      {data.applied !== null && activeLsps.includes(s.lsp) && <Badge tone="accent">In use</Badge>}
                    </span>
                    <span className="flex flex-wrap gap-x-2 text-xs text-muted">
                      <span className="truncate font-mono" title={s.lsp}>
                        {shortLsp(s.lsp)}
                      </span>
                      {known.get(s.lsp)?.channels != null && (
                        <span>
                          {known.get(s.lsp)!.channels!.toLocaleString()} public channel{known.get(s.lsp)!.channels === 1 ? "" : "s"}
                        </span>
                      )}
                      {known.has(s.lsp) && known.get(s.lsp)?.connected !== undefined && (
                        <span className={known.get(s.lsp)?.connected ? "text-ok" : undefined}>
                          {known.get(s.lsp)?.connected ? "connected" : "not connected"}
                        </span>
                      )}
                    </span>
                  </span>
                  {s.custom && (
                    <Button size="sm" variant="ghost" onClick={(e) => { e.preventDefault(); remove(s.lsp); }}>
                      Remove
                    </Button>
                  )}
                </label>
              ))}
            </div>

            <details className="group rounded-lg border border-dashed border-line p-3">
              <summary className="cursor-pointer list-none text-sm font-medium text-accent [&::-webkit-details-marker]:hidden">
                Add another LSP
              </summary>
              <div className="mt-3 space-y-3">
                <div>
                  <Field label="Node" hint="pubkey@host:port, as the LSP publishes it for LSPS2.">
                    <Input
                      value={adding.lsp}
                      onChange={(e) => setAdding({ ...adding, lsp: e.target.value })}
                      placeholder="03…@lsp.example.com:9735"
                      spellCheck={false}
                      className="font-mono"
                    />
                  </Field>
                </div>
                <Field label="Token (optional)" hint="Only if the LSP gave you one. Kept in Allod's secret files, never shown again.">
                  <Input type="password" autoComplete="off" value={adding.token} onChange={(e) => setAdding({ ...adding, token: e.target.value })} />
                </Field>
                {addLsp !== "" && !LSP_FORM.test(addLsp) && <p className="text-xs text-warn">The node must look like pubkey@host:port.</p>}
                <Button size="sm" variant="secondary" disabled={!addOk} onClick={add}>
                  Add to the list
                </Button>
              </div>
            </details>
          </fieldset>

          <label className={cn("flex items-start gap-3 rounded-lg border border-line p-3", !draft.enabled && "opacity-60")}>
            <input
              type="checkbox"
              className="mt-1"
              disabled={!draft.enabled}
              checked={draft.trust_0conf}
              onChange={(e) => setDraft((d) => ({ ...d, trust_0conf: e.target.checked }))}
            />
            <span>
              <span className="block text-sm font-medium">Accept their channels before they confirm (0-conf)</span>
              <span className="block text-xs text-muted">
                The payment that opens the channel arrives at once instead of after a block (about 10 minutes, sometimes
                much longer). Until the channel confirms you trust the LSP not to double-spend its funding: only turn this
                on for LSPs you trust.
              </span>
            </span>
          </label>

          {err && <Alert tone="danger" title="Could not save">{err}</Alert>}

          {outdated && (
            <Alert tone="warn" title="Update the Lightning node first">
              This Lightning node (LDK Server) is older than this setting: it would ignore a change here and keep the
              installation&apos;s default LSP. Update the stack, then choose the LSPs.
            </Alert>
          )}

          <RestartProgress
            phase={phase}
            names={saved.enabled ? saved.lsps.map((l) => { const s = sources.find((x) => x.lsp === l); return s ? nameOf(s) : "an LSP"; }).join(", ") : "no LSP (just-in-time channels off)"}
          />

          <div className="flex flex-col gap-3 rounded-lg bg-surface-2 p-3 sm:flex-row sm:items-center sm:justify-between" role="status" aria-label="Result">
            <p className="min-w-0 text-xs text-muted">
              {dirty
                ? "Saving restarts the Lightning node."
                : phase.kind === "waiting" || phase.kind === "starting"
                  ? "The Lightning node is restarting to use this…"
                  : data.enabled
                    ? `In use: ${saved.lsps.map((l) => { const s = sources.find((x) => x.lsp === l); return s ? nameOf(s) : "an LSP"; }).join(", ")}${saved.trust_0conf ? ", 0-conf" : ""}.`
                    : "Off: no just-in-time channels."}
            </p>
            <div className="flex shrink-0 gap-2">
              {dirty && (
                <Button variant="ghost" onClick={() => setDraft(saved)} disabled={busy}>
                  Discard
                </Button>
              )}
              <Button onClick={() => setConfirming(true)} disabled={busy || !dirty || !valid || outdated}>
                Save
              </Button>
            </div>
          </div>
        </CardContent>
      </Card>

      <ul className="list-disc space-y-1 pl-5 text-xs text-muted">
        <li>The LSP sees the payments that open a channel through it, and sets its own opening fee.</li>
        <li>Channels an LSP already opened stay open when you change or turn off this setting.</li>
      </ul>

      <Modal open={confirming} onClose={() => !busy && setConfirming(false)} title="Restart the Lightning node?">
        <div className="space-y-4 text-sm">
          <p>
            The Lightning node reads this setting only when it starts, so saving restarts it. For about a minute it cannot
            send or receive; payments in progress continue after the restart, and your channels stay open.
          </p>
          <div className="flex justify-end gap-2">
            <Button variant="ghost" onClick={() => setConfirming(false)} disabled={busy}>
              Cancel
            </Button>
            <Button onClick={() => void save()} disabled={busy}>
              {busy ? "Saving…" : "Save and restart"}
            </Button>
          </div>
        </div>
      </Modal>
    </div>
  );
}
