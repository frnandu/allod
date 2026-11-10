import { X } from "@phosphor-icons/react";
import useSWR from "swr";
import { Link } from "react-router";
import { api, fetcher } from "../lib/api";
import { Alert } from "./ui/alert";

/** An owner alert (SPEC §8.9): something that needs attention and is not a payment. */
export interface OwnerAlert {
  id: number;
  key: string;
  kind: string;
  title: string;
  body: string;
  url: string | null;
  created_at: number;
}

const KEY = "/api/alerts";

/** The owner's alerts on every page (a gateway about to expire…), until dismissed. */
export function AlertsBanner() {
  const { data, mutate } = useSWR<{ alerts: OwnerAlert[] }>(KEY, fetcher, { refreshInterval: 5 * 60_000 });
  const alerts = data?.alerts ?? [];
  if (alerts.length === 0) return null;
  return (
    <div className="mb-6 space-y-2">
      {alerts.map((a) => (
        <Alert key={a.id} tone={a.key.endsWith(":expired") || a.key.endsWith(":renew_failed") ? "danger" : "warn"} title={a.title}>
          <span className="flex items-start justify-between gap-3">
            <span>
              {a.body}{" "}
              {a.url && (
                <Link to={a.url} className="font-medium text-accent underline">
                  Open
                </Link>
              )}
            </span>
            <button
              type="button"
              className="shrink-0 text-muted hover:text-fg"
              aria-label="Dismiss"
              title="Dismiss"
              onClick={async () => {
                try {
                  await mutate(await api.post<{ alerts: OwnerAlert[] }>(`/api/alerts/${a.id}/dismiss`), {
                    revalidate: false,
                  });
                } catch {
                  void mutate();
                }
              }}
            >
              <X size={16} />
            </button>
          </span>
        </Alert>
      ))}
    </div>
  );
}
