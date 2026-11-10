import { Key, Spinner } from "@phosphor-icons/react";
import { useEffect, useRef, useState } from "react";
import { Brand } from "../components/Brand";
import { SignerConnect } from "../components/SignerConnect";
import { Alert } from "../components/ui/alert";
import { Button } from "../components/ui/button";
import { Card, CardContent } from "../components/ui/card";
import { ApiError, login } from "../lib/api";
import { forgetSigner, restoreSigner, type Signer } from "../lib/signer";

/** Signs in with the chosen signer as soon as it is known; access decides the rest. */
export function SignIn() {
  const [signer, setSigner] = useState<Signer | null>(() => restoreSigner());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tried = useRef<Signer | null>(null);

  async function signIn(s: Signer) {
    setError(null);
    setBusy(true);
    try {
      await login(s);
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

  useEffect(() => {
    if (!signer || tried.current === signer) return;
    tried.current = signer;
    void signIn(signer);
  }, [signer]);

  return (
    <div className="v-grain flex min-h-screen flex-col items-center justify-center px-4 py-10">
      <Brand size="lg" className="mb-10" />
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
              {error ? (
                <div className="flex flex-wrap gap-2">
                  <Button variant="secondary" onClick={() => void signIn(signer)} disabled={busy}>
                    Try again
                  </Button>
                  <Button
                    variant="ghost"
                    onClick={() => {
                      forgetSigner();
                      tried.current = null;
                      setSigner(null);
                      setError(null);
                    }}
                    disabled={busy}
                  >
                    Use another signer
                  </Button>
                </div>
              ) : (
                <p className="flex items-center justify-center gap-2 text-sm text-muted">
                  <Spinner size={18} className="animate-spin" /> Signing in…
                </p>
              )}
            </div>
          ) : (
            <SignerConnect
              onConnected={(s) => {
                setSigner(s);
              }}
            />
          )}
          {error && <Alert tone="danger" title="Sign-in failed">{error}</Alert>}
        </CardContent>
      </Card>
    </div>
  );
}
