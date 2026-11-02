import { Eye } from "@phosphor-icons/react";
import { useState } from "react";
import { Alert } from "../../components/ui/alert";
import { QrBlock } from "../../components/ui/extras";
import type { Balances } from "../../lib/api";

/**
 * Fractional-reserve warning (SPEC §10): accounts hold more than the node can spend
 * over Lightning.
 */
export function CoverageWarning({ accountsMsat, balances }: { accountsMsat: number | null | undefined; balances: Balances | undefined }) {
  if (!balances || !accountsMsat || accountsMsat <= balances.lightning.spendable_msat) return null;
  return (
    <Alert tone="danger" title="Accounts are under-collateralised">
      Accounts hold more than this node can spend over Lightning right now. Some accounts cannot be paid out in full: open
      channels or add outbound liquidity.
    </Alert>
  );
}

/** A secret (connection link) shown once, blurred until the owner reveals it (SPEC §13.4). */
export function SecretLink({ value }: { value: string }) {
  const [shown, setShown] = useState(false);
  return (
    <div className="relative">
      <div className={shown ? "" : "pointer-events-none select-none blur-md"} aria-hidden={!shown}>
        <QrBlock value={value} caption="Scan or paste this link into a Nostr Wallet Connect wallet." />
      </div>
      {!shown && (
        <button
          type="button"
          onClick={() => setShown(true)}
          className="absolute inset-0 flex items-center justify-center gap-2 text-sm font-medium"
        >
          <Eye size={18} /> Show connection link
        </button>
      )}
    </div>
  );
}

export function SetupTips() {
  return (
    <ul className="list-disc space-y-1 pl-5 text-sm text-muted">
      <li>Open any wallet app that supports Nostr Wallet Connect (NWC) and choose to connect a wallet.</li>
      <li>Scan the QR code or paste the link. It works like a key: anyone with it can spend this account&rsquo;s balance.</li>
      <li>The link is shown only now. If it is lost, create a new one from the account page; the balance stays.</li>
    </ul>
  );
}
