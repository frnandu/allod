// The `superuser` scope (SPEC §9.1) grants the `create_connection` NWC method: the app
// can pair other apps with this wallet. It is deliberately not a regular permission
// checkbox but an Advanced option, always shown with the risk warning below.

import { Alert } from "../../components/ui/alert";

export const SUPERUSER_SCOPE = "superuser";

/** Checkbox for the Advanced section of connection forms. */
export function SuperuserToggle({ checked, onChange }: { checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="flex items-start gap-3 rounded-lg p-2 hover:bg-surface-2">
      <input type="checkbox" className="mt-1" checked={checked} onChange={(e) => onChange(e.target.checked)} />
      <span>
        <span className="block text-sm">Allow creating app connections</span>
        <span className="block text-xs text-muted">
          Grants the NWC create_connection method. Apps such as Alby Go can use it to connect other apps to this wallet.
        </span>
      </span>
    </label>
  );
}

/** Risks of the permission, shown wherever it is granted or in effect. */
export function SuperuserWarning() {
  return (
    <Alert tone="danger" title="Powerful connection permission">
      This app can create new connections with payment access. Child connections may spend funds available to this wallet.
      A parent budget does not currently cap their combined spending, so a compromised app could create many spending
      connections. Grant this only to an app you trust, and revoke it if you no longer need this access.
    </Alert>
  );
}
