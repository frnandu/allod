import { useId } from "react";
import { cn } from "../lib/utils";

/** The Allod mark: a geometric "A" (a gable roof, land you own) with the Bitcoin node at its peak. Same art as `public/favicon.svg`. */
export function BrandMark({ className }: { className?: string }) {
  const gradient = useId();
  return (
    <svg viewBox="0 0 1024 1024" className={cn("size-7", className)} aria-hidden="true">
      <defs>
        <radialGradient id={gradient} cx="65%" cy="12%" r="105%">
          <stop offset="0" stopColor="#009F96" />
          <stop offset="0.55" stopColor="#007B76" />
          <stop offset="1" stopColor="#004C49" />
        </radialGradient>
      </defs>
      <rect width="1024" height="1024" rx="175" fill={`url(#${gradient})`} />
      <circle cx="512" cy="237" r="91" fill="#FF9C0A" />
      <path fill="#F7F6F0" fillRule="evenodd" d="M462 348 H562 L846 785 H706 L512 486 L318 785 H178 Z" />
      <path fill="#F7F6F0" d="M450 645 H574 L581 785 H443 Z" />
    </svg>
  );
}

/** Logo plus "allod" wordmark. `lg` is the hero size used on sign-in and setup. */
export function Brand({ className, size = "md" }: { className?: string; size?: "md" | "lg" }) {
  const lg = size === "lg";
  return (
    <span className={cn("flex items-center", lg ? "gap-4" : "gap-2.5", className)}>
      <BrandMark className={lg ? "size-16" : "size-9"} />
      <span className={cn("allod-wordmark", lg ? "text-6xl" : "text-3xl")}>allod</span>
    </span>
  );
}
