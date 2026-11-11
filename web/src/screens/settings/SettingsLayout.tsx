import { At, Bell, CurrencyBtc, Globe, IdentificationCard, Key, ShieldCheck, SlidersHorizontal, Warning, WifiHigh, type Icon } from "@phosphor-icons/react";
import { NavLink, Outlet } from "react-router";
import useSWR from "swr";
import { fetcher, type NetworkStatus } from "../../lib/api";
import { sats } from "../../lib/format";
import { prismFailureLabel, usePrismFailures } from "../../lib/prism";
import { cn } from "../../lib/utils";

/** Settings sections. New settings go into one of these, or a new entry here. */
export const SETTINGS_SECTIONS: {
  to: string;
  label: string;
  hint: string;
  icon: Icon;
  end?: boolean;
  /** Only on the appliance images (the host network helper answers). */
  appliance?: boolean;
}[] = [
  { to: "/settings", label: "General", hint: "Display currency", icon: SlidersHorizontal, end: true },
  { to: "/settings/backup", label: "Backup seed", hint: "Recovery phrase", icon: Key },
  { to: "/settings/bitcoin", label: "Bitcoin", hint: "Chain source", icon: CurrencyBtc },
  { to: "/settings/notifications", label: "Notifications", hint: "Payment alerts", icon: Bell },
  { to: "/settings/lightning-addresses", label: "Lightning addresses", hint: "Receive at name@domain", icon: At },
  { to: "/settings/domains", label: "Domains", hint: "Your domains and DNS", icon: Globe },
  { to: "/settings/nip05", label: "Nostr names (NIP-05)", hint: "Names on your domains", icon: IdentificationCard },
  { to: "/settings/gateways", label: "Gateways", hint: "Hide your home IP", icon: ShieldCheck },
  { to: "/settings/network", label: "Network", hint: "Wi-Fi", icon: WifiHigh, appliance: true },
];

export function SettingsLayout() {
  const { data: network } = useSWR<NetworkStatus>("/api/system/network", fetcher);
  const { failedCount, failedMsat } = usePrismFailures();
  const sections = SETTINGS_SECTIONS.filter((s) => !s.appliance || network?.available);
  return (
    <div className="space-y-6">
      <h1 className="text-3xl font-semibold">Settings</h1>
      <div className="grid gap-6 md:grid-cols-[13rem_minmax(0,1fr)]">
        <nav aria-label="Settings sections" className="flex gap-1 overflow-x-auto md:flex-col md:self-start">
          {sections.map(({ to, label, hint, icon: Icon, end }) => {
            const failed = to === "/settings/lightning-addresses" && failedCount > 0;
            return (
              <NavLink
                key={to}
                to={to}
                end={end}
                className={({ isActive }) =>
                  cn(
                    "flex shrink-0 items-center gap-3 rounded-lg px-3 py-2 text-sm",
                    isActive ? "bg-surface-2 font-medium text-fg" : "text-muted hover:bg-surface-2 hover:text-fg",
                  )
                }
              >
                <Icon size={18} weight="duotone" className="shrink-0" />
                <span className="flex flex-col">
                  <span className="flex items-center gap-1.5">
                    {label}
                    {failed && (
                      <span
                        className="inline-flex items-center gap-0.5 rounded-full bg-danger/15 px-1.5 py-0.5 text-[10px] font-semibold text-danger"
                        title={prismFailureLabel(failedCount)}
                        aria-label={prismFailureLabel(failedCount)}
                      >
                        <Warning size={10} weight="fill" />
                        {failedCount > 9 ? "9+" : failedCount}
                      </span>
                    )}
                  </span>
                  <span className={cn("hidden text-xs font-normal md:block", failed ? "text-danger" : "text-muted")}>
                    {failed ? `${sats(failedMsat)} sats failed to send` : hint}
                  </span>
                </span>
              </NavLink>
            );
          })}
        </nav>
        <div className="min-w-0 space-y-6">
          <Outlet />
        </div>
      </div>
    </div>
  );
}

/** One titled group of settings inside a section. */
export function SettingsGroup({ title, description, children }: { title: string; description?: string; children: React.ReactNode }) {
  return (
    <section className="space-y-3" aria-label={title}>
      <div>
        <h2 className="text-lg font-semibold">{title}</h2>
        {description && <p className="text-sm text-muted">{description}</p>}
      </div>
      {children}
    </section>
  );
}
