import { useSyncExternalStore } from "react";

export interface Session {
  token: string;
  expires_at: number;
  pubkey: string;
}

const KEY = "allod.session";
const listeners = new Set<() => void>();
let cached: Session | null | undefined;

function read(): Session | null {
  if (cached !== undefined) return cached;
  try {
    const raw = sessionStorage.getItem(KEY);
    const parsed = raw ? (JSON.parse(raw) as Session) : null;
    cached = parsed && parsed.expires_at * 1000 > Date.now() ? parsed : null;
  } catch {
    cached = null;
  }
  return cached;
}

export function getSession(): Session | null {
  const s = read();
  if (s && s.expires_at * 1000 <= Date.now()) {
    clearSession();
    return null;
  }
  return s;
}

export function setSession(s: Session) {
  cached = s;
  try {
    sessionStorage.setItem(KEY, JSON.stringify(s));
  } catch {
    /* kept in memory only */
  }
  listeners.forEach((l) => l());
}

export function clearSession() {
  cached = null;
  try {
    sessionStorage.removeItem(KEY);
  } catch {
    /* ignore */
  }
  listeners.forEach((l) => l());
}

export function useSession(): Session | null {
  return useSyncExternalStore(
    (l) => {
      listeners.add(l);
      return () => listeners.delete(l);
    },
    getSession,
    () => null,
  );
}
