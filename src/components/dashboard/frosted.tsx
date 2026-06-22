import type * as React from "react";
import { cn } from "@/lib/utils";

/** Space-Mono uppercase label used across Frosted/Brutal cards. */
export function FrostLabel({
  children,
  className,
}: {
  children: React.ReactNode;
  className?: string;
}) {
  return (
    <span
      className={cn(
        "font-spacemono text-[11px] uppercase tracking-[0.14em] text-white/60",
        className
      )}
    >
      {children}
    </span>
  );
}

/** Empty-state shell for a Frosted/Brutal card: a corner label + a centred,
 *  muted message. Keeps cards legible (and labelled) before data arrives. */
export function EmptyCard({ label, message }: { label: string; message: string }) {
  return (
    <div className="flex h-full w-full flex-col px-6 py-5">
      <FrostLabel>{label}</FrostLabel>
      <div className="flex flex-1 items-center justify-center font-body text-[13px] text-white/40">
        {message}
      </div>
    </div>
  );
}
