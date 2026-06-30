// Settings → Comptes & Système → Logs. Ported from the former docked LogsPanel — same
// query/content, now a live view inside Settings instead of a panel toggled from the
// LeftRail ⋮ menu (which no longer exists).

import { useQuery } from "@tanstack/react-query";
import { useAppStatus } from "@/queries/useAppStatus";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

function useLogs() {
  return useQuery({
    queryKey: ["local-logs"],
    queryFn: () => api.getLocalLogs(200),
    refetchInterval: 5_000,
  });
}

export function LogsTab() {
  const status = useAppStatus();
  const logs = useLogs();

  return (
    <div className="h-full overflow-auto rounded-md border border-border bg-card px-3 py-2 font-mono text-[11px] leading-relaxed">
      {/* App status line */}
      {status.data && (
        <div className="text-muted-foreground">
          [ok] backend={status.data.backend} v{status.data.version}{" "}
          latency={status.data.latency.websocket_to_ui_ms}ms
        </div>
      )}
      {/* SQLite logs */}
      {logs.data?.map((entry) => (
        <div
          key={entry.id}
          className={cn(
            "flex gap-2",
            entry.level === "error" && "text-red-400",
            entry.level === "warn" && "text-amber-400",
            entry.level === "info" && "text-muted-foreground"
          )}
        >
          <span className="shrink-0 text-muted-foreground/50">
            {entry.created_at.slice(11, 19)}
          </span>
          <span>[{entry.level}] {entry.message}</span>
        </div>
      ))}
    </div>
  );
}
