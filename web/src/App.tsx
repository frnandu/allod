import { Spinner } from "@phosphor-icons/react";
import { createBrowserRouter, RouterProvider, type RouteObject } from "react-router";
import useSWR from "swr";
import { AppShell } from "./components/AppShell";
import { Alert } from "./components/ui/alert";
import { fetcher, type InfoResponse } from "./lib/api";
import { useSession } from "./lib/session";
import { ConnectionDetail } from "./screens/connections/ConnectionDetail";
import { Connections } from "./screens/connections/Connections";
import { NewConnection } from "./screens/connections/NewConnection";
import { Home } from "./screens/Home";
import { Settings } from "./screens/Settings";
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
  if (!info.data.setup_completed) return <SetupWizard info={info.data} />;
  if (!session) return <SignIn />;
  return <AppShell session={session} />;
}

export const routes: RouteObject[] = [
  {
    element: <Gate />,
    children: [
      { path: "/", element: <Home /> },
      { path: "/accounts", element: <Placeholder title="Accounts" milestone="M6" /> },
      { path: "/connections", element: <Connections /> },
      { path: "/connections/new", element: <NewConnection /> },
      { path: "/connections/:id", element: <ConnectionDetail /> },
      { path: "/send", element: <Send /> },
      { path: "/receive", element: <Receive /> },
      { path: "/activity", element: <Activity /> },
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
      { path: "/settings", element: <Settings /> },
      { path: "*", element: <Placeholder title="Not found" milestone="no milestone" /> },
    ],
  },
];

export function App() {
  return <RouterProvider router={createBrowserRouter(routes)} />;
}
