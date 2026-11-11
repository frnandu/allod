import { CheckCircle, Plugs } from "@phosphor-icons/react";
import { useMemo, useState } from "react";
import { Link, useNavigate, useSearchParams } from "react-router";
import { Alert } from "../../components/ui/alert";
import { Button } from "../../components/ui/button";
import { Card, CardContent } from "../../components/ui/card";
import { api, type AppConn } from "../../lib/api";
import { dateTime, sats } from "../../lib/format";
import {
  METHODS,
  NOTIFICATIONS,
  SPENDING_METHODS,
  parseAuthRequest,
  returnUrl,
  safeIcon,
  type AuthRequest,
} from "../../lib/nwcAuth";
import { NewConnection } from "./NewConnection";

interface Authorized {
  app: AppConn;
  wallet_pubkey: string;
  relay_url: string | null;
  state: string;
  lud16?: string;
}

/** `/connections/new`: the form, or the confirmation page when an app asked (NWC-08). */
export function NewConnectionRoute() {
  const [params] = useSearchParams();
  return params.has("pubkey") ? <AuthorizeConnection params={params} /> : <NewConnection />;
}

/** Tells the app's page the outcome: an event here (webview) and a message to the opener
 * (popup). The opener's origin is unknown, and the message holds no secret. */
function announce(type: "nwc:success" | "nwc:declined", detail: Record<string, unknown>) {
  window.dispatchEvent(new CustomEvent(type, { detail }));
  (window.opener as Window | null)?.postMessage({ type, ...detail }, "*");
}

function AuthorizeConnection({ params }: { params: URLSearchParams }) {
  const request = useMemo(() => parseAuthRequest(params), [params]);
  if (typeof request === "string") {
    return (
      <div className="mx-auto max-w-lg space-y-6">
        <h1 className="text-3xl font-semibold">Connection request</h1>
        <Alert tone="danger" title="This request cannot be approved">
          {request}
        </Alert>
        <Link to="/connections" className="text-sm text-accent">
          Go to connections
        </Link>
      </div>
    );
  }
  return <Confirm request={request} />;
}

