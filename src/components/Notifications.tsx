// Corner notice stack. Not a dialog: no overlay, no focus trap, nothing to
// acknowledge. It sits above the chart, out of the price action, and gets out of the
// way on its own.

import { AlertTriangle, Info, X } from "lucide-react";
import { cn } from "@/lib/utils";
import { useNotificationStore, type NoticeLevel } from "@/stores/notificationStore";

const STYLES: Record<NoticeLevel, string> = {
  error: "border-red-500/40 bg-red-950/85 text-red-100",
  warn: "border-amber-500/40 bg-amber-950/85 text-amber-100",
  info: "border-border bg-card/95 text-foreground",
};

export function Notifications() {
  const notices = useNotificationStore((s) => s.notices);
  const dismiss = useNotificationStore((s) => s.dismiss);

  if (notices.length === 0) return null;

  return (
    // `pointer-events-none` on the stack so the empty space never blocks a chart
    // click; each card re-enables them for its own dismiss button.
    <div className="pointer-events-none fixed bottom-4 right-4 z-[60] flex flex-col gap-1.5">
      {notices.map((n) => (
        <div
          key={n.id}
          role="status"
          className={cn(
            "pointer-events-auto flex max-w-sm items-start gap-2 rounded-md border px-3 py-2 shadow-lg backdrop-blur",
            STYLES[n.level],
          )}
        >
          {n.level === "info" ? (
            <Info className="mt-px h-3.5 w-3.5 shrink-0 opacity-80" />
          ) : (
            <AlertTriangle className="mt-px h-3.5 w-3.5 shrink-0 opacity-80" />
          )}
          <span className="text-[11px] leading-snug">{n.message}</span>
          <button
            onClick={() => dismiss(n.id)}
            className="ml-auto shrink-0 opacity-50 transition-opacity hover:opacity-100"
            aria-label="Fermer"
          >
            <X className="h-3 w-3" />
          </button>
        </div>
      ))}
    </div>
  );
}
