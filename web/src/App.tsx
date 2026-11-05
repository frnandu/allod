import { Spinner } from "@phosphor-icons/react";
import { useEffect, useState } from "react";
import { createBrowserRouter, RouterProvider, type RouteObject } from "react-router";
import useSWR from "swr";
import { AppShell } from "./components/AppShell";
import { Alert } from "./components/ui/alert";
import { claimSetup, fetcher, hasSetupClaim, type InfoResponse } from "./lib/api";
import { useSession } from "./lib/session";
import { Backup } from "./screens/backup/Backup";
import { AccountDetail } from "./screens/accounts/AccountDetail";
import { Accounts } from "./screens/accounts/Accounts";
import { NewAccount } from "./screens/accounts/NewAccount";
import { ConnectionDetail } from "./screens/connections/ConnectionDetail";
import { Connections } from "./screens/connections/Connections";
import { NewConnection } from "./screens/connections/NewConnection";
import { Home } from "./screens/Home";
import { BitcoinSettings } from "./screens/settings/Bitcoin";
import { GeneralSettings } from "./screens/settings/General";
import { NetworkSettings } from "./screens/settings/Network";
import { NotificationSettings } from "./screens/settings/Notifications";
import { SettingsLayout } from "./screens/settings/SettingsLayout";
import { Channels } from "./screens/node/Channels";
import { Forwards } from "./screens/node/Forwards";
import { NodeLayout } from "./screens/node/Node";
import { Onchain } from "./screens/node/Onchain";
import { Overview } from "./screens/node/Overview";
import { Peers } from "./screens/node/Peers";
import { Sign } from "./screens/node/Sign";
import { Activity } from "./screens/wallet/Activity";
import { Receive } from "./screens/wallet/Receive";
import { Send } from "./screens/wallet/Send";
import { Placeholder } from "./screens/Placeholder";
import { SetupWizard } from "./screens/setup/SetupWizard";
import { SignIn } from "./screens/SignIn";

function SetupClaimGate({ info }: { info: InfoResponse }) {
  const [state, setState] = useState<"checking" | "ready" | "missing">("checking");
  const [error, setError] = useState("");

  useEffect(() => {
    let live = true;
    const raw = window.location.hash.startsWith("#setup=")
      ? window.location.hash.slice("#setup=".length)
      : "";
    if (raw) window.history.replaceState(null, "", `${window.location.pathname}${window.location.search}`);
    (async () => {
      try {
        if (raw) await claimSetup(raw);
        const valid = raw ? true : await hasSetupClaim();
        if (live) setState(valid ? "ready" : "missing");
      } catch (e) {
        if (live) {
          setError(e instanceof Error ? e.message : String(e));
          setState("missing");
        }
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  if (state === "checking") {
    return (
      <div className="flex min-h-screen items-center justify-center text-muted">
        <Spinner size={28} className="animate-spin" />
      </div>
    );
  }
  if (state === "missing") {
    const command = info.deployment.platform === "docker"
      ? `docker compose exec allod allod setup-link --base-url ${window.location.origin}`
      : `allod setup-link --base-url ${window.location.origin}`;
    return (
      <div className="mx-auto max-w-xl space-y-5 p-6 pt-20">
        <Alert tone="warn" title="Open your private setup link">
          Setup is locked to this machine. Run command below on host, then open link it prints.
        </Alert>
        <pre className="overflow-x-auto rounded-lg border border-line bg-surface-2 p-4 text-sm"><code>{command}</code></pre>
        {error && <p className="text-sm text-danger">{error}</p>}
      </div>
    );
  }
  return <SetupWizard info={info} />;
}

/** Setup first, then sign-in, then the app (SPEC §3.2–3.3). */
function Gate() {
  const info = useSWR<InfoResponse>("/api/info", fetcher, { revalidateOnFocus: false });
  const session = useSession();

  if (info.error) {
    return (
      <div className="mx-auto max-w-md p-10">
        <Alert tone="danger" title="Cannot reach Allod">
          {String(info.error.message ?? info.error)}
        </Alert>
      </div>
    );
  }
  if (!info.data) {
    return (
      <div className="flex min-h-screen items-center justify-center text-muted">
        <Spinner size={28} className="animate-spin" />
      </div>
    );
  }
  // Stay in the wizard until setup is marked complete, so the session it creates does not
  // skip the backup steps; the wizard reloads the page at the end.
  if (!info.data.setup_completed) return <SetupClaimGate info={info.data} />;
  if (!session) return <SignIn />;
  return <AppShell session={session} />;
}

export const routes: RouteObject[] = [
  {
    element: <Gate />,
    children: [
      { path: "/", element: <Home /> },
      { path: "/accounts", element: <Accounts /> },
      { path: "/accounts/new", element: <NewAccount /> },
      { path: "/accounts/:id", element: <AccountDetail /> },
      { path: "/connections", element: <Connections /> },
      { path: "/connections/new", element: <NewConnection /> },
      { path: "/connections/:id", element: <ConnectionDetail /> },
      { path: "/send", element: <Send /> },
      { path: "/receive", element: <Receive /> },
      { path: "/activity", element: <Activity /> },
      { path: "/backup", element: <Backup /> },
      {
        path: "/node",
        element: <NodeLayout />,
        children: [
          { index: true, element: <Overview /> },
          { path: "channels", element: <Channels /> },
          { path: "peers", element: <Peers /> },
          { path: "onchain", element: <Onchain /> },
          { path: "forwards", element: <Forwards /> },
          { path: "sign", element: <Sign /> },
        ],
      },
      {
        path: "/settings",
        element: <SettingsLayout />,
        children: [
          { index: true, element: <GeneralSettings /> },
          { path: "bitcoin", element: <BitcoinSettings /> },
          { path: "notifications", element: <NotificationSettings /> },
          { path: "network", element: <NetworkSettings /> },
        ],
      },
      { path: "*", element: <Placeholder title="Not found" milestone="no milestone" /> },
    ],
  },
];

export function App() {
  return <RouterProvider router={createBrowserRouter(routes)} />;
}
