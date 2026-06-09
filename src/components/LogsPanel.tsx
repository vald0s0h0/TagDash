import { ScrollText, X } from "lucide-react";
import { useUiStore } from "@/stores/uiStore";
import { useAppStatus } from "@/queries/useAppStatus";
import { useQuery } from "@tanstack/react-query";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";

function useLogs() {
  return useQuery({
    queryKey: ["local-logs"],
    queryFn: () => api.getLocalLogs(50),
    refetchInterval: 5_000,
  });
}

export function LogsPanel() {
  const toggle = useUiStore((s) => s.toggleLogs);
  const status = useAppStatus();
  const logs = useLogs();

  return (
    <section className="flex h-48 flex-col border-t border-border bg-card">
      <header className="flex items-center justify-between border-b border-border px-3 py-1.5">
        <div className="flex items-center gap-2 text-xs uppercase tracking-wider text-muted-foreground">
          <ScrollText className="h-3.5 w-3.5" />
          <span>Logs</span>
        </div>
        <button
          onClick={toggle}
          className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
          title="Close"
        >
          <X className="h-3.5 w-3.5" />
        </button>
      </header>
      <div className="flex-1 overflow-auto px-3 py-2 font-mono text-[11px] leading-relaxed">
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
        {!logs.data?.length && (
          <div className="text-muted-foreground">[ok] ui shell ready</div>
        )}
      </div>
    </section>
  );
}
