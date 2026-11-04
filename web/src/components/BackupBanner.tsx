import { Link } from "react-router";
import { useBackupVerified } from "../lib/backup";
import { Alert } from "./ui/alert";

/** Shown on every page until the recovery phrase backup is checked. */
export function BackupBanner() {
  if (useBackupVerified() !== false) return null;
  return (
    <Alert tone="warn" title="Back up your recovery phrase" className="mb-6">
      You have not checked your recovery phrase backup. If this machine fails, you lose your funds.{" "}
      <Link to="/backup" className="font-medium text-accent underline">
        Back it up now
      </Link>
    </Alert>
  );
}

/** Extra warning before receiving funds the phrase must be able to recover. */
export function UnverifiedBackupWarning({ what }: { what: string }) {
  if (useBackupVerified() !== false) return null;
  return (
    <Alert tone="danger" title="Your recovery phrase is not backed up">
      {what} Without a checked backup of your recovery phrase you can lose them for good.{" "}
      <Link to="/backup" className="font-medium text-accent underline">
        Back it up first
      </Link>
    </Alert>
  );
}