function Confirm({ request }: { request: AuthRequest }) {
  const navigate = useNavigate();
  const [optional, setOptional] = useState<string[]>(() => request.optionalRequestMethods.filter((m) => m in METHODS));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [done, setDone] = useState<Authorized | null>(null);
  const [declined, setDeclined] = useState(false);

  const name = request.name ?? "An app";
  const icon = safeIcon(request.icon);
  const unknown = request.requestMethods.filter((m) => !(m in METHODS));
  const types = request.notificationTypes.filter((t) => t in NOTIFICATIONS);
  const spends = [...request.requestMethods, ...optional].some((m) => SPENDING_METHODS.includes(m));

  const approve = async () => {
    setBusy(true);
    setError(null);
    try {
      const r = await api.post<Authorized>("/api/apps/authorize", {
        pubkey: request.pubkey,
        state: request.state,
        name: request.name,
        expires_at: request.expiresAt,
        max_amount: request.maxAmount,
        renewal_period: request.renewalPeriod,
        request_methods: request.requestMethods,
        optional_request_methods: request.optionalRequestMethods,
        granted_optional_methods: optional,
        notification_types: types,
        isolated: request.isolated,
        metadata: request.metadata,
        relays: request.relays,
      });
      announce("nwc:success", {
        relayUrl: r.relay_url,
        walletPubkey: r.wallet_pubkey,
        state: r.state,
        ...(r.lud16 ? { lud16: r.lud16 } : {}),
      });
      // No popup to answer: go back to the app with the result.
      const back = window.opener
        ? null
        : returnUrl(request.returnTo, { relay_url: r.relay_url, wallet_pubkey: r.wallet_pubkey, state: r.state, lud16: r.lud16 });
      if (back) window.location.assign(back);
      setDone(r);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const decline = () => {
    announce("nwc:declined", { state: request.state });
    setDeclined(true);
    if (window.opener) window.close();
    else void navigate("/connections");
  };

  if (done) {
    return (
      <div className="mx-auto max-w-lg space-y-6">
        <h1 className="text-3xl font-semibold">“{done.app.name}” is connected</h1>
        <Alert tone="info" title="Go back to the app">
          {request.relays.length
            ? "Allod told the app over Nostr. It should show the wallet within a few seconds."
            : "The app has what it needs. You can close this window."}
        </Alert>
        <Link to={`/connections/${done.app.id}`} className="text-sm text-accent">
          Go to the connection
        </Link>
      </div>
    );
  }
  if (declined) {
    return (
      <div className="mx-auto max-w-lg space-y-6">
        <h1 className="text-3xl font-semibold">Request declined</h1>
        <p className="text-sm text-muted">Nothing was created. You can close this window.</p>
      </div>
    );
  }

  return (
    <div className="mx-auto max-w-lg space-y-6">
      <h1 className="text-3xl font-semibold">Connection request</h1>
      <Card>
        <CardContent className="space-y-5 pt-6 text-sm">
          <div className="flex items-center gap-3">
            {icon ? (
              <img src={icon} alt="" referrerPolicy="no-referrer" className="size-12 rounded-xl border border-line object-cover" />
            ) : (
              <span className="flex size-12 items-center justify-center rounded-xl border border-line text-muted">
                <Plugs size={24} />
              </span>
            )}
            <div className="min-w-0">
              <p className="truncate text-lg font-medium">{name}</p>
              <p className="text-xs text-muted">wants to connect to this wallet. Name and icon are the app's own claim.</p>
            </div>
          </div>

          <section className="space-y-2">
            <h2 className="font-medium">It asks to</h2>
            {request.requestMethods.length + request.optionalRequestMethods.length === 0 && (
              <p className="text-muted">Nothing beyond reading the connection's own details.</p>
            )}
            <ul className="space-y-1">
              {request.requestMethods.map((m) => (
                <li key={m} className="flex items-center gap-2">
                  <CheckCircle size={16} className={m in METHODS ? "text-accent" : "text-danger"} />
                  {METHODS[m] ?? m}
                </li>
              ))}
            </ul>
            {request.optionalRequestMethods.length > 0 && (
              <fieldset className="space-y-1 pt-1">
                <legend className="pb-1 text-xs text-muted">Optional: untick what it should not get</legend>
                {request.optionalRequestMethods.map((m) => (
                  <label key={m} className={`flex items-center gap-2 ${m in METHODS ? "" : "opacity-50"}`}>
                    <input
                      type="checkbox"
                      disabled={!(m in METHODS)}
                      checked={optional.includes(m)}
                      onChange={() => setOptional(optional.includes(m) ? optional.filter((x) => x !== m) : [...optional, m])}
                    />
                    {METHODS[m] ?? `${m} (not available)`}
                  </label>
                ))}
              </fieldset>
            )}
          </section>

          {types.length > 0 && (
            <section className="space-y-1">
              <h2 className="font-medium">It will be told about</h2>
              <p className="text-muted">{types.map((t) => NOTIFICATIONS[t]).join(", ")}</p>
            </section>
          )}

          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
            <dt className="text-muted">Spending limit</dt>
            <dd>
              {request.maxAmount == null
                ? "None"
                : `${sats(request.maxAmount)} sats${
                    request.renewalPeriod && request.renewalPeriod !== "never" ? `, renewed ${request.renewalPeriod}` : " in total"
                  }`}
            </dd>
            <dt className="text-muted">Expires</dt>
            <dd>{request.expiresAt ? dateTime(request.expiresAt) : "Never"}</dd>
            <dt className="text-muted">Balance</dt>
            <dd>{request.isolated ? "Its own, separate from yours" : "Your main balance"}</dd>
            {request.relays.length > 0 && (
              <>
                <dt className="text-muted">Answer goes to</dt>
                <dd className="break-all">{request.relays.join(", ")}</dd>
              </>
            )}
          </dl>

          {unknown.length > 0 && (
            <Alert tone="danger" title="Allod cannot give what this app needs">
              It requires {unknown.join(", ")}, which Allod does not offer to apps. The request can only be declined.
            </Alert>
          )}
          {spends && request.maxAmount == null && unknown.length === 0 && (
            <Alert tone="warn" title="No spending limit">
              This app asks to pay without a limit: it could spend everything the connection can reach.
            </Alert>
          )}
          {error && (
            <Alert tone="danger" title="Could not create the connection">
              {error}
            </Alert>
          )}
          <div className="flex gap-3">
            <Button variant="secondary" className="flex-1" disabled={busy} onClick={decline}>
              Decline
            </Button>
            <Button className="flex-1" disabled={busy || unknown.length > 0} onClick={() => void approve()}>
              {busy ? "Connecting…" : "Approve"}
            </Button>
          </div>
        </CardContent>
      </Card>
    </div>
  );
}
