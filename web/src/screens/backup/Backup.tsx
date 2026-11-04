import { CheckCircle, ShieldCheck } from "@phosphor-icons/react";
import { useState } from "react";
import { useNavigate } from "react-router";
import { mutate } from "swr";
import { SignerConnect } from "../../components/SignerConnect";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { revealRecoveryPhrase } from "../../lib/api";
import { useBackupVerified } from "../../lib/backup";
import { restoreSigner, type Signer } from "../../lib/signer";
import { BackupStep, ConfirmStep } from "../setup/SetupWizard";

/** Back up and check the recovery phrase after setup (SPEC §13.3). */
export function Backup() {
  const verified = useBackupVerified();
  const navigate = useNavigate();
  const [signer, setSigner] = useState<Signer | null>(() => restoreSigner());
  const [phrase, setPhrase] = useState<string | null>(null);
  const [step, setStep] = useState<"show" | "confirm">("show");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  let body;
  if (verified && !phrase) {
    body = (
      <Alert tone="info" title="Backup checked">
        Your recovery phrase backup has been checked. Keep the paper copy safe.
      </Alert>
    );
  } else if (phrase && step === "show") {
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
        {error && <Alert tone="danger" title="Could not show the phrase">{error}</Alert>}
        {signer ? (
          <Button
            size="lg"
            className="w-full"
            disabled={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                setPhrase(await revealRecoveryPhrase(signer));
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              } finally {
                setBusy(false);
              }
            }}
          >
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
