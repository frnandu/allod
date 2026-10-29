import {
  ClockCounterClockwise,
  CaretDown,
  GearSix,
  HardDrives,
  House,
  PlugsConnected,
  SignOut,
  Stack,
  type Icon,
} from "@phosphor-icons/react";
import { nip19 } from "nostr-tools";
import { NavLink, Outlet } from "react-router";
import { logout } from "../lib/api";
import { useLiveEvents } from "../lib/live";
import type { Session } from "../lib/session";
import { cn, shorten } from "../lib/utils";
import { Brand } from "./Brand";
import { Avatar, useProfile } from "./Person";
import { Sigil } from "./Sigil";

const nav: { to: string; label: string; icon: Icon }[] = [
  { to: "/", label: "Home", icon: House },
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
  // The owner's Nostr name and picture (from their public profile).
  const owner = useProfile(session.pubkey);
  return (
    <div className="v-grain min-h-screen pb-20 md:pb-0">
      <header className="sticky top-0 z-20 border-b border-line bg-surface/85 backdrop-blur">
        <div className="mx-auto flex h-16 max-w-6xl items-center gap-6 px-4">
          <Brand />
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
              </NavLink>
            ))}
          </nav>
          <div className="ml-auto flex items-center gap-2">
            <button
              type="button"
              className="flex items-center gap-2 rounded-full border border-line px-2 py-1 text-sm"
              title="Account switcher (accounts arrive in M6)"
            >
              <Sigil seed="main" size={22} />
              <span className="font-medium">Main</span>
              <CaretDown size={14} className="text-muted" />
            </button>
            <button
              type="button"
              onClick={() => void logout()}
              className="flex items-center gap-2 rounded-full px-2 py-1 text-sm text-muted hover:text-fg"
              title={`Signed in as ${owner?.name ? `${owner.name} (${npub(session.pubkey)})` : npub(session.pubkey)}. Sign out`}
            >
              <Avatar pubkey={session.pubkey} picture={owner?.picture} size={26} />
              {owner?.name ? (
                <span className="hidden max-w-40 truncate text-sm font-medium text-fg lg:inline">{owner.name}</span>
              ) : (
                <span className="hidden font-mono text-xs lg:inline">{shorten(npub(session.pubkey), 10, 4)}</span>
              )}
              <SignOut size={18} />
            </button>
          </div>
        </div>
      </header>

      <main className="mx-auto max-w-6xl px-4 py-8">
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
            <I size={22} weight="duotone" />
            {label}
          </NavLink>
        ))}
      </nav>
    </div>
  );
}
