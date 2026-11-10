import { CheckCircle, ShieldCheck } from "@phosphor-icons/react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { mutate } from "swr";
import { SignerConnect } from "../../components/SignerConnect";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { Input, Label } from "../../components/ui/input";
import { revealRecoveryPhrase } from "../../lib/api";
import { useBackupVerified } from "../../lib/backup";
import { useSession } from "../../lib/session";
import { localKeySigner, restoreSigner, storedLocalPubkey, type Signer } from "../../lib/signer";
import { BackupStep, ConfirmStep } from "../setup/SetupWizard";

/** Back up and check the recovery phrase after setup (SPEC §13.3). */
export function Backup() {
  const verified = useBackupVerified();
  const navigate = useNavigate();
  const session = useSession();
  const localMatches = !!session && storedLocalPubkey() === session.pubkey;
  const [signer, setSigner] = useState<Signer | null>(() => restoreSigner());
  const [password, setPassword] = useState("");
  const [phrase, setPhrase] = useState<string | null>(null);
  const [step, setStep] = useState<"show" | "confirm">("show");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function reveal() {
    setBusy(true);
    setError(null);
    try {
      // A key kept in this browser is already unlocked after sign-in; ask for its
      // encryption password again before using it to reveal the recovery phrase.
      const s = localMatches ? localKeySigner(password) : signer;
      if (!s) throw new Error("Connect your Nostr signer first");
      setPhrase(await revealRecoveryPhrase(s));
      setPassword("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  let body;
  if (phrase && step === "show") {
    body = <BackupStep phrase={phrase} onNext={() => setStep("confirm")} />;
  } else if (phrase) {
    body = (
      <ConfirmStep
        phrase={phrase}
        onBack={() => setStep("show")}
        onNext={() => {
          setPhrase(null);
          void mutate("/api/backup", { verified: true }, { revalidate: true });
          navigate("/");
        }}
      />
    );
  } else {
    body = (
      <div className="space-y-4">
        <div className="space-y-2">
          <div className="flex size-11 items-center justify-center rounded-full bg-accent-soft text-accent">
            <ShieldCheck size={22} weight="duotone" />
          </div>
          <h1 className="text-2xl font-semibold">Back up your recovery phrase</h1>
          <p className="text-muted">Sign with your Nostr key to show the words, then type 3 of them back.</p>
        </div>
        {verified && (
          <Alert tone="info" title="Backup checked">
            Your recovery phrase backup has been checked. Keep the paper copy safe.
          </Alert>
        )}
        {error && <Alert tone="danger" title="Could not show the phrase">{error}</Alert>}
        {localMatches ? (
          <form
            className="space-y-2"
            onSubmit={(e) => {
              e.preventDefault();
              void reveal();
            }}
          >
            <Label htmlFor="backup-password">Password for the key in this browser</Label>
            <p className="text-sm text-muted">
              You are signed in with the key kept in this browser. Enter the password that encrypts it to approve
              showing the recovery phrase.
            </p>
            <div className="flex gap-2">
              <Input
                id="backup-password"
                type="password"
                value={password}
                onChange={(e) => setPassword(e.target.value)}
                placeholder="Password"
                autoComplete="current-password"
              />
              <Button type="submit" disabled={busy || !password}>
                <CheckCircle size={18} /> Show words
              </Button>
            </div>
          </form>
        ) : signer ? (
          <Button size="lg" className="w-full" disabled={busy} onClick={() => void reveal()}>
            <CheckCircle size={18} /> Sign and show the words
          </Button>
        ) : (
          <SignerConnect onConnected={(s) => setSigner(s)} />
        )}
      </div>
    );
  }
  return (
    <Card className="mx-auto w-full max-w-xl">
      <CardContent className="space-y-6 p-6 sm:p-8">{body}</CardContent>
    </Card>
  );
}
