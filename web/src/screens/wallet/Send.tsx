import { ArrowLeft, CheckCircle, PaperPlaneTilt, Spinner, WarningCircle, X } from "@phosphor-icons/react";
import { useState } from "react";
import { Link } from "react-router";
import { Amount } from "../../components/Amount";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { Field } from "../../components/ui/extras";
import { Input, Textarea } from "../../components/ui/input";
import { Avatar } from "../../components/Person";
import { api, type Decoded, type PayTarget, type Profile, type Tx } from "../../lib/api";
import { dateTime, parseSats, sats } from "../../lib/format";
import { ContactSuggestions, TargetChoice, useContactMatches } from "./ContactSearch";

export function Send() {
  const [payment, setPayment] = useState("");
  const [preview, setPreview] = useState<Decoded | null>(null);
  const [amount, setAmount] = useState("");
  const [note, setNote] = useState("");
  const [maxFee, setMaxFee] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [result, setResult] = useState<Tx | null>(null);
  const [recipient, setRecipient] = useState<Profile | null>(null);
  const [highlighted, setHighlighted] = useState(0);
  const { matches, loading: searching, discovering } = useContactMatches(recipient ? "" : payment);

  async function review(target = payment) {
    setError(null);
    setBusy(true);
    try {
      setPreview(await api.post<Decoded>("/api/payments/decode", { payment: target.trim() }));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  function chooseTarget(t: PayTarget) {
    setPayment(t.value);
    setPreview(null);
    setNote("");
    void review(t.value);
  }

  function pickContact(p: Profile) {
    setRecipient(p);
    const first = p.targets?.[0];
    if (first) chooseTarget(first);
  }

  async function pay() {
    setError(null);
    setBusy(true);
    try {
      const body: Record<string, unknown> = { payment: payment.trim() };
      const a = parseSats(amount);
      if (!preview?.amount_msat && a) body.amount_msat = a * 1000;
      if (note.trim()) body.payer_note = note.trim();
      const f = parseSats(maxFee);
      if (f) body.max_fee_msat = f * 1000;
      if (recipient && recipient.targets?.some((t) => t.value === payment.trim())) {
        body.recipient = { pubkey: recipient.pubkey, name: recipient.name };
      }
      setResult(await api.post<Tx>("/api/payments", body));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  if (result) {
    const ok = result.state === "SETTLED";
    return (
      <Card className="mx-auto max-w-lg">
        <CardContent className="space-y-5 p-8 text-center">
          {ok ? <CheckCircle size={56} weight="duotone" className="mx-auto text-ok" /> : result.state === "PENDING" ? <Spinner size={56} className="mx-auto animate-spin text-warn" /> : <WarningCircle size={56} weight="duotone" className="mx-auto text-danger" />}
          <h1 className="text-2xl font-semibold">{ok ? "Paid" : result.state === "PENDING" ? "Payment in progress" : "Payment failed"}</h1>
          <Amount msat={result.amount_msat} big />
          {ok && <p className="text-sm text-muted">Fee {Math.ceil(result.fee_msat / 1000)} sats · {dateTime(result.settled_at)}</p>}
          {result.state === "PENDING" && <p className="text-sm text-muted">It completes in the background; you will see it in Activity.</p>}
          {result.failure_reason && <Alert tone="danger" title="Reason">{result.failure_reason}</Alert>}
          {result.payer_proof && <p className="break-all font-mono text-[11px] text-muted">Payer proof: {result.payer_proof}</p>}
          <div className="flex justify-center gap-3">
            <Link to="/" className="inline-flex h-10 items-center rounded-full border border-line px-5 text-sm">Done</Link>
            <Button variant="secondary" onClick={() => { setResult(null); setPreview(null); setPayment(""); setAmount(""); setNote(""); setRecipient(null); }}>
              Send another
            </Button>
          </div>
        </CardContent>
      </Card>
    );
  }

  return (
    <div className="mx-auto max-w-lg space-y-5">
      <Link to="/" className="inline-flex items-center gap-1 text-sm text-muted"><ArrowLeft size={14} /> Home</Link>
      <h1 className="text-3xl font-semibold">Send</h1>
      <Card>
        <CardContent className="space-y-5 p-6">
          {recipient && (
            <div className="space-y-3 rounded-[var(--radius-card)] border border-line p-3">
              <div className="flex items-center gap-3">
                <Avatar pubkey={recipient.pubkey} picture={recipient.picture} />
                <div className="min-w-0 flex-1">
                  <p className="truncate font-medium">{recipient.name ?? "Contact"}</p>
                  <p className="truncate text-xs text-muted">{recipient.username ? `@${recipient.username}` : recipient.nip05 ?? recipient.lud16}</p>
                </div>
                <button type="button" className="rounded-lg p-1 text-muted hover:text-fg" title="Clear" onClick={() => { setRecipient(null); setPayment(""); setPreview(null); }}>
                  <X size={16} />
                </button>
              </div>
              <TargetChoice targets={recipient.targets ?? []} value={payment.trim()} onChange={(t) => chooseTarget(t)} />
            </div>
          )}
          <Field
            label={recipient ? "Paying to" : "Who or what to pay"}
            hint="Type a Nostr contact's name, or paste a BOLT11 invoice, BOLT12 offer (lno1…), name@domain, LNURL or bitcoin: link."
          >
            <Textarea
              value={payment}
              onChange={(e) => { setPayment(e.target.value); setPreview(null); setRecipient(null); setHighlighted(0); }}
              onKeyDown={(e) => {
                if (!matches.length) return;
                if (e.key === "ArrowDown") { e.preventDefault(); setHighlighted((h) => Math.min(h + 1, matches.length - 1)); }
                else if (e.key === "ArrowUp") { e.preventDefault(); setHighlighted((h) => Math.max(h - 1, 0)); }
                else if (e.key === "Enter") {
                  const choice = [matches[highlighted], ...matches].find((m) => m?.pay_target);
                  if (choice) { e.preventDefault(); pickContact(choice); }
                }
                else if (e.key === "Escape") { setPayment(""); }
              }}
              aria-autocomplete="list"
              placeholder="A contact's name, or lnbc… / lno1… / name@domain"
              className="font-mono text-xs"
              spellCheck={false}
              autoFocus
            />
            <ContactSuggestions matches={matches} loading={searching} discovering={discovering} highlighted={highlighted} onPick={pickContact} />
          </Field>

          {preview && (
            <div className="space-y-4 rounded-[var(--radius-card)] border border-line bg-surface-2/50 p-4">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <p className="text-xs uppercase tracking-wide text-muted">
                    {preview.kind === "bolt12" ? "BOLT12 offer" : preview.kind === "bolt11" ? "Lightning invoice" : preview.kind === "lnurl" ? `Lightning address${preview.payee ? ` · ${preview.payee}` : ""}` : "On-chain address"}
                    {preview.self_payment && " · internal"}
                  </p>
                  <p className="font-medium">{preview.description || "No description"}</p>
                </div>
                {preview.amount_msat ? <Amount msat={preview.amount_msat} className="text-right" /> : null}
              </div>
              {!preview.network_ok && <Alert tone="danger" title="Wrong network">This is for a different Bitcoin network than your node.</Alert>}
              {preview.is_expired && <Alert tone="danger" title="Expired">This invoice has expired.</Alert>}
              {preview.kind === "onchain" && <Alert tone="info" title="On-chain payment">Use Node → On-chain to send to a Bitcoin address.</Alert>}
              {!preview.amount_msat && preview.kind !== "onchain" && (
                <Field
                  label="Amount (sats)"
                  hint={preview.min_msat != null && preview.max_msat != null ? `Between ${sats(preview.min_msat)} and ${sats(preview.max_msat)} sats.` : undefined}
                >
                  <Input inputMode="numeric" value={amount} onChange={(e) => setAmount(e.target.value)} placeholder="e.g. 5,000" />
                </Field>
              )}
              {(preview.kind === "bolt12" || (preview.kind === "lnurl" && !!preview.comment_allowed)) && (
                <Field label="Note to the recipient (optional)">
                  <Input
                    value={note}
                    maxLength={preview.kind === "lnurl" ? Math.min(preview.comment_allowed ?? 0, 512) : 512}
                    onChange={(e) => setNote(e.target.value)}
                  />
                </Field>
              )}
              {preview.kind !== "onchain" && !preview.self_payment && (
                <Field label="Maximum routing fee (sats, optional)" hint="Default: 1% of the amount, at least 10 sats.">
                  <Input inputMode="numeric" value={maxFee} onChange={(e) => setMaxFee(e.target.value)} />
                </Field>
              )}
            </div>
          )}

          {error && <Alert tone="danger" title="Could not send">{error}</Alert>}

          {!preview ? (
            <Button size="lg" className="w-full" disabled={!payment.trim() || busy} onClick={() => void review()}>
              {busy && <Spinner size={18} className="animate-spin" />} Review
            </Button>
          ) : (
            <Button
              size="lg"
              className="w-full"
              disabled={busy || !preview.network_ok || preview.is_expired || preview.kind === "onchain" || (!preview.amount_msat && !parseSats(amount))}
              onClick={() => void pay()}
            >
              {busy ? <Spinner size={18} className="animate-spin" /> : <PaperPlaneTilt size={18} />} Pay
            </Button>
          )}
        </CardContent>
      </Card>
    </div>
  );
}
