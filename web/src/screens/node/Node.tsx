import { NavLink, Outlet } from "react-router";
import { cn } from "../../lib/utils";

const tabs = [
  { to: "/node", label: "Overview", end: true },
  { to: "/node/channels", label: "Channels" },
  { to: "/node/peers", label: "Peers" },
  { to: "/node/onchain", label: "On-chain" },
  { to: "/node/forwards", label: "Forwards" },
  { to: "/node/sign", label: "Sign message" },
];

export function NodeLayout() {
  return (
    <div className="space-y-6">
      <h1 className="text-3xl font-semibold">Node</h1>
      <nav className="flex flex-wrap gap-1 border-b border-line">
        {tabs.map((t) => (
          <NavLink
            key={t.to}
            to={t.to}
            end={t.end}
            className={({ isActive }) =>
              cn("-mb-px border-b-2 px-3 py-2 text-sm", isActive ? "border-accent text-fg" : "border-transparent text-muted hover:text-fg")
            }
          >
            {t.label}
          </NavLink>
        ))}
      </nav>
      <Outlet />
    </div>
  );
}
