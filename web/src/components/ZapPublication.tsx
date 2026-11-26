import { Lightning, Spinner } from "@phosphor-icons/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { publishBolt12Zap, type ZapReceipt } from "../lib/zap";
import { Button } from "./ui/button";

const MESSAGES: Record<ZapReceipt["status"], string> = {
  published: "Zap published.",
  waiting: "The zap can be published once the payment completes (Activity → this payment).",
  no_proof: "Your node gave no payment proof, so the zap cannot be published yet.",
  not_accepted: "No relay accepted the zap yet.",
  sign: "The zap still needs your signature.",
  none: "This payment is not a zap.",
};

/** Publishing the zap of a BOLT12 payment; `auto` starts it right away. */
export function ZapPublication({ transaction, auto = false }: { transaction: string; auto?: boolean }) {
  const [busy, setBusy] = useState(false);
  const [receipt, setReceipt] = useState<ZapReceipt | null>(null);
  const [error, setError] = useState<string | null>(null);
  const started = useRef(false);

  const publish = useCallback(async () => {
    setBusy(true);
    setError(null);
    try {
      setReceipt(await publishBolt12Zap(transaction));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }, [transaction]);

  useEffect(() => {
    if (!auto || started.current) return;
    started.current = true;
    void publish();
  }, [auto, publish]);

  return (
    <div className="space-y-2 text-sm text-muted" role="status">
      {receipt && <p>{MESSAGES[receipt.status]}</p>}
      {error && <p className="text-danger">{error}</p>}
      {receipt?.status !== "published" && (
        <Button variant="secondary" disabled={busy} onClick={() => void publish()}>
          {busy ? <Spinner size={16} className="animate-spin" /> : <Lightning size={16} />} Publish zap
        </Button>
      )}
    </div>
  );
}
