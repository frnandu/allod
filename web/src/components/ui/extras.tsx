import { CaretRight, Check, Copy, X } from "@phosphor-icons/react";
import { QRCodeSVG } from "qrcode.react";
import { useEffect, useState, type ReactNode } from "react";
import { copyText } from "../../lib/clipboard";
import { cn } from "../../lib/utils";
import { Button } from "./button";

export function CopyButton({ value, label = "Copy" }: { value: string; label?: string }) {
  const [state, setState] = useState<"idle" | "done" | "failed">("idle");
  return (
    <Button
      size="sm"
      variant="secondary"
      onClick={async () => {
        setState((await copyText(value)) ? "done" : "failed");
        setTimeout(() => setState("idle"), 1500);
      }}
    >
      {state === "done" ? <Check size={16} /> : <Copy size={16} />}{" "}
      {state === "done" ? "Copied" : state === "failed" ? "Could not copy" : label}
    </Button>
  );
}

export function QrBlock({ value, caption }: { value: string; caption?: string }) {
  return (
    <div className="flex flex-col items-center gap-3">
      <QRCodeSVG value={value.toUpperCase().startsWith("LN") ? value.toUpperCase() : value} size={220} className="rounded-xl bg-white p-3" />
      {caption && <p className="text-xs text-muted">{caption}</p>}
      <p className="max-w-full break-all rounded-lg bg-surface-2 p-2 font-mono text-[11px] text-muted">{value}</p>
      <CopyButton value={value} />
    </div>
  );
}

export function Tabs<T extends string>({
  tabs,
  value,
  onChange,
}: {
  /** `muted`: shown greyed out, but still selectable (to explain why). */
  tabs: { id: T; label: string; muted?: boolean }[];
  value: T;
  onChange: (t: T) => void;
}) {
  return (
    <div className="inline-flex flex-wrap gap-1 rounded-full border border-line bg-surface p-1" role="tablist">
      {tabs.map((t) => (
        <button
          key={t.id}
          role="tab"
          aria-selected={value === t.id}
          onClick={() => onChange(t.id)}
          className={cn(
            "rounded-full px-4 py-1.5 text-sm transition",
            value === t.id
              ? t.muted
                ? "bg-surface-2 text-muted"
                : "bg-accent text-accent-fg"
              : t.muted
                ? "text-muted/50 hover:text-muted"
                : "text-muted hover:text-fg",
          )}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}

export function Modal({
  open,
  onClose,
  title,
  children,
  wide,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  /** A wider dialog, for lists of options. */
  wide?: boolean;
}) {
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);
  if (!open) return null;
  return (
    <div className="fixed inset-0 z-50 flex items-end justify-center bg-black/40 p-0 sm:items-center sm:p-4" onClick={onClose}>
      <div
        role="dialog"
        aria-label={title}
        className={cn(
          "max-h-[90vh] w-full overflow-y-auto rounded-t-2xl border border-line bg-surface p-6 sm:rounded-2xl",
          wide ? "max-w-3xl" : "max-w-lg",
        )}
        onClick={(e) => e.stopPropagation()}
      >
        <div className="mb-4 flex items-center justify-between">
          <h2 className="text-lg font-semibold">{title}</h2>
          <button onClick={onClose} aria-label="Close" className="rounded-full p-1 text-muted hover:bg-surface-2">
            <X size={18} />
          </button>
        </div>
        {children}
      </div>
    </div>
  );
}

export function Field({ label, hint, children }: { label: string; hint?: string; children: ReactNode }) {
  return (
    <label className="block space-y-1">
      <span className="text-sm font-medium">{label}</span>
      {children}
      {hint && <span className="block text-xs text-muted">{hint}</span>}
    </label>
  );
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className="rounded-[var(--radius-card)] border border-dashed border-line p-6 text-center text-sm text-muted">{children}</p>;
}

/** Extra detail, closed until asked for (native `<details>`: keyboard and screen readers for free). */
export function Disclosure({ label = "Details", children, className }: { label?: string; children: ReactNode; className?: string }) {
  return (
    <details className={cn("group text-sm", className)}>
      <summary className="inline-flex cursor-pointer list-none items-center gap-1 text-xs font-medium text-accent select-none [&::-webkit-details-marker]:hidden">
        <CaretRight size={12} weight="bold" className="transition group-open:rotate-90" />
        {label}
      </summary>
      <div className="mt-3 space-y-2">{children}</div>
    </details>
  );
}
