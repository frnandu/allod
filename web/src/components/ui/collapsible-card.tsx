import { CaretDown } from "@phosphor-icons/react";
import { useId, useState, type MouseEvent, type ReactNode } from "react";
import { Link } from "react-router";
import { cn } from "../../lib/utils";
import { Card, CardContent, CardTitle } from "./card";

const STORAGE_PREFIX = "allod.card.";

/** Whether a card is open, remembered in this browser (a per-viewer convenience: storage
 * may be missing or blocked, and the card then starts closed). */
function useRememberedOpen(id: string): [boolean, (open: boolean) => void] {
  const key = STORAGE_PREFIX + id;
  const [open, setOpen] = useState(() => {
    try {
      return localStorage.getItem(key) === "open";
    } catch {
      return false;
    }
  });
  return [
    open,
    (next) => {
      setOpen(next);
      try {
        localStorage.setItem(key, next ? "open" : "closed");
      } catch {
        // Not remembered: fine.
      }
    },
  ];
}

const TONE = { ok: "text-ok", warn: "text-warn", danger: "text-danger" } as const;

/** A figure; `to` links it to where it is managed. */
export type BriefItem = { label: string; value: ReactNode; tone?: keyof typeof TONE; to?: string };
/** Something that needs attention; `to` links to where it is dealt with. */
export type BriefNote = { text: string; to?: string };

/** What a closed card shows: a few figures, and what needs attention, if anything. */
export function Brief({ items, attention = [] }: { items: BriefItem[]; attention?: BriefNote[] }) {
  return (
    <div className="space-y-1.5">
      <dl className="flex flex-wrap gap-x-5 gap-y-1 text-sm">
        {items.map((i) => {
          const body = (
            <>
              <dt className="text-xs text-muted group-hover:text-fg">{i.label}</dt>
              <dd className={cn("font-mono", i.tone && TONE[i.tone])}>{i.value}</dd>
            </>
          );
          return i.to ? (
            <Link key={i.label} to={i.to} className="group flex items-baseline gap-1.5">
              {body}
            </Link>
          ) : (
            <div key={i.label} className="flex items-baseline gap-1.5">
              {body}
            </div>
          );
        })}
      </dl>
      {attention.length > 0 && (
        <p className="text-xs text-warn">
          {attention.map((n, i) => (
            <span key={n.text}>
              {i > 0 && " · "}
              {n.to ? (
                <Link to={n.to} className="underline-offset-2 hover:underline">
                  {n.text}
                </Link>
              ) : (
                n.text
              )}
            </span>
          ))}
        </p>
      )}
    </div>
  );
}

/**
 * A card that folds to its title, badge and a brief summary (the subtitle shows open). Closed by default; each card's
 * state (by `id`) is remembered between reloads. The whole header toggles it, except the
 * links inside it.
 */
export function CollapsibleCard({
  id,
  title,
  subtitle,
  badge,
  brief,
  className,
  contentClassName = "space-y-4",
  children,
}: {
  id: string;
  title: ReactNode;
  subtitle?: ReactNode;
  badge?: ReactNode;
  brief?: ReactNode;
  className?: string;
  /** Spacing of the open body. */
  contentClassName?: string;
  children: ReactNode;
}) {
  const [open, setOpen] = useRememberedOpen(id);
  const bodyId = useId();
  const onHeader = (e: MouseEvent) => {
    // The button toggles on its own; links and copy buttons in the subtitle keep their job.
    if ((e.target as HTMLElement).closest("a, button")) return;
    setOpen(!open);
  };
  return (
    <Card className={className}>
      <div className="flex cursor-pointer items-center justify-between gap-3 p-5 pb-3 select-none" onClick={onHeader}>
        <div className="min-w-0">
          <CardTitle>
            <button
              type="button"
              aria-expanded={open}
              aria-controls={bodyId}
              onClick={() => setOpen(!open)}
              className="inline-flex items-center gap-1.5 rounded text-left focus-visible:outline-2 focus-visible:outline-accent"
            >
              {title}
              <CaretDown size={14} weight="bold" aria-hidden className={cn("text-muted transition", !open && "-rotate-90")} />
            </button>
          </CardTitle>
          {open && subtitle}
        </div>
        {badge}
      </div>
      <CardContent id={bodyId} className={open ? contentClassName : undefined}>
        {open ? children : brief}
      </CardContent>
    </Card>
  );
}
