import { Info, Warning, WarningOctagon } from "@phosphor-icons/react";
import type { ReactNode } from "react";
import { cn } from "../../lib/utils";

const tones = {
  info: { box: "border-accent/30 bg-accent-soft/60", icon: Info },
  warn: { box: "border-warn/40 bg-warn/10", icon: Warning },
  danger: { box: "border-danger/40 bg-danger/10", icon: WarningOctagon },
};

export function Alert({
  tone = "info",
  title,
  children,
  className,
}: {
  tone?: keyof typeof tones;
  title?: string;
  children?: ReactNode;
  className?: string;
}) {
  const { box, icon: Icon } = tones[tone];
  return (
    <div role="alert" className={cn("flex gap-3 rounded-lg border p-3 text-sm", box, className)}>
      <Icon size={18} weight="duotone" className="mt-0.5 shrink-0" />
      <div className="space-y-1">
        {title && <p className="font-medium">{title}</p>}
        {children && <div className="text-muted">{children}</div>}
      </div>
    </div>
  );
}
