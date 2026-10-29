import { DeviceMobile, Plugs, PuzzlePiece, Spinner } from "@phosphor-icons/react";
import { QRCodeSVG } from "qrcode.react";
import { useEffect, useRef, useState } from "react";
import { bunkerSigner, hasExtension, startNostrConnect, chooseExtension, type Signer } from "../lib/signer";
import { Alert } from "./ui/alert";
import { Button } from "./ui/button";
import { Input, Label } from "./ui/input";

/** Lets the owner pick a Nostr signer: extension (NIP-07) or remote signer (NIP-46). */
export function SignerConnect({ onConnected }: { onConnected: (signer: Signer, pubkey: string) => void }) {
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [bunker, setBunker] = useState("");
  const [pairing, setPairing] = useState<{ uri: string; cancel: () => void } | null>(null);
  const [authUrl, setAuthUrl] = useState<string | null>(null);
  const extension = hasExtension();
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      pairing?.cancel();
    };
  }, [pairing]);

  async function finish(signer: Signer) {
    const pubkey = await signer.getPublicKey();
    if (mounted.current) onConnected(signer, pubkey);
  }

  async function run(action: () => Promise<void>) {
    setError(null);
    setBusy(true);
    try {
      await action();
    } catch (e) {
      if (mounted.current) setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  function pairWithApp() {
    const p = startNostrConnect();
    setPairing(p);
    void run(async () => finish(await p.signer));
  }

  return (
    <div className="space-y-5">
      <div className="grid gap-3 sm:grid-cols-2">
        <button
          type="button"
          disabled={!extension || busy}
          onClick={() => void run(async () => finish(chooseExtension()))}
          className="flex items-start gap-3 rounded-[var(--radius-card)] border border-line bg-surface p-4 text-left transition hover:border-accent disabled:opacity-50"
        >
          <PuzzlePiece size={28} weight="duotone" className="shrink-0 text-accent" />
          <span>
            <span className="block font-medium">Browser extension</span>
            <span className="block text-sm text-muted">
              {extension ? "Alby, nos2x, Keys.band and other NIP-07 extensions" : "No NIP-07 extension detected"}
            </span>
          </span>
        </button>
        <button
          type="button"
          disabled={busy}
          onClick={pairWithApp}
          className="flex items-start gap-3 rounded-[var(--radius-card)] border border-line bg-surface p-4 text-left transition hover:border-accent disabled:opacity-50"
        >
          <DeviceMobile size={28} weight="duotone" className="shrink-0 text-accent" />
          <span>
            <span className="block font-medium">Signer app</span>
            <span className="block text-sm text-muted">Amber, nsec.app or any NIP-46 signer, via QR code</span>
          </span>
        </button>
      </div>

      {pairing && (
        <div className="flex flex-col items-center gap-3 rounded-[var(--radius-card)] border border-line bg-surface p-5 text-center">
          <QRCodeSVG value={pairing.uri} size={196} className="rounded-lg bg-white p-2" />
          <p className="text-sm text-muted">Scan with your signer app, or copy the link into it.</p>
          <div className="flex gap-2">
            <Button size="sm" variant="secondary" onClick={() => void navigator.clipboard?.writeText(pairing.uri)}>
              Copy link
            </Button>
            <a href={pairing.uri} className="inline-flex h-8 items-center rounded-full px-3 text-sm text-accent">
              Open signer
            </a>
          </div>
          <p className="flex items-center gap-2 text-xs text-muted">
            <Spinner size={14} className="animate-spin" /> Waiting for the signer to connect…
          </p>
        </div>
      )}

      <form
        className="space-y-2"
        onSubmit={(e) => {
          e.preventDefault();
          void run(async () => finish(await bunkerSigner(bunker, setAuthUrl)));
        }}
      >
        <Label htmlFor="bunker">Or paste a bunker:// link</Label>
        <div className="flex gap-2">
          <Input
            id="bunker"
            value={bunker}
            onChange={(e) => setBunker(e.target.value)}
            placeholder="bunker://… or name@domain"
            autoComplete="off"
            spellCheck={false}
          />
          <Button type="submit" variant="secondary" disabled={!bunker.trim() || busy}>
            <Plugs size={16} /> Connect
          </Button>
        </div>
      </form>

      {authUrl && (
        <Alert tone="warn" title="Your signer asks you to approve this connection">
          <a href={authUrl} target="_blank" rel="noreferrer" className="text-accent underline">
            Open the approval page
          </a>
        </Alert>
      )}
      {error && <Alert tone="danger" title="Could not connect to the signer">{error}</Alert>}
    </div>
  );
}
