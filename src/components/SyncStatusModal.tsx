import { RefreshCw, RotateCcw } from "lucide-react";
import {
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { Badge } from "@/components/ui/badge";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Separator } from "@/components/ui/separator";
import { useSyncStatus } from "@/queries/useSyncStatus";
import { api } from "@/lib/tauri";
import { cn } from "@/lib/utils";
import type { SyncQueueRow } from "@/types";
import { useState } from "react";

interface Props {
  open:    boolean;
  onClose: () => void;
}

function StatusBadge({ status }: { status: SyncQueueRow["status"] }) {
  return (
    <Badge
      variant="outline"
      className={cn(
        "text-[10px] tabular-nums",
        status === "success" && "border-emerald-700 text-emerald-400",
        status === "pending" && "border-amber-700 text-amber-400",
        status === "failed"  && "border-red-700 text-red-400"
      )}
    >
      {status}
    </Badge>
  );
}

function Counter({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div className="flex flex-col items-center gap-0.5">
      <span className={cn("text-xl font-bold tabular-nums", color)}>{value}</span>
      <span className="text-[10px] uppercase tracking-wide text-muted-foreground">{label}</span>
    </div>
  );
}

export function SyncStatusModal({ open, onClose }: Props) {
  const { data, isLoading, refetch } = useSyncStatus();
  const [retrying, setRetrying] = useState<string | null>(null);

  const handleRetry = async (event_id: string) => {
    setRetrying(event_id);
    try {
      await api.retryTradeTallyEvent(event_id);
      await refetch();
    } catch (e) {
      console.error("retry failed:", e);
    } finally {
      setRetrying(null);
    }
  };

  const handleRetryAll = async () => {
    setRetrying("all");
    try {
      await api.retryAllTradeTallyEvents();
      await refetch();
    } catch (e) {
      console.error("retry all failed:", e);
    } finally {
      setRetrying(null);
    }
  };

  const hasFailed = (data?.failed ?? 0) > 0;

  return (
    <Dialog open={open} onOpenChange={(o) => !o && onClose()}>
      <DialogContent className="max-w-3xl">
        <DialogHeader>
          <div className="flex items-center justify-between">
            <DialogTitle>TradeTally Sync</DialogTitle>
            <div className="flex items-center gap-2">
              {hasFailed && (
                <button
                  onClick={handleRetryAll}
                  disabled={retrying === "all"}
                  className="flex items-center gap-1.5 rounded border border-red-700/50 px-2 py-1 text-[11px] text-red-400 hover:bg-red-900/20 disabled:opacity-50"
                  title="Relancer tous les événements échoués"
                >
                  <RotateCcw className="h-3 w-3" />
                  Retry all failed
                </button>
              )}
              <button
                onClick={() => refetch()}
                className="rounded p-1 text-muted-foreground hover:bg-accent hover:text-foreground"
                title="Refresh"
              >
                <RefreshCw className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
        </DialogHeader>

        {isLoading && (
          <p className="py-4 text-center text-xs text-muted-foreground">Loading…</p>
        )}

        {data && (
          <>
            {/* Counters */}
            <div className="flex justify-around py-3">
              <Counter label="Pending" value={data.pending} color="text-amber-400" />
              <Counter label="Success" value={data.success} color="text-emerald-400" />
              <Counter label="Failed"  value={data.failed}  color="text-red-400" />
            </div>

            <Separator />

            <p className="mt-3 text-[10px] uppercase tracking-wider text-muted-foreground">
              Recent events (50)
            </p>
            <ScrollArea className="mt-1 h-80">
              {data.recent.length === 0 ? (
                <p className="py-6 text-center text-xs text-muted-foreground">
                  No events yet.
                </p>
              ) : (
                <table className="w-full text-xs">
                  <thead>
                    <tr className="border-b border-border text-left text-[10px] uppercase tracking-wider text-muted-foreground">
                      <th className="pb-1.5 pr-3">Time</th>
                      <th className="pb-1.5 pr-3">Symbol</th>
                      <th className="pb-1.5 pr-3">Type</th>
                      <th className="pb-1.5 pr-3">Endpoint</th>
                      <th className="pb-1.5 pr-3">Status</th>
                      <th className="pb-1.5 pr-3">Try</th>
                      <th className="pb-1.5">Action</th>
                    </tr>
                  </thead>
                  <tbody>
                    {data.recent.map((row) => (
                      <tr
                        key={row.event_id}
                        className="border-b border-border/40 hover:bg-accent/30"
                      >
                        <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">
                          {row.timestamp.slice(11, 19)}
                        </td>
                        <td className="py-1.5 pr-3 font-semibold">{row.symbol}</td>
                        <td className="py-1.5 pr-3 text-muted-foreground">{row.event_type}</td>
                        <td className="py-1.5 pr-3 font-mono text-[10px] text-muted-foreground truncate max-w-[10rem]" title={row.endpoint}>
                          {row.endpoint}
                        </td>
                        <td className="py-1.5 pr-3">
                          <StatusBadge status={row.status} />
                        </td>
                        <td className="py-1.5 pr-3 tabular-nums text-muted-foreground">
                          {row.attempts}
                        </td>
                        <td className="py-1.5">
                          {(row.status === "failed" || row.status === "pending") && (
                            <button
                              onClick={() => handleRetry(row.event_id)}
                              disabled={retrying === row.event_id}
                              className="rounded px-1.5 py-0.5 text-[10px] text-amber-400 hover:bg-amber-900/20 disabled:opacity-50"
                              title="Relancer"
                            >
                              <RotateCcw className="h-3 w-3" />
                            </button>
                          )}
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </ScrollArea>

            {/* Errors detail */}
            {data.recent.some((r) => r.error_message) && (
              <>
                <Separator className="mt-2" />
                <div className="mt-2 space-y-1">
                  <p className="text-[10px] uppercase tracking-wider text-muted-foreground">
                    Error details
                  </p>
                  {data.recent
                    .filter((r) => r.error_message)
                    .map((r) => (
                      <p key={r.event_id} className="text-[11px] text-red-400">
                        [{r.symbol} · {r.event_type}] {r.error_message}
                      </p>
                    ))}
                </div>
              </>
            )}
          </>
        )}
      </DialogContent>
    </Dialog>
  );
}
