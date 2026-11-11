import {
  ClockCounterClockwise,
  GearSix,
  HardDrives,
  PlugsConnected,
  SignOut,
  Stack,
  Wallet,
  Warning,
  type Icon,
} from "@phosphor-icons/react";
import { nip19 } from "nostr-tools";
import { useEffect } from "react";
import { Link, NavLink, Outlet } from "react-router";
import { logout } from "../lib/api";
import { useLiveEvents } from "../lib/live";
import { prismFailureLabel, usePrismFailures } from "../lib/prism";
import { syncPush } from "../lib/push";
import type { Session } from "../lib/session";
import { cn, shorten } from "../lib/utils";
import { AlertsBanner } from "./AlertsBanner";
import { BackupBanner } from "./BackupBanner";
import { StandbyBitcoindBanner } from "./StandbyBitcoindBanner";
import { Brand } from "./Brand";
import { Avatar, useProfile } from "./Person";

const nav: { to: string; label: string; icon: Icon }[] = [
  { to: "/", label: "Wallet", icon: Wallet },
  { to: "/activity", label: "Activity", icon: ClockCounterClockwise },
  { to: "/accounts", label: "Accounts", icon: Stack },
  { to: "/connections", label: "Connections", icon: PlugsConnected },
  { to: "/node", label: "Node", icon: HardDrives },
  { to: "/settings", label: "Settings", icon: GearSix },
];

function npub(hex: string): string {
  try {
    return nip19.npubEncode(hex);
  } catch {
    return hex;
  }
}

export function AppShell({ session }: { session: Session }) {
  useLiveEvents();
  const { failedCount } = usePrismFailures();
  // Keep this browser's push subscription current (keys can rotate; SPEC §8.9).
  useEffect(() => {
    syncPush().catch(() => {});
  }, []);
  // The owner's Nostr name and picture (from their public profile).
  const owner = useProfile(session.pubkey);
  return (
    <div className="v-grain min-h-screen pb-20 md:pb-0">
      <header className="sticky top-0 z-20 border-b border-line bg-surface/85 backdrop-blur">
        <div className="mx-auto flex h-16 max-w-6xl items-center gap-6 px-4">
          <Link to="/" aria-label="allod wallet" className="shrink-0">
            <Brand />
          </Link>
          <nav className="hidden items-center gap-1 md:flex">
            {nav.map(({ to, label, icon: I }) => (
              <NavLink
                key={to}
                to={to}
                end={to === "/"}
                className={({ isActive }) =>
                  cn(
                    "flex items-center gap-2 rounded-full px-3 py-1.5 text-sm transition-colors",
                    isActive ? "bg-accent-soft text-accent" : "text-muted hover:text-fg",
                  )
                }
              >
                <I size={18} weight="duotone" />
                {label}
                {to === "/settings" && failedCount > 0 && (
                  <span
                    className="inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-danger/15 px-1 text-[10px] font-semibold text-danger"
                    title={prismFailureLabel(failedCount)}
                    aria-label={prismFailureLabel(failedCount)}
                  >
                    <Warning size={10} weight="fill" />
                    {failedCount > 9 ? "9+" : failedCount}
                  </span>
                )}
              </NavLink>
            ))}
          </nav>
          <div className="ml-auto flex items-center gap-1">
            <div
              className="flex items-center gap-2 px-2 py-1 text-sm text-muted"
              title={`Signed in as ${owner?.name ? `${owner.name} (${npub(session.pubkey)})` : npub(session.pubkey)}`}
            >
              <Avatar pubkey={session.pubkey} picture={owner?.picture} size={26} />
              {owner?.name ? (
                <span className="hidden max-w-40 truncate text-sm font-medium text-fg lg:inline">{owner.name}</span>
              ) : (
                <span className="hidden font-mono text-xs lg:inline">{shorten(npub(session.pubkey), 10, 4)}</span>
              )}
            </div>
            <button
              type="button"
              onClick={() => void logout()}
              className="rounded-full p-2 text-muted hover:bg-surface-2 hover:text-fg"
              title="Sign out"
              aria-label="Sign out"
            >
              <SignOut size={18} />
            </button>
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-6xl px-4 py-8">
        <BackupBanner />
        <AlertsBanner />
        <StandbyBitcoindBanner />
        <Outlet />
      </main>

      <nav className="fixed inset-x-0 bottom-0 z-20 flex border-t border-line bg-surface/95 backdrop-blur md:hidden">
        {nav.map(({ to, label, icon: I }) => (
          <NavLink
            key={to}
            to={to}
            end={to === "/"}
            className={({ isActive }) =>
              cn("flex flex-1 flex-col items-center gap-0.5 py-2 text-[11px]", isActive ? "text-accent" : "text-muted")
            }
          >
            <span className="relative">
              <I size={22} weight="duotone" />
              {to === "/settings" && failedCount > 0 && (
                <span
                  className="absolute -right-2 -top-1 inline-flex h-4 min-w-4 items-center justify-center rounded-full bg-danger px-0.5 text-[9px] font-semibold text-white"
                  title={prismFailureLabel(failedCount)}
                  aria-label={prismFailureLabel(failedCount)}
                >
                  {failedCount > 9 ? "9+" : failedCount}
                </span>
              )}
            </span>
            {label}
          </NavLink>
        ))}
      </nav>
    </div>
  );
}
