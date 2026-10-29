import { ArrowRight, Key, Spinner } from "@phosphor-icons/react";
import { nip19 } from "nostr-tools";
import { useEffect, useState } from "react";
import { Brand } from "../components/Brand";
import { SignerConnect } from "../components/SignerConnect";
import { Sigil } from "../components/Sigil";
import { Alert } from "../components/ui/alert";
import { Button } from "../components/ui/button";
import { Card, CardContent } from "../components/ui/card";
import { ApiError, login } from "../lib/api";
import { forgetSigner, restoreSigner, type Signer } from "../lib/signer";
import { shorten } from "../lib/utils";

export function SignIn() {
  const [signer, setSigner] = useState<Signer | null>(() => restoreSigner());
  const [pubkey, setPubkey] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    if (signer && !pubkey) {
      signer.getPublicKey().then((pk) => alive && setPubkey(pk)).catch(() => alive && setSigner(null));
    }
    return () => {
      alive = false;
    };
  }, [signer, pubkey]);

  async function signIn() {
    if (!signer) return;
    setError(null);
    setBusy(true);
    try {
      await login(signer);
    } catch (e) {
      setError(
        e instanceof ApiError && e.status === 403
          ? "This Nostr key is not an owner of this node."
          : e instanceof Error
            ? e.message
            : String(e),
      );
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="v-grain flex min-h-screen flex-col items-center justify-center px-4 py-10">
      <Brand className="mb-8" />
      <Card className="w-full max-w-lg">
        <CardContent className="space-y-6 p-6 sm:p-8">
          <div className="space-y-2">
            <div className="flex size-11 items-center justify-center rounded-full bg-accent-soft text-accent">
              <Key size={22} weight="duotone" />
            </div>
            <h1 className="text-2xl font-semibold">Sign in</h1>
            <p className="text-muted">Use the Nostr key that owns this node.</p>
          </div>

          {signer ? (
            <div className="space-y-4">
              <div className="flex items-center gap-3 rounded-[var(--radius-card)] border border-line p-3">
                <Sigil seed={pubkey ?? "?"} size={36} />
                <div className="min-w-0">
                  <p className="text-sm font-medium">{signer.label}</p>
                  <p className="truncate font-mono text-xs text-muted">
                    {pubkey ? shorten(nip19.npubEncode(pubkey), 14, 8) : "connecting…"}
                  </p>
                </div>
                <Button
                  variant="ghost"
                  size="sm"
                  className="ml-auto"
                  onClick={() => {
                    forgetSigner();
                    setSigner(null);
                    setPubkey(null);
                  }}
                >
                  Change
                </Button>
              </div>
              <Button size="lg" className="w-full" disabled={busy || !pubkey} onClick={() => void signIn()}>
                {busy ? <Spinner size={18} className="animate-spin" /> : null}
                Sign in <ArrowRight size={18} />
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
          {error && <Alert tone="danger" title="Sign-in failed">{error}</Alert>}
        </CardContent>
      </Card>
    </div>
  );
}
