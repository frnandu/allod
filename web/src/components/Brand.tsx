import { cn } from "../lib/utils";

/** The Allod mark: an "A" drawn as a gable roof (land you own) with the Bitcoin node at its peak. */
export function BrandMark({ className }: { className?: string }) {
  return (
    <svg viewBox="0 0 32 32" className={cn("size-7", className)} aria-hidden="true">
      <rect width="32" height="32" rx="9" className="fill-accent" />
      <path d="M8 24 16 9.5 24 24M11.6 18.5h8.8" fill="none" stroke="currentColor" strokeWidth="3" strokeLinecap="round" strokeLinejoin="round" className="text-accent-fg" />
      <circle cx="16" cy="9.5" r="2.6" className="fill-btc" />
    </svg>
  );
}

export function Brand({ className }: { className?: string }) {
  return (
    <span className={cn("flex items-center gap-2", className)}>
      <BrandMark />
      <span className="font-display text-lg font-semibold tracking-tight">Allod</span>
    </span>
  );
}
